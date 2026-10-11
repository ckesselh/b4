//! Mapping of DuckDB result values onto the output model.
//!
//! Every value is carried over exactly or as exact text; nothing passes
//! through a float that was not a float in the database:
//!
//! | DuckDB type | [`Kind`] | [`Cell`] |
//! |---|---|---|
//! | `BOOLEAN` | boolean | boolean |
//! | `TINYINT` to `BIGINT`, unsigned up to `UINTEGER` | integer | integer |
//! | `UBIGINT`, `HUGEINT`, `UHUGEINT` | integer | integer when it fits 64 bits, a decimal of scale 0 up to 96 bits, the digits as text beyond |
//! | `FLOAT`, `DOUBLE` | float | float |
//! | `DECIMAL(w, s)` | decimal of scale `s` | decimal; the exact digits as text when it needs more than 96 bits |
//! | `DATE` | date | date (`infinity` as text) |
//! | `TIMESTAMP`, `TIMESTAMP_S`, `_MS`, `_NS` | timestamp | timestamp; text with nine decimals when there are nanoseconds |
//! | `TIMESTAMP WITH TIME ZONE` | text | the UTC time with a trailing `Z` |
//! | `TIME`, `TIME WITH TIME ZONE` | text | `HH:MM:SS[.ffffff]` (the offset of a `TIMETZ` is not available) |
//! | `INTERVAL` | text | such as `1 year 2 months 3 days 04:05:06` |
//! | `VARCHAR`, `UUID`, `ENUM` | text | text |
//! | `BLOB`, `GEOMETRY` | text | lower-case hex |
//! | `BIT` | text | the bits as `0` and `1` |
//! | `LIST`, `ARRAY`, `STRUCT`, `MAP`, `UNION` | text | JSON text, see [`json_text`] |
//! | `NULL` of any type | | null |
//!
//! Two values cannot be carried over as they are, and this is what happens
//! to them:
//!
//! * A `VARCHAR` that is not valid UTF-8 (DuckDB checks text on the way in,
//!   so this takes a damaged or foreign database) is decoded lossily: every
//!   invalid sequence becomes U+FFFD, and the cell is otherwise complete.
//!   Nothing marks the cell as changed.
//! * An `ENUM` value whose label cannot be looked up in its dictionary
//!   becomes null, like a missing value.

use duckdb::core::LogicalTypeId;
use duckdb::types::{TimeUnit, Value, ValueRef};
use rust_decimal::Decimal;

use crate::output::{Cell, Date, Kind, Timestamp, DOUBLE_DIGITS, MAX_SAFE_INTEGER};

const MICROS_PER_SECOND: i64 = 1_000_000;
const NANOS_PER_MICRO: i64 = 1_000;

/// The column kind for a result column of the DuckDB type `id`. `scale` is
/// the scale of a `DECIMAL` column.
pub(super) fn kind_of(id: Option<LogicalTypeId>, scale: u32) -> Kind {
    use LogicalTypeId as T;
    match id {
        Some(T::Boolean) => Kind::Boolean,
        Some(
            T::Tinyint
            | T::Smallint
            | T::Integer
            | T::Bigint
            | T::Hugeint
            | T::UTinyint
            | T::USmallint
            | T::UInteger
            | T::UBigint
            | T::UHugeint,
        ) => Kind::Integer,
        Some(T::Float | T::Double) => Kind::Float,
        Some(T::Decimal) => Kind::Decimal { scale },
        Some(T::Date) => Kind::Date,
        Some(T::Timestamp | T::TimestampS | T::TimestampMs | T::TimestampNs) => Kind::Timestamp,
        _ => Kind::Text,
    }
}

/// The cell for `value` of a column of the DuckDB type `id`.
pub(super) fn cell_of(value: ValueRef<'_>, id: Option<LogicalTypeId>) -> Cell {
    match value {
        ValueRef::Null => Cell::Null,
        ValueRef::Boolean(v) => Cell::Boolean(v),
        ValueRef::TinyInt(v) => Cell::Integer(i64::from(v)),
        ValueRef::SmallInt(v) => Cell::Integer(i64::from(v)),
        ValueRef::Int(v) => Cell::Integer(i64::from(v)),
        ValueRef::BigInt(v) => Cell::Integer(v),
        ValueRef::UTinyInt(v) => Cell::Integer(i64::from(v)),
        ValueRef::USmallInt(v) => Cell::Integer(i64::from(v)),
        ValueRef::UInt(v) => Cell::Integer(i64::from(v)),
        ValueRef::UBigInt(v) => integer_cell(i128::from(v)),
        ValueRef::HugeInt(v) => integer_cell(v),
        ValueRef::UHugeInt(v) => match i128::try_from(v) {
            Ok(v) => integer_cell(v),
            Err(_) => Cell::Text(v.to_string()),
        },
        ValueRef::Float(v) => Cell::Float(widen(v)),
        ValueRef::Double(v) => Cell::Float(v),
        ValueRef::Decimal(v) => {
            match Decimal::try_from_i128_with_scale(v.value(), u32::from(v.scale())) {
                Ok(decimal) => Cell::Decimal(decimal),
                // More than 96 bits: the exact digits as text.
                Err(_) => Cell::Text(v.to_string()),
            }
        }
        ValueRef::Timestamp(unit, v) => {
            if id == Some(LogicalTypeId::TimestampTZ) {
                Cell::Text(format!("{}Z", timestamp_text(unit, v)))
            } else {
                match exact_micros(unit, v) {
                    Some(micros) if !is_infinite_micros(micros) => {
                        Cell::Timestamp(Timestamp::from_micros(micros))
                    }
                    _ => Cell::Text(timestamp_text(unit, v)),
                }
            }
        }
        ValueRef::Date32(days) => match infinity(i64::from(days), i64::from(i32::MAX)) {
            Some(text) => Cell::Text(text.to_owned()),
            None => Cell::Date(Date::from_days_since_epoch(days)),
        },
        ValueRef::Time64(unit, v) => Cell::Text(time_text(unit, v)),
        ValueRef::Interval {
            months,
            days,
            nanos,
        } => Cell::Text(interval_text(months, days, nanos)),
        // Lossy by design, see the module documentation.
        ValueRef::Text(bytes) => Cell::Text(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) if id == Some(LogicalTypeId::Bit) => Cell::Text(bit_text(bytes)),
        ValueRef::Blob(bytes) | ValueRef::Geometry(bytes) => Cell::Text(hex(bytes)),
        ValueRef::Enum(..) => match value.as_str() {
            Ok(name) => Cell::Text(name.to_owned()),
            // A label that cannot be resolved, see the module documentation.
            Err(_) => Cell::Null,
        },
        // Lists, arrays, structs, maps and unions, and whatever a later
        // DuckDB adds: the owned value knows how to walk them.
        nested => Cell::Text(json_text(&nested.to_owned())),
    }
}

/// An integer of up to 128 bits as the narrowest exact cell.
fn integer_cell(value: i128) -> Cell {
    if let Ok(small) = i64::try_from(value) {
        return Cell::Integer(small);
    }
    match Decimal::try_from_i128_with_scale(value, 0) {
        Ok(decimal) => Cell::Decimal(decimal),
        Err(_) => Cell::Text(value.to_string()),
    }
}

/// A `FLOAT` as the double a person would write for it: 1.1, not the
/// 1.100000023841858 that widening the bits gives.
fn widen(value: f32) -> f64 {
    value.to_string().parse().unwrap_or(f64::from(value))
}

/// `value` in microseconds, if that is exact and in range.
fn exact_micros(unit: TimeUnit, value: i64) -> Option<i64> {
    match unit {
        TimeUnit::Second => value.checked_mul(MICROS_PER_SECOND),
        TimeUnit::Millisecond => value.checked_mul(1_000),
        TimeUnit::Microsecond => Some(value),
        TimeUnit::Nanosecond => (value % NANOS_PER_MICRO == 0).then_some(value / NANOS_PER_MICRO),
    }
}

/// DuckDB stores `infinity` and `-infinity` as the largest value and its
/// negation.
fn infinity(value: i64, max: i64) -> Option<&'static str> {
    if value == max {
        Some("infinity")
    } else if value == -max {
        Some("-infinity")
    } else {
        None
    }
}

fn is_infinite_micros(micros: i64) -> bool {
    infinity(micros, i64::MAX).is_some()
}

/// A timestamp as `YYYY-MM-DDTHH:MM:SS` with as many decimals as it has:
/// none, six, or nine for a `TIMESTAMP_NS` with nanoseconds.
fn timestamp_text(unit: TimeUnit, value: i64) -> String {
    if let Some(text) = infinity(value, i64::MAX) {
        return text.to_owned();
    }
    match exact_micros(unit, value) {
        Some(micros) => Timestamp::from_micros(micros).to_string(),
        None if unit == TimeUnit::Nanosecond => {
            let micros = value.div_euclid(NANOS_PER_MICRO);
            let seconds = micros.div_euclid(MICROS_PER_SECOND) * MICROS_PER_SECOND;
            let nanos = value.rem_euclid(NANOS_PER_MICRO * MICROS_PER_SECOND);
            format!("{}.{nanos:09}", Timestamp::from_micros(seconds))
        }
        // Out of the range of the output model: the raw count says more than
        // a wrong date would.
        None => format!("{value} {}", unit_name(unit)),
    }
}

fn unit_name(unit: TimeUnit) -> &'static str {
    match unit {
        TimeUnit::Second => "s",
        TimeUnit::Millisecond => "ms",
        TimeUnit::Microsecond => "us",
        TimeUnit::Nanosecond => "ns",
    }
}

/// A time of day as `HH:MM:SS`, with the decimals it has.
fn time_text(unit: TimeUnit, value: i64) -> String {
    let (seconds, fraction, digits) = match unit {
        TimeUnit::Second => (value, 0, 0),
        TimeUnit::Millisecond => (value.div_euclid(1_000), value.rem_euclid(1_000) * 1_000, 6),
        TimeUnit::Microsecond => (
            value.div_euclid(MICROS_PER_SECOND),
            value.rem_euclid(MICROS_PER_SECOND),
            6,
        ),
        TimeUnit::Nanosecond => (
            value.div_euclid(1_000_000_000),
            value.rem_euclid(1_000_000_000),
            9,
        ),
    };
    let mut text = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    );
    if fraction != 0 {
        text.push_str(&format!(".{fraction:0digits$}"));
    }
    text
}

/// An interval the way DuckDB prints it: `1 year 2 months 3 days 04:05:06`.
fn interval_text(months: i32, days: i32, nanos: i64) -> String {
    fn push(parts: &mut Vec<String>, count: i64, unit: &str) {
        if count != 0 {
            let plural = if count.abs() == 1 { "" } else { "s" };
            parts.push(format!("{count} {unit}{plural}"));
        }
    }
    let mut parts = Vec::new();
    push(&mut parts, i64::from(months / 12), "year");
    push(&mut parts, i64::from(months % 12), "month");
    push(&mut parts, i64::from(days), "day");
    if nanos != 0 || parts.is_empty() {
        let sign = if nanos < 0 { "-" } else { "" };
        let time = time_text(
            TimeUnit::Nanosecond,
            i64::try_from(nanos.unsigned_abs()).unwrap_or(i64::MAX),
        );
        parts.push(format!("{sign}{time}"));
    }
    parts.join(" ")
}

/// Bytes as lower-case hex digits.
fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// A `BIT` value as `0` and `1`. DuckDB stores the number of padding bits in
/// the first byte and the bits after it, padded at the front.
fn bit_text(bytes: &[u8]) -> String {
    let Some((&padding, bits)) = bytes.split_first() else {
        return String::new();
    };
    bits.iter()
        .flat_map(|byte| (0..8).rev().map(move |bit| (byte >> bit) & 1))
        .skip(usize::from(padding))
        .map(|bit| if bit == 1 { '1' } else { '0' })
        .collect()
}

/// A nested value as JSON text: a list as an array, a struct as an object
/// with its fields in order, a map as an object whose keys are the text of the
/// map keys. Inside, a date or a timestamp is a string, a blob a string of
/// hex digits, a float that JSON cannot express (`NaN`, infinities) a string.
///
/// Numbers follow the rule of `--output json` (see [`crate::output`]), so
/// that a reader who parses the text with doubles gets no silently rounded
/// value: an integer is a number up to a magnitude of 2^53 and a decimal up
/// to 15 significant digits; beyond that each is a string of its exact
/// digits.
///
/// In `--output json` the whole text is one JSON string: the result model has
/// no nested cells.
pub(super) fn json_text(value: &Value) -> String {
    let mut text = String::new();
    push_json(&mut text, value);
    text
}

fn push_json(text: &mut String, value: &Value) {
    match value {
        Value::Null => text.push_str("null"),
        Value::Boolean(v) => text.push_str(if *v { "true" } else { "false" }),
        Value::Float(v) if v.is_finite() => text.push_str(&v.to_string()),
        Value::Double(v) if v.is_finite() => text.push_str(&v.to_string()),
        Value::List(items) | Value::Array(items) => {
            text.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                push_json(text, item);
            }
            text.push(']');
        }
        Value::Struct(fields) => {
            text.push('{');
            for (index, (name, item)) in fields.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                push_json_string(text, name);
                text.push(':');
                push_json(text, item);
            }
            text.push('}');
        }
        Value::Map(entries) => {
            text.push('{');
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                push_json_string(text, &scalar_text(key));
                text.push(':');
                push_json(text, item);
            }
            text.push('}');
        }
        Value::Union(inner) => push_json(text, inner),
        scalar => {
            let plain = scalar_text(scalar);
            if is_json_number(scalar, &plain) {
                text.push_str(&plain);
            } else {
                push_json_string(text, &plain);
            }
        }
    }
}

/// Whether this scalar, whose text is `plain`, is written as a JSON number:
/// it is a number, and a double carries it through unchanged.
fn is_json_number(value: &Value, plain: &str) -> bool {
    let safe_integer = |magnitude: u128| magnitude <= u128::from(MAX_SAFE_INTEGER);
    match value {
        Value::TinyInt(_)
        | Value::SmallInt(_)
        | Value::Int(_)
        | Value::UTinyInt(_)
        | Value::USmallInt(_)
        | Value::UInt(_) => true,
        Value::BigInt(v) => safe_integer(u128::from(v.unsigned_abs())),
        Value::UBigInt(v) => safe_integer(u128::from(*v)),
        Value::HugeInt(v) => safe_integer(v.unsigned_abs()),
        Value::UHugeInt(v) => safe_integer(*v),
        Value::Decimal(_) => significant_digits(plain) <= DOUBLE_DIGITS,
        _ => false,
    }
}

/// The number of significant digits of the decimal number `plain`: its
/// digits without the leading and the trailing zeros.
fn significant_digits(plain: &str) -> usize {
    let digits: String = plain.chars().filter(char::is_ascii_digit).collect();
    digits
        .trim_start_matches('0')
        .trim_end_matches('0')
        .len()
        .max(1)
}

/// The text of a value inside a nested value. Scalars only; a nested value
/// as a map key gets its JSON text.
fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Boolean(v) => v.to_string(),
        Value::TinyInt(v) => v.to_string(),
        Value::SmallInt(v) => v.to_string(),
        Value::Int(v) => v.to_string(),
        Value::BigInt(v) => v.to_string(),
        Value::HugeInt(v) => v.to_string(),
        Value::UHugeInt(v) => v.to_string(),
        Value::UTinyInt(v) => v.to_string(),
        Value::USmallInt(v) => v.to_string(),
        Value::UInt(v) => v.to_string(),
        Value::UBigInt(v) => v.to_string(),
        Value::Float(v) => float_text(f64::from(*v), &v.to_string()),
        Value::Double(v) => float_text(*v, &v.to_string()),
        Value::Decimal(v) => v.to_string(),
        Value::Timestamp(unit, v) => timestamp_text(*unit, *v),
        Value::Text(v) | Value::Enum(v) => v.clone(),
        Value::Blob(v) | Value::Geometry(v) => hex(v),
        Value::Date32(days) => match infinity(i64::from(*days), i64::from(i32::MAX)) {
            Some(text) => text.to_owned(),
            None => Date::from_days_since_epoch(*days).to_string(),
        },
        Value::Time64(unit, v) => time_text(*unit, *v),
        Value::Interval {
            months,
            days,
            nanos,
        } => interval_text(*months, *days, *nanos),
        nested => json_text(nested),
    }
}

fn float_text(value: f64, finite: &str) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else {
        finite.to_owned()
    }
}

fn push_json_string(text: &mut String, value: &str) {
    // A string always serialises; the fallback only keeps this total.
    match serde_json::to_string(value) {
        Ok(literal) => text.push_str(&literal),
        Err(_) => text.push_str("\"\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_take_the_narrowest_exact_cell() {
        assert_eq!(integer_cell(-5), Cell::Integer(-5));
        assert_eq!(
            integer_cell(i128::from(i64::MAX) + 1),
            Cell::Decimal(Decimal::from_i128_with_scale(9_223_372_036_854_775_808, 0))
        );
        assert_eq!(
            integer_cell(i128::MAX),
            Cell::Text("170141183460469231731687303715884105727".to_owned())
        );
    }

    #[test]
    fn a_float_is_widened_by_its_text() {
        assert_eq!(widen(1.1), 1.1);
        assert_eq!(widen(0.5), 0.5);
        assert!(widen(f32::NAN).is_nan());
        assert_eq!(widen(f32::INFINITY), f64::INFINITY);
    }

    #[test]
    fn timestamps_keep_every_digit() {
        assert_eq!(
            timestamp_text(TimeUnit::Second, 1_709_210_096),
            "2024-02-29T12:34:56"
        );
        assert_eq!(
            timestamp_text(TimeUnit::Nanosecond, 1_709_210_096_123_456_789),
            "2024-02-29T12:34:56.123456789"
        );
        assert_eq!(
            timestamp_text(TimeUnit::Nanosecond, 1_709_210_096_123_456_000),
            "2024-02-29T12:34:56.123456"
        );
        // Before 1970 the parts are still taken towards the past.
        assert_eq!(
            timestamp_text(TimeUnit::Nanosecond, -1),
            "1969-12-31T23:59:59.999999999"
        );
        assert_eq!(timestamp_text(TimeUnit::Microsecond, i64::MAX), "infinity");
        assert_eq!(
            timestamp_text(TimeUnit::Second, i64::MAX - 1),
            "9223372036854775806 s"
        );
    }

    #[test]
    fn times_and_intervals() {
        assert_eq!(
            time_text(TimeUnit::Microsecond, 45_296_789_000),
            "12:34:56.789000"
        );
        assert_eq!(time_text(TimeUnit::Microsecond, 45_296_000_000), "12:34:56");
        assert_eq!(
            interval_text(14, 3, 3_000_000_000),
            "1 year 2 months 3 days 00:00:03"
        );
        assert_eq!(interval_text(0, 1, 0), "1 day");
        assert_eq!(interval_text(0, 0, 0), "00:00:00");
        assert_eq!(interval_text(0, 0, -1_500_000_000), "-00:00:01.500000000");
    }

    #[test]
    fn blobs_and_bits() {
        assert_eq!(hex(&[0xaa, 0x00, 0x0f]), "aa000f");
        assert_eq!(hex(&[]), "");
        // '101'::BIT is stored as five padding bits and the byte 11111101.
        assert_eq!(bit_text(&[5, 0b1111_1101]), "101");
        assert_eq!(bit_text(&[]), "");
    }

    #[test]
    fn nested_values_are_json_text() {
        let decimal = duckdb::types::Decimal::new(5, 2, 125).unwrap();
        let value = Value::List(vec![
            Value::Int(1),
            Value::Null,
            Value::Text("a \"b\"".to_owned()),
            Value::Decimal(decimal),
            Value::Date32(19_723),
            Value::Double(f64::NAN),
            Value::Blob(vec![1, 255]),
        ]);
        assert_eq!(
            json_text(&value),
            r#"[1,null,"a \"b\"",1.25,"2024-01-01","NaN","01ff"]"#
        );
    }

    #[test]
    fn nested_numbers_a_double_cannot_hold_are_strings() {
        let decimal = |width, scale, value| {
            Value::Decimal(duckdb::types::Decimal::new(width, scale, value).unwrap())
        };
        let safe = 1_i64 << 53;
        let value = Value::Struct(
            vec![
                ("at".to_owned(), Value::BigInt(safe)),
                ("beyond".to_owned(), Value::BigInt(safe + 1)),
                ("below".to_owned(), Value::BigInt(-safe - 1)),
                ("max".to_owned(), Value::BigInt(i64::MAX)),
                ("min".to_owned(), Value::BigInt(i64::MIN)),
                ("unsigned".to_owned(), Value::UBigInt(u64::MAX)),
                ("huge".to_owned(), Value::HugeInt(i128::from(safe) + 1)),
                ("small_huge".to_owned(), Value::HugeInt(-7)),
                ("uhuge".to_owned(), Value::UHugeInt(u128::MAX)),
                (
                    "list".to_owned(),
                    Value::List(vec![
                        // 15 significant digits is the limit of a decimal.
                        decimal(18, 2, 123_456_789_012_345),
                        decimal(18, 2, 1_234_567_890_123_456),
                        decimal(38, 3, 12_345_678_901_234_567_890_123_456_789),
                        // Trailing zeros are not significant.
                        decimal(38, 0, 10_000_000_000_000_000_000_000),
                    ]),
                ),
            ]
            .into(),
        );
        assert_eq!(
            json_text(&value),
            concat!(
                r#"{"at":9007199254740992,"beyond":"9007199254740993","#,
                r#""below":"-9007199254740993","max":"9223372036854775807","#,
                r#""min":"-9223372036854775808","unsigned":"18446744073709551615","#,
                r#""huge":"9007199254740993","small_huge":-7,"#,
                r#""uhuge":"340282366920938463463374607431768211455","#,
                r#""list":[1234567890123.45,"12345678901234.56","#,
                r#""12345678901234567890123456.789",10000000000000000000000]}"#
            )
        );
        // Still JSON, and the strings hold the exact digits.
        let parsed: serde_json::Value = serde_json::from_str(&json_text(&value)).unwrap();
        assert_eq!(parsed["max"], serde_json::json!("9223372036854775807"));
    }

    #[test]
    fn significant_digits_ignore_sign_point_and_outer_zeros() {
        assert_eq!(significant_digits("0"), 1);
        assert_eq!(significant_digits("-0.00120"), 2);
        assert_eq!(significant_digits("1200"), 2);
        assert_eq!(significant_digits("1001"), 4);
        assert_eq!(significant_digits("12345678901234.56"), 16);
    }
}
