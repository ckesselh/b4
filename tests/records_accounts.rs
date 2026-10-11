//! Accounts and account groups against the record fixtures produced with
//! Banking4.
//!
//! `tests/fixtures/records/account.*` and `account_group.*` hold records that
//! Banking4 wrote from made-up values, and the expected result of reading them
//! (`README-accounts.md` there). Every record is decoded here
//! and compared field by field with that description, through the JSON form
//! of the decoded struct. The names and numbers of the enumerations are
//! spelled out again in this file, so that the comparison does not lean on
//! the tables it is checking.
//!
//! The same records are then cut after every byte. A walker that knows only
//! the sizes of the wire primitives (02 §4) and the order of the fields
//! (03 "Account") says which fields lie wholly in front of a cut; those must
//! read as in the full record, and at a cut between two fields everything
//! behind it must read as its default.

// Test helpers outside `#[test]` functions unwrap freely: a failure is a
// broken fixture, and the panic message names it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use b4::model::accounts::{load_accounts, Account, AccountGroup, DOCUMENTED_RECORD_VERSION};
use b4::model::base::{load_collection, load_collection_strict, LoadReport, Record};
use b4::wire::records;
use serde::Serialize;
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------- fixtures

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/records")
        .join(name)
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = fixture_path(name);
    fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The `records` array of a fixture description.
fn fixture_records(name: &str) -> Vec<Value> {
    let text = String::from_utf8(fixture_bytes(name)).unwrap();
    let mut description: Value = serde_json::from_str(&text).unwrap();
    match description["records"].take() {
        Value::Array(records) => records,
        other => panic!("{name}: no records array, found {other}"),
    }
}

/// The bodies of the records of a collection file, without size prefixes,
/// checked against the offsets and sizes the description gives.
fn fixture_bodies(bin: &str, described: &[Value]) -> Vec<Vec<u8>> {
    let bytes = fixture_bytes(bin);
    let bodies: Vec<Vec<u8>> = records(&bytes)
        .map(|record| {
            let record = record.unwrap();
            let about = &described[record.index];
            assert_eq!(
                json!(record.offset),
                about["offset"],
                "{bin} record {}",
                record.index
            );
            assert_eq!(json!(record.body.len()), about["record_size"]);
            record.body.to_vec()
        })
        .collect();
    assert_eq!(bodies.len(), described.len(), "{bin}");
    bodies
}

// ------------------------------------------- the fixture's notation of values

/// How a field of the fixture description relates to the decoded struct.
#[derive(Clone, Copy)]
enum Shape {
    /// The same JSON value under the same key.
    Same,
    /// An account kind: a number there, its name here.
    Kind,
    /// A flag set: a number there, the list of the set flags here. The width
    /// is the number of bits the field has on the wire.
    Flags(&'static [(u32, &'static str)], u32),
    /// A decimal as text with its scale. The fixture shows a stored negative
    /// zero with its sign, which `Decimal` does not keep.
    Decimal,
    /// A date: a time stamp at midnight there, a plain date here.
    Midnight,
    /// A retention period: the unit is a number there and a name here.
    Period,
    /// Bank access material: described by the fixture, absent from the
    /// decoded struct on purpose.
    Dropped,
}

/// 03 "Account kind".
const KINDS: [(u8, &str); 8] = [
    (0x00, "none"),
    (0x01, "bank"),
    (0x02, "cash"),
    (0x04, "credit_card"),
    (0x08, "portfolio"),
    (0x10, "asset"),
    (0x20, "paypal"),
    (0x40, "crypto"),
];

/// 03 "Account flags".
const ACCOUNT_FLAGS: &[(u32, &str)] = &[
    (0x0000_0001, "hidden"),
    (0x0000_0002, "exclude_from_totals"),
    (0x0000_0004, "legacy_statement_after_order"),
    (0x0000_0008, "show_holder_name"),
    (0x0000_0010, "nagging_contact"),
    (0x0000_0020, "nagging_order_contact"),
    (0x0000_0040, "exclude_from_general_sync"),
    (0x0000_0080, "exclude_from_category_totals"),
    (0x0000_0100, "statement_schedule_weekly"),
    (0x0000_0200, "statement_schedule_monthly"),
    (0x0000_0800, "show_summary_items"),
    (0x0000_1000, "do_not_acknowledge_statements"),
    (0x0000_2000, "vop_opt_in_batch_transfers"),
    (0x0000_4000, "vop_opt_out_single_transfers"),
    (0x0100_0000, "legacy_include_in_replenish"),
];

/// 03 "Sync flags".
const SYNC_FLAGS: &[(u32, &str)] = &[
    (0x0000_0002, "balances"),
    (0x0000_0004, "statements"),
    (0x0000_0008, "standing_orders"),
    (0x0000_0010, "postdated_transfers"),
    (0x0000_0020, "portfolio"),
    (0x0000_0040, "assets"),
    (0x0000_0080, "files"),
    (0x0000_0100, "account_info"),
    (0x0000_0200, "pay_checks"),
    (0x0000_0400, "payees"),
    (0x0000_0800, "portfolio_statement"),
    (0x0000_1000, "direct_debit_advice"),
    (0x0000_2000, "payment_status_report"),
    (0x1000_0000, "protocol"),
];

/// 03 "Balance flags".
const BALANCE_FLAGS: &[(u32, &str)] = &[
    (0x0001, "has_bank_overdraft_limit"),
    (0x0002, "has_bank_available_funds"),
    (0x0100, "show_overdraft_limit"),
    (0x0200, "show_available_funds"),
    (0x0400, "show_credit_interest_rate"),
];

/// 03 "Notification flags".
const NOTIFY_FLAGS: &[(u32, &str)] = &[
    (0x1, "disable_debit_credit_notifications"),
    (0x2, "enable_balance_notifications"),
    (0x4, "enable_spending_limit_notifications"),
];

/// 03 "Bookkeeping-service flags".
const BOOKKEEPING_FLAGS: &[(u32, &str)] = &[(0x1, "disabled")];

/// 03 "Group flags".
const GROUP_FLAGS: &[(u32, &str)] = &[
    (0x1, "show_first_account_icon"),
    (0x2, "anonymous"),
    (0x4, "exclude_from_totals"),
];

/// 03 "Retention period".
const RETENTION_UNITS: [&str; 3] = ["days", "calendar_months", "calendar_years"];

/// Every field of the account fixture's `set` and `read` objects.
const ACCOUNT_FIELDS: &[(&str, Shape)] = &[
    ("oid", Shape::Same),
    ("update_stamp", Shape::Same),
    ("account_kind", Shape::Kind),
    ("country_code", Shape::Same),
    ("bank_code", Shape::Same),
    ("account_number", Shape::Same),
    ("currency", Shape::Same),
    ("name", Shape::Same),
    ("contact_name", Shape::Dropped),
    ("iban", Shape::Same),
    ("bic", Shape::Same),
    ("holder_name", Shape::Same),
    ("description", Shape::Same),
    ("last_download_time", Shape::Same),
    ("statement_retention", Shape::Period),
    ("sync_flags", Shape::Flags(SYNC_FLAGS, 32)),
    ("last_sync_time", Shape::Same),
    ("statement_balance_time", Shape::Same),
    ("statement_balance", Shape::Decimal),
    ("online_balance_time", Shape::Same),
    ("online_balance", Shape::Decimal),
    ("order_contact_name", Shape::Dropped),
    ("flags", Shape::Flags(ACCOUNT_FLAGS, 32)),
    ("opening_date", Shape::Midnight),
    ("holder_street", Shape::Same),
    ("holder_town", Shape::Same),
    ("credit_interest_rate", Shape::Decimal),
    ("debit_interest_rate", Shape::Decimal),
    ("overdraft_interest_rate", Shape::Decimal),
    ("overdraft_limit", Shape::Decimal),
    ("fints_account_sub_id", Shape::Dropped),
    ("creditor_id", Shape::Same),
    ("reference_account_iban", Shape::Same),
    ("internal_transfer_ibans", Shape::Same),
    ("originator_name", Shape::Same),
    ("last_download_token", Shape::Dropped),
    ("reserved_text", Shape::Same),
    ("position", Shape::Same),
    ("group_oid", Shape::Same),
    ("balance_flags", Shape::Flags(BALANCE_FLAGS, 16)),
    ("available_funds", Shape::Decimal),
    ("notify_flags", Shape::Flags(NOTIFY_FLAGS, 16)),
    ("notify_min_debit", Shape::Decimal),
    ("notify_min_credit", Shape::Decimal),
    ("notify_min_balance", Shape::Decimal),
    ("notify_max_balance", Shape::Decimal),
    ("notify_spending_limit", Shape::Decimal),
    ("fints_customer_id", Shape::Dropped),
    ("payments_retention", Shape::Period),
    ("files_retention", Shape::Period),
    ("contact_guid", Shape::Dropped),
    ("order_contact_guid", Shape::Dropped),
    ("sevdesk_last_entry_date", Shape::Midnight),
    ("sevdesk_flags", Shape::Flags(BOOKKEEPING_FLAGS, 32)),
    ("icon_name", Shape::Same),
];

/// Keys of the decoded account that the fixture does not describe: wire
/// values Banking4 does not keep in memory. Each is checked on its own in
/// [`check_account_extras`].
const ACCOUNT_EXTRA_KEYS: [&str; 5] = [
    "record_version",
    "stored_account_kind",
    "statement_retention_days",
    "payments_retention_text",
    "files_retention_text",
];

/// Every field of the group fixture's `set` and `read` objects.
const GROUP_FIELDS: &[(&str, Shape)] = &[
    ("oid", Shape::Same),
    ("update_stamp", Shape::Same),
    ("position", Shape::Same),
    ("name", Shape::Same),
    ("description", Shape::Same),
    ("flags", Shape::Flags(GROUP_FLAGS, 32)),
];

fn kind_name(raw: i64) -> String {
    KINDS
        .iter()
        .find(|(value, _)| i64::from(*value) == raw)
        .map_or_else(|| raw.to_string(), |(_, name)| (*name).to_owned())
}

/// The bits a serialised flag set stands for: the named flags through
/// `table`, and one item of unnamed bits in hexadecimal.
fn flag_bits(names: &Value, table: &[(u32, &str)]) -> u32 {
    let mut bits = 0;
    for name in names.as_array().unwrap() {
        let name = name.as_str().unwrap();
        let value = match table.iter().find(|(_, known)| *known == name) {
            Some((value, _)) => *value,
            None => {
                let digits = name
                    .strip_prefix("0x")
                    .unwrap_or_else(|| panic!("unknown flag name {name}"));
                let unnamed = u32::from_str_radix(digits, 16).unwrap();
                let named = table.iter().fold(0, |all, (value, _)| all | value);
                assert_eq!(unnamed & named, 0, "{name} repeats named flags");
                unnamed
            }
        };
        assert_eq!(bits & value, 0, "{name} listed twice");
        bits |= value;
    }
    bits
}

/// Whether the two texts are the same decimal, allowing for the sign of a
/// zero.
fn same_decimal(described: &str, decoded: &str) -> bool {
    described == decoded
        || (described.strip_prefix('-') == Some(decoded)
            && decoded.chars().all(|digit| digit == '0' || digit == '.'))
}

/// A long text is described as a unit and a repeat count.
fn expand_repeat(value: &Value) -> Value {
    match (value.get("repeat"), value.get("times")) {
        (Some(Value::String(unit)), Some(Value::Number(times))) => {
            let times = usize::try_from(times.as_u64().unwrap()).unwrap();
            Value::String(unit.repeat(times))
        }
        _ => value.clone(),
    }
}

/// Compare one decoded record, as JSON, with the fixture's `read` object.
/// Every key of either side must be accounted for by `fields` or `extra`.
fn compare_with_description(
    context: &str,
    decoded: &Map<String, Value>,
    described: &Map<String, Value>,
    fields: &[(&str, Shape)],
    extra: &[&str],
) {
    let listed: BTreeSet<&str> = fields.iter().map(|(key, _)| *key).collect();
    for key in described.keys() {
        assert!(
            listed.contains(key.as_str()),
            "{context}: fixture field {key} is not handled"
        );
    }
    let mut expected_keys: BTreeSet<&str> = extra.iter().copied().collect();

    for (key, shape) in fields {
        let described = expand_repeat(
            described
                .get(*key)
                .unwrap_or_else(|| panic!("{context}: the fixture has no {key}")),
        );
        if matches!(shape, Shape::Dropped) {
            assert!(
                !decoded.contains_key(*key),
                "{context}: {key} must not be decoded"
            );
            continue;
        }
        expected_keys.insert(key);
        let actual = decoded
            .get(*key)
            .unwrap_or_else(|| panic!("{context}: {key} is missing"));
        let equal = match shape {
            Shape::Same => *actual == described,
            Shape::Kind => actual.as_str() == Some(&kind_name(described.as_i64().unwrap())),
            Shape::Flags(table, width) => {
                // The fixture prints the number signed; compare the bits.
                let mask = u32::MAX >> (32 - width);
                let stored = described.as_i64().unwrap().to_le_bytes();
                let stored = u32::from_le_bytes([stored[0], stored[1], stored[2], stored[3]]);
                assert_eq!(
                    stored & !mask,
                    0,
                    "{context}: {key} is wider than its field"
                );
                flag_bits(actual, table) == stored
            }
            Shape::Decimal => same_decimal(described.as_str().unwrap(), actual.as_str().unwrap()),
            Shape::Midnight => match &described {
                Value::Null => actual.is_null(),
                described => {
                    let day = described
                        .as_str()
                        .unwrap()
                        .strip_suffix("T00:00:00.0000000")
                        .unwrap_or_else(|| panic!("{context}: {key} is not a date"));
                    actual.as_str() == Some(day)
                }
            },
            Shape::Period => {
                let unit = usize::try_from(described["unit"].as_u64().unwrap()).unwrap();
                *actual == json!({ "unit": RETENTION_UNITS[unit], "count": described["count"] })
            }
            Shape::Dropped => unreachable!(),
        };
        assert!(
            equal,
            "{context}: {key} differs: decoded {actual}, described {described}"
        );
    }

    let decoded_keys: BTreeSet<&str> = decoded.keys().map(String::as_str).collect();
    assert_eq!(
        decoded_keys, expected_keys,
        "{context}: keys of the decoded record"
    );
}

fn to_object<T: Serialize>(record: &T) -> Map<String, Value> {
    match serde_json::to_value(record).unwrap() {
        Value::Object(object) => object,
        other => panic!("a record serialises as {other}"),
    }
}

// ----------------------------------------------------------- account fixtures

/// What the writer stores for a period in the text form (03 "Text form").
fn written_period_text(period: &Value) -> Value {
    let count = period["count"].as_i64().unwrap();
    if count == i64::from(i32::MAX) {
        return Value::Null;
    }
    let letter = match period["unit"].as_i64().unwrap() {
        0 => 'D',
        1 => 'M',
        2 => 'Y',
        _ => '?',
    };
    json!(format!("{letter}{count}"))
}

/// What the writer stores for a period in the day form (03 "Day form").
fn written_period_days(period: &Value) -> i64 {
    let count = i32::try_from(period["count"].as_i64().unwrap()).unwrap();
    let length = match period["unit"].as_i64().unwrap() {
        0 => 1,
        1 => 30,
        2 => 365,
        _ => return i64::from(i32::MAX),
    };
    if count >= 36500 {
        i64::from(i32::MAX)
    } else {
        i64::from(count.wrapping_mul(length))
    }
}

/// The decoded fields the fixture does not describe, checked against what
/// the writer was given (`set`) and the writer rules of the document.
fn check_account_extras(context: &str, decoded: &Map<String, Value>, set: &Value) {
    assert_eq!(
        decoded["record_version"],
        json!(DOCUMENTED_RECORD_VERSION),
        "{context}"
    );
    assert_eq!(
        decoded["stored_account_kind"],
        json!(kind_name(set["account_kind"].as_i64().unwrap() & 0xFF)),
        "{context}: stored_account_kind"
    );
    assert_eq!(
        decoded["statement_retention_days"],
        json!(written_period_days(&set["statement_retention"])),
        "{context}: statement_retention_days"
    );
    for key in ["payments_retention", "files_retention"] {
        assert_eq!(
            decoded[&format!("{key}_text")],
            written_period_text(&set[key]),
            "{context}: {key}_text"
        );
    }
}

#[test]
fn account_fixtures_decode_to_the_described_values() {
    let described = fixture_records("account.json");
    assert_eq!(described.len(), 24);
    fixture_bodies("account.bin", &described);

    // Strict: every byte of every chunk of every record is consumed.
    let collection = load_collection_strict::<Account>(&fixture_bytes("account.bin")).unwrap();
    assert_eq!(collection.records.len(), described.len());
    assert!(collection.report.is_clean());
    assert!(collection.report.short_chunks.is_zero());

    for (index, (account, about)) in collection.records.iter().zip(&described).enumerate() {
        let context = format!("account {index} ({})", about["about"].as_str().unwrap());
        let decoded = to_object(account);
        compare_with_description(
            &context,
            &decoded,
            about["read"].as_object().unwrap(),
            ACCOUNT_FIELDS,
            &ACCOUNT_EXTRA_KEYS,
        );
        check_account_extras(&context, &decoded, &about["set"]);
    }
}

/// The loader's view of the same file: the record of kind none is dropped,
/// nothing is newer than the documented version, nothing to warn about.
#[test]
fn account_fixture_file_loads_without_the_placeholder() {
    let described = fixture_records("account.json");
    let placeholders = described
        .iter()
        .filter(|about| about["read"]["account_kind"] == json!(0))
        .count();
    // "kind none" and "a record with every field zero or null".
    assert_eq!(placeholders, 2);

    let loaded = load_accounts(&fixture_bytes("account.bin"));
    assert_eq!(loaded.placeholders, placeholders);
    assert_eq!(loaded.accounts.len(), described.len() - placeholders);
    assert_eq!(loaded.report.decoded, described.len());
    assert_eq!(loaded.newer_versions, 0);
    assert_eq!(loaded.highest_version, DOCUMENTED_RECORD_VERSION);
    assert!(loaded.warnings("AcctsNew").is_empty());
    assert!(loaded
        .accounts
        .iter()
        .all(|account| !account.is_placeholder()));
}

#[test]
fn account_group_fixtures_decode_to_the_described_values() {
    let described = fixture_records("account_group.json");
    assert_eq!(described.len(), 7);
    fixture_bodies("account_group.bin", &described);

    let collection =
        load_collection_strict::<AccountGroup>(&fixture_bytes("account_group.bin")).unwrap();
    assert_eq!(collection.records.len(), described.len());
    assert!(collection.report.is_clean());
    assert!(collection.report.short_chunks.is_zero());

    for (index, (group, about)) in collection.records.iter().zip(&described).enumerate() {
        let context = format!("group {index} ({})", about["about"].as_str().unwrap());
        compare_with_description(
            &context,
            &to_object(group),
            about["read"].as_object().unwrap(),
            GROUP_FIELDS,
            &[],
        );
    }
}

// ------------------------------------------------------------- truncation

/// The wire primitives the two records are made of, as far as their size
/// goes (02 §4).
#[derive(Clone, Copy)]
enum Prim {
    U8,
    U16,
    I32,
    Decimal,
    Date,
    Ticks,
    Guid,
    Code(usize),
    Text,
    /// A presence byte and, if it is set, a count and that many texts.
    TextList,
}

/// One wire field: its name in the table of 03, its primitive, and the keys
/// of the decoded record that depend on this field and on nothing else.
/// Fields whose decoded value also depends on other fields (the day and time
/// stamp pairs, everything the reader's processing steps touch) and fields
/// that are not decoded at all have no keys.
type Field = (&'static str, Prim, &'static [&'static str]);

/// 03 "Account", fields 1 to 68: the first chunk.
const ACCOUNT_LAYOUT: &[Field] = &[
    ("oid", Prim::I32, &["oid"]),
    ("update_stamp", Prim::I32, &["update_stamp"]),
    ("account_kind", Prim::I32, &["stored_account_kind"]),
    ("country_code", Prim::Code(2), &["country_code"]),
    ("bank_code", Prim::Text, &["bank_code"]),
    ("account_number", Prim::Text, &["account_number"]),
    ("currency", Prim::Code(3), &["currency"]),
    ("name", Prim::Text, &["name"]),
    ("contact_name", Prim::Text, &[]),
    ("iban", Prim::Text, &["iban"]),
    ("bic", Prim::Text, &["bic"]),
    ("holder_name", Prim::Text, &["holder_name"]),
    ("description", Prim::Text, &["description"]),
    ("last_download_date", Prim::Date, &[]),
    ("unused_15", Prim::U8, &[]),
    (
        "statement_retention_days",
        Prim::I32,
        &["statement_retention_days", "statement_retention"],
    ),
    ("record_version", Prim::U8, &["record_version"]),
    ("sync_flags", Prim::I32, &[]),
    ("last_sync_date", Prim::Date, &[]),
    ("statement_balance_date", Prim::Date, &[]),
    ("statement_balance", Prim::Decimal, &[]),
    ("unused_22", Prim::U8, &[]),
    ("unused_23", Prim::U8, &[]),
    ("unused_24", Prim::U8, &[]),
    ("unused_25", Prim::U8, &[]),
    ("unused_26", Prim::I32, &[]),
    ("online_balance_date", Prim::Date, &[]),
    ("online_balance", Prim::Decimal, &[]),
    ("order_contact_name", Prim::Text, &[]),
    ("flags", Prim::I32, &["flags"]),
    ("opening_date", Prim::Date, &["opening_date"]),
    (
        "holder_address",
        Prim::Text,
        &["holder_street", "holder_town"],
    ),
    (
        "credit_interest_rate",
        Prim::Decimal,
        &["credit_interest_rate"],
    ),
    (
        "debit_interest_rate",
        Prim::Decimal,
        &["debit_interest_rate"],
    ),
    (
        "overdraft_interest_rate",
        Prim::Decimal,
        &["overdraft_interest_rate"],
    ),
    ("overdraft_limit", Prim::Decimal, &["overdraft_limit"]),
    ("fints_account_sub_id", Prim::Text, &[]),
    ("last_sync_time", Prim::Ticks, &[]),
    ("creditor_id", Prim::Text, &["creditor_id"]),
    (
        "reference_account_iban",
        Prim::Text,
        &["reference_account_iban"],
    ),
    ("internal_transfer_ibans", Prim::TextList, &[]),
    ("originator_name", Prim::Text, &["originator_name"]),
    ("last_download_token", Prim::Text, &[]),
    ("reserved_text", Prim::Text, &["reserved_text"]),
    ("position", Prim::U16, &["position"]),
    ("group_oid", Prim::I32, &["group_oid"]),
    ("balance_flags", Prim::U16, &["balance_flags"]),
    ("available_funds", Prim::Decimal, &["available_funds"]),
    ("statement_balance_time", Prim::Ticks, &[]),
    ("online_balance_time", Prim::Ticks, &[]),
    ("last_download_time", Prim::Ticks, &[]),
    ("notify_flags", Prim::U16, &["notify_flags"]),
    ("notify_min_debit", Prim::Decimal, &["notify_min_debit"]),
    ("notify_min_credit", Prim::Decimal, &["notify_min_credit"]),
    ("notify_min_balance", Prim::Decimal, &["notify_min_balance"]),
    ("notify_max_balance", Prim::Decimal, &["notify_max_balance"]),
    (
        "notify_spending_limit",
        Prim::Decimal,
        &["notify_spending_limit"],
    ),
    ("fints_customer_id", Prim::Text, &[]),
    (
        "payments_retention",
        Prim::Text,
        &["payments_retention_text"],
    ),
    ("files_retention", Prim::Text, &["files_retention_text"]),
    ("unused_63", Prim::Ticks, &[]),
    ("contact_guid", Prim::Guid, &[]),
    ("order_contact_guid", Prim::Guid, &[]),
    (
        "sevdesk_last_entry_date",
        Prim::Date,
        &["sevdesk_last_entry_date"],
    ),
    ("sevdesk_flags", Prim::I32, &["sevdesk_flags"]),
    ("icon_name", Prim::Text, &["icon_name"]),
];

/// 03 "Account group": its one chunk.
const GROUP_LAYOUT: &[Field] = &[
    ("oid", Prim::I32, &["oid"]),
    ("update_stamp", Prim::I32, &["update_stamp"]),
    ("position", Prim::U16, &["position"]),
    ("name", Prim::Text, &["name"]),
    ("description", Prim::Text, &["description"]),
    ("flags", Prim::I32, &["flags"]),
];

/// The account that an empty record decodes to: every default of the table
/// of 03 "Account", in the shape of a record below version 2.
fn default_account() -> Value {
    // A string and not `json!`: the macro's recursion limit is lower than
    // the number of fields.
    serde_json::from_str(
        r#"{
        "oid": 0,
        "update_stamp": 0,
        "account_kind": "none",
        "stored_account_kind": "none",
        "record_version": 0,
        "country_code": null,
        "bank_code": null,
        "account_number": null,
        "currency": null,
        "name": null,
        "iban": null,
        "bic": null,
        "holder_name": null,
        "description": null,
        "last_download_time": null,
        "statement_retention_days": 0,
        "statement_retention": { "unit": "days", "count": 0 },
        "sync_flags": [],
        "last_sync_time": null,
        "statement_balance_time": null,
        "statement_balance": "0",
        "online_balance_time": null,
        "online_balance": "0",
        "flags": [],
        "opening_date": null,
        "holder_street": null,
        "holder_town": null,
        "credit_interest_rate": "0",
        "debit_interest_rate": "0",
        "overdraft_interest_rate": "0",
        "overdraft_limit": "0",
        "creditor_id": null,
        "reference_account_iban": null,
        "internal_transfer_ibans": null,
        "originator_name": null,
        "reserved_text": null,
        "position": 0,
        "group_oid": 0,
        "balance_flags": [],
        "available_funds": "0",
        "notify_flags": [],
        "notify_min_debit": "0",
        "notify_min_credit": "0",
        "notify_min_balance": "0",
        "notify_max_balance": "0",
        "notify_spending_limit": "0",
        "payments_retention": { "unit": "days", "count": 0 },
        "payments_retention_text": null,
        "files_retention": { "unit": "days", "count": 0 },
        "files_retention_text": null,
        "sevdesk_last_entry_date": null,
        "sevdesk_flags": [],
        "icon_name": null
    }"#,
    )
    .unwrap()
}

fn default_group() -> Value {
    json!({
        "oid": 0,
        "update_stamp": 0,
        "position": 0,
        "name": null,
        "description": null,
        "flags": [],
    })
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

/// Size of a `string` at `offset`, with its length prefix (02 §4.8).
fn text_size(bytes: &[u8], offset: usize) -> usize {
    let short = usize::from(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]));
    if short == 0xFFFF {
        6 + usize::try_from(u32_at(bytes, offset + 2)).unwrap()
    } else {
        2 + short
    }
}

/// Where each field of `layout` starts and ends in the first chunk of
/// `body`, as offsets into the record body. The fields must fill the chunk
/// exactly: Banking4 writes no byte that is not part of a field.
fn field_spans(body: &[u8], layout: &[Field]) -> Vec<(usize, usize)> {
    let chunk_size = usize::try_from(u32_at(body, 0) & 0x3FFF_FFFF).unwrap();
    let mut offset = 4;
    let mut spans = Vec::new();
    for (name, prim, _) in layout {
        let size = match prim {
            Prim::U8 => 1,
            Prim::U16 => 2,
            Prim::I32 | Prim::Date => 4,
            Prim::Ticks => 8,
            Prim::Decimal | Prim::Guid => 16,
            Prim::Code(width) => *width,
            Prim::Text => text_size(body, offset),
            Prim::TextList => {
                let mut size = 1;
                if body[offset] != 0 {
                    let count = u32_at(body, offset + 1);
                    size += 4;
                    for _ in 0..count {
                        size += text_size(body, offset + size);
                    }
                }
                size
            }
        };
        spans.push((offset, offset + size));
        offset += size;
        assert!(offset <= chunk_size, "{name} runs past the chunk");
    }
    assert_eq!(
        offset, chunk_size,
        "bytes of the first chunk that no field explains"
    );
    spans
}

/// A record whose first chunk ends after `cut` bytes of the record body (so
/// `cut` counts the chunk header), as an older writer would have produced
/// it: the chunk header says so. `tail` is what follows the first chunk.
fn cut_first_chunk(body: &[u8], cut: usize, tail: &[u8]) -> Vec<u8> {
    let more = u32::from(!tail.is_empty()) << 31;
    let mut record = (u32::try_from(cut).unwrap() | more).to_le_bytes().to_vec();
    record.extend(&body[4..cut]);
    record.extend(tail);
    record
}

fn as_file(record: &[u8]) -> Vec<u8> {
    let mut file = i32::try_from(record.len()).unwrap().to_le_bytes().to_vec();
    file.extend(record);
    file
}

/// The cut positions to try in a first chunk with these field spans: every
/// byte, except that the inside of a very long field is sampled.
fn cut_positions(spans: &[(usize, usize)]) -> Vec<usize> {
    const MARGIN: usize = 300;
    let mut cuts = vec![4];
    for (start, end) in spans {
        for cut in start + 1..=*end {
            let inside = cut - start > MARGIN && end - cut > MARGIN;
            if !inside || cut % 997 == 0 {
                cuts.push(cut);
            }
        }
    }
    cuts
}

/// One truncated record as it decoded, for the checks of a record type.
struct CutRecord<'a> {
    /// What was cut, for assertion messages.
    context: &'a str,
    /// The decoded record as JSON.
    decoded: &'a Map<String, Value>,
    /// Where the first chunk ends, as an offset into the record body.
    cut: usize,
    /// Whether the cut lies between two fields rather than inside one.
    between_fields: bool,
    /// Whether the record still has what followed its first chunk.
    has_second_chunk: bool,
    /// What the loader counted.
    report: &'a LoadReport,
}

/// Cut the first chunk of `body` after every byte and check each result:
/// the loader must take it as one record; fields wholly in front of the cut
/// read as in the full record; at a cut between two fields the record must
/// decode and every field behind the cut read as its default. `check` gets
/// each decoded record with the cut and whether it lies between two fields.
fn check_truncations<T: Record + Serialize>(
    context: &str,
    body: &[u8],
    layout: &[Field],
    defaults: &Value,
    check: &dyn Fn(&CutRecord<'_>),
) -> usize {
    let spans = field_spans(body, layout);
    let chunk_size = spans.last().unwrap().1;
    let second_chunk = &body[chunk_size..];
    let full = to_object(&load_collection_strict::<T>(&as_file(body)).unwrap().records[0]);

    let mut cases = 0;
    for cut in cut_positions(&spans) {
        let between_fields = cut == 4 || spans.iter().any(|(_, end)| *end == cut);
        // Once with what follows the first chunk still there, once without.
        let mut tails = vec![&[][..]];
        if !second_chunk.is_empty() {
            tails.push(second_chunk);
        }
        for tail in tails {
            let context = format!(
                "{context}, first chunk cut at {cut}, {} bytes behind it",
                tail.len()
            );
            let collection = load_collection::<T>(&as_file(&cut_first_chunk(body, cut, tail)));
            assert_eq!(collection.report.stopped, None, "{context}");
            assert_eq!(collection.report.framed(), 1, "{context}");
            assert_eq!(collection.report.stray_bytes, 0, "{context}");
            cases += 1;
            let Some(record) = collection.records.first() else {
                // Bytes of one field read as another can be a value the
                // reader refuses. Not at a cut between fields, though.
                assert!(
                    !between_fields,
                    "{context}: {:?}",
                    collection.report.undecodable_samples
                );
                continue;
            };
            let decoded = to_object(record);
            for ((_, _, keys), (start, end)) in layout.iter().zip(&spans) {
                for key in *keys {
                    if *end <= cut {
                        assert_eq!(
                            decoded[*key], full[*key],
                            "{context}: {key} lies before the cut"
                        );
                    } else if between_fields && *start >= cut {
                        assert_eq!(
                            decoded[*key], defaults[*key],
                            "{context}: {key} lies behind the cut"
                        );
                    }
                }
            }
            check(&CutRecord {
                context: &context,
                decoded: &decoded,
                cut,
                between_fields,
                has_second_chunk: !tail.is_empty(),
                report: &collection.report,
            });
        }
    }
    cases
}

#[test]
fn an_empty_record_decodes_to_the_documented_defaults() {
    let empty = as_file(&4_u32.to_le_bytes());
    let accounts = load_collection_strict::<Account>(&empty).unwrap();
    assert_eq!(
        Value::Object(to_object(&accounts.records[0])),
        default_account()
    );
    let groups = load_collection_strict::<AccountGroup>(&empty).unwrap();
    assert_eq!(
        Value::Object(to_object(&groups.records[0])),
        default_group()
    );
}

#[test]
fn truncated_account_fixtures_decode_to_defaults_behind_the_cut() {
    let described = fixture_records("account.json");
    let bodies = fixture_bodies("account.bin", &described);
    let defaults = default_account();
    let version = ACCOUNT_LAYOUT
        .iter()
        .position(|(name, _, _)| *name == "record_version")
        .unwrap();
    let field = |name: &str| {
        ACCOUNT_LAYOUT
            .iter()
            .position(|(field, _, _)| *field == name)
            .unwrap()
    };

    let mut cases = 0;
    for (index, body) in bodies.iter().enumerate() {
        let spans = field_spans(body, ACCOUNT_LAYOUT);
        let (version_start, version_end) = spans[version];
        let (payments_start, _) = spans[field("payments_retention")];
        let (files_start, _) = spans[field("files_retention")];
        let (_, download_day_end) = spans[field("last_download_date")];
        // The second chunk is one byte, as the document says.
        assert_eq!(body.len() - spans.last().unwrap().1, 5, "account {index}");

        cases += check_truncations::<Account>(
            &format!("account {index}"),
            body,
            ACCOUNT_LAYOUT,
            &defaults,
            &|record| {
                let CutRecord {
                    context,
                    decoded,
                    cut,
                    between_fields,
                    has_second_chunk,
                    report,
                } = *record;
                if !between_fields {
                    return;
                }
                if cut <= version_start {
                    // "A record cut before field 17 has record_version 0 and
                    // therefore takes the early exit": nothing behind field
                    // 21, no processing, periods "not set", and a second
                    // chunk stays unvisited.
                    for (key, value) in decoded {
                        let key = key.as_str();
                        let plain = ACCOUNT_LAYOUT
                            .iter()
                            .any(|(_, _, keys)| keys.contains(&key));
                        // The kind and the download day lie before field 17.
                        if !plain && key != "account_kind" && key != "last_download_time" {
                            assert_eq!(*value, defaults[key], "{context}: {key}");
                        }
                    }
                    // Only the day-precision form of the download time can
                    // have been read.
                    match &decoded["last_download_time"] {
                        Value::Null => {}
                        time => {
                            assert!(cut >= download_day_end, "{context}");
                            assert!(time.as_str().unwrap().ends_with("T00:00:00.0000000"));
                        }
                    }
                    assert_eq!(
                        decoded["account_kind"], decoded["stored_account_kind"],
                        "{context}"
                    );
                    assert_eq!(
                        report.extra_chunks.records,
                        usize::from(has_second_chunk),
                        "{context}"
                    );
                } else if cut >= version_end {
                    // All fixture records are of version 26: the whole layout
                    // is read, the second chunk visited, and a missing period
                    // text is "unlimited".
                    assert!(report.extra_chunks.is_zero(), "{context}");
                    let unlimited = json!({ "unit": "days", "count": i32::MAX });
                    if cut <= payments_start {
                        assert_eq!(decoded["payments_retention"], unlimited, "{context}");
                    }
                    if cut <= files_start {
                        assert_eq!(decoded["files_retention"], unlimited, "{context}");
                    }
                }
                assert!(report.unread_bytes.is_zero(), "{context}");
            },
        );
    }
    // 23 records of about 750 bytes and one of 70 000, each cut twice.
    assert!(cases > 30_000, "{cases} cases");
}

#[test]
fn truncated_group_fixtures_decode_to_defaults_behind_the_cut() {
    let described = fixture_records("account_group.json");
    let bodies = fixture_bodies("account_group.bin", &described);
    let defaults = default_group();
    let mut cases = 0;
    for (index, body) in bodies.iter().enumerate() {
        cases += check_truncations::<AccountGroup>(
            &format!("group {index}"),
            body,
            GROUP_LAYOUT,
            &defaults,
            &|record| {
                let context = record.context;
                assert!(record.report.extra_chunks.is_zero(), "{context}");
                assert!(!record.has_second_chunk, "{context}");
                assert!(record.cut >= 4 && !record.decoded.is_empty(), "{context}");
                // Bytes too few for the next field stay unread; at a cut
                // between fields there are none.
                assert!(
                    !record.between_fields || record.report.unread_bytes.is_zero(),
                    "{context}"
                );
            },
        );
    }
    assert_eq!(
        cases,
        bodies.iter().map(|body| body.len() - 3).sum::<usize>()
    );
}

/// A record cut off without its chunk header being adjusted is not an older
/// record but a damaged one. Whatever the loader makes of it, it must not
/// panic and must not invent records.
#[test]
fn records_cut_off_mid_chunk_never_panic() {
    for (bin, json_name) in [
        ("account.bin", "account.json"),
        ("account_group.bin", "account_group.json"),
    ] {
        let bodies = fixture_bodies(bin, &fixture_records(json_name));
        for body in &bodies {
            let step = if body.len() > 4096 { 251 } else { 1 };
            for cut in (0..body.len()).step_by(step) {
                let file = as_file(&body[..cut]);
                let accounts = load_accounts(&file);
                assert!(accounts.report.framed() <= 1);
                assert!(accounts.accounts.len() + accounts.placeholders <= 1);
                let groups = load_collection::<AccountGroup>(&file);
                assert!(groups.report.framed() <= 1);
                // The file itself cut off, size prefix and all.
                let whole = as_file(body);
                let accounts = load_accounts(&whole[..cut]);
                assert!(accounts.report.decoded <= 1);
            }
        }
    }
}

// --------------------------------------------------------------- mutations

/// A small deterministic generator (xorshift64*), so that a failure can be
/// reproduced from the round number.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(bound).unwrap()).unwrap()
    }
}

/// Damaged records must never panic, never be taken for more records than
/// the file frames, and always serialise.
#[test]
fn mutated_fixture_records_never_panic() {
    let mut bodies = fixture_bodies("account.bin", &fixture_records("account.json"));
    bodies.extend(fixture_bodies(
        "account_group.bin",
        &fixture_records("account_group.json"),
    ));
    // The 70 000-byte record would only slow the loop down.
    bodies.retain(|body| body.len() < 4096);

    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let mut decoded = 0_usize;
    let mut undecodable = 0_usize;
    for round in 0..60_000 {
        let mut record = bodies[random.below(bodies.len())].clone();
        for _ in 0..=random.below(4) {
            let position = random.below(record.len());
            match random.below(6) {
                0 => record[position] = 0xFF,
                1 => record[position] = 0,
                2 => record[position] ^= 1 << random.below(8),
                3 => {
                    record.remove(position);
                }
                4 => record.insert(position, u8::try_from(random.below(256)).unwrap()),
                _ => record[position] = u8::try_from(random.below(256)).unwrap(),
            }
        }
        // Make the chunk structure valid again in half of the rounds, so
        // that the damage reaches the field decoders and not only the
        // chunk check.
        if round % 2 == 0 && record.len() >= 4 {
            // One chunk that spans the whole record.
            let size = u32::try_from(record.len()).unwrap();
            record[..4].copy_from_slice(&size.to_le_bytes());
        }
        let file = as_file(&record);

        let accounts = load_accounts(&file);
        assert!(accounts.report.framed() <= 1, "round {round}");
        assert_eq!(
            accounts.accounts.len() + accounts.placeholders,
            accounts.report.decoded,
            "round {round}"
        );
        for account in &accounts.accounts {
            serde_json::to_string(account).unwrap();
        }
        let _ = accounts.warnings("AcctsNew");
        decoded += accounts.report.decoded;
        undecodable += accounts.report.undecodable;

        let groups = load_collection::<AccountGroup>(&file);
        assert!(groups.report.framed() <= 1, "round {round}");
        for group in &groups.records {
            serde_json::to_string(group).unwrap();
        }
    }
    // The rounds must have exercised both outcomes, or the test shows nothing.
    assert!(decoded > 10_000, "{decoded} decoded");
    assert!(undecodable > 100, "{undecodable} undecodable");
}

// ---------------------------------------------------------------- real data

/// Local check on real data, skipped unless `B4_TEST_RAW_DIR` names a
/// directory holding the decrypted files of a vault. Prints counts only.
#[test]
fn real_account_files_decode() {
    let Some(raw_dir) = std::env::var_os("B4_TEST_RAW_DIR") else {
        return;
    };
    let raw_dir = PathBuf::from(raw_dir);
    for name in ["AcctsNew", "Accts"] {
        let Ok(bytes) = fs::read(raw_dir.join(name)) else {
            continue;
        };
        let loaded = load_accounts(&bytes);
        assert_eq!(loaded.report.stopped, None, "{name}");
        assert_eq!(loaded.report.undecodable, 0, "{name}");
        assert!(loaded.report.extra_chunks.is_zero(), "{name}");
        println!(
            "{name}: {} records, {} placeholders, {} of a newer version (highest {}), {} with unread bytes",
            loaded.report.decoded,
            loaded.placeholders,
            loaded.newer_versions,
            loaded.highest_version,
            loaded.report.unread_bytes.records
        );
        for warning in loaded.warnings(name) {
            println!("  {warning}");
        }
    }
    if let Ok(bytes) = fs::read(raw_dir.join("AcctGroups")) {
        let groups = load_collection::<AccountGroup>(&bytes);
        assert!(groups.report.is_clean(), "AcctGroups");
        println!("AcctGroups: {} records", groups.report.decoded);
    }
}
