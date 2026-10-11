//! Value types the primitives decode to, and the calendar arithmetic behind
//! them (docs/format/02-records-wire-format.md §4.11 to §4.14, §5).
//!
//! All of them are plain data with our own semantics: a [`Date`] is always a
//! real calendar day, "no date" is `None`, and nothing here knows about time
//! zones except [`Stamp`], which is UTC by definition.

use std::fmt;

use rust_decimal::Decimal;
use serde::{Serialize, Serializer};

/// Number of 100 ns ticks in one second.
const TICKS_PER_SECOND: i64 = 10_000_000;
/// Number of 100 ns ticks in one day.
const TICKS_PER_DAY: i64 = 86_400 * TICKS_PER_SECOND;
/// Tick count of 1970-01-01T00:00:00 (02 §4.13).
const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
/// Days from 0001-01-01 to 1970-01-01.
const UNIX_EPOCH_DAY_NUMBER: i64 = 719_162;
/// Tick count of 2000-01-01T00:00:00, the stamp epoch (02 §5.1).
const STAMP_EPOCH_TICKS: i64 = 630_822_816_000_000_000;
/// Seconds from 1970-01-01 to 2000-01-01 (02 §5.1).
const STAMP_EPOCH_UNIX_SECONDS: i64 = 946_684_800;
/// Largest valid tick count, 9999-12-31T23:59:59.9999999 (02 §4.13).
const MAX_TICKS: i64 = 3_155_378_975_999_999_999;

// --------------------------------------------------------------- calendar

/// Whether `year` is a leap year of the proleptic Gregorian calendar.
#[must_use]
pub const fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Number of days of `month` (1 to 12) in `year`; 0 for any other month.
#[must_use]
pub const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days from 1970-01-01 to the given day of the proleptic Gregorian calendar
/// (negative before 1970).
///
/// The arguments are not validated: a month outside 1 to 12 or a day outside
/// the month gives the day that many months or days further on. For a valid
/// date between the years 1 and 9999 the result fits DuckDB's `Date32`.
#[must_use]
pub const fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    // Years are counted from 1 March so that the leap day is the last day of
    // the year; 400 years are one era of exactly 146097 days.
    let month = month as i64;
    let year = year as i64 - if month <= 2 { 1 } else { 0 };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = (month + 9).rem_euclid(12);
    let day_of_year = (153 * month_from_march + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The year, month and day that lie `days` days after 1970-01-01. Inverse of
/// [`days_from_civil`].
#[must_use]
pub const fn civil_from_days(days: i32) -> (i32, u8, u8) {
    let shifted = days as i64 + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    // The casts cannot truncate: an `i32` day count is at most 5.9 million
    // years from 1970, the month is 1..=12 and the day 1..=31.
    (year as i32, month as u8, day as u8)
}

// ------------------------------------------------------------------- Date

/// A calendar date between 0001-01-01 and 9999-12-31, without time and zone.
///
/// "No date" is not a `Date`: the readers return `Option<Date>`. Serialises as
/// ISO 8601 text, `YYYY-MM-DD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

impl Date {
    /// The first day a `Date` can hold.
    pub const MIN: Date = Date {
        year: 1,
        month: 1,
        day: 1,
    };
    /// The last day a `Date` can hold. Banking4 uses it for "end of time".
    pub const MAX: Date = Date {
        year: 9999,
        month: 12,
        day: 31,
    };

    /// The date with these components, or `None` if they are not a real
    /// calendar day of the years 1 to 9999.
    #[must_use]
    pub const fn new(year: u16, month: u8, day: u8) -> Option<Date> {
        if year >= 1 && year <= 9999 && day >= 1 && day <= days_in_month(year as i32, month) {
            Some(Date { year, month, day })
        } else {
            None
        }
    }

    /// The year, 1 to 9999.
    #[must_use]
    pub const fn year(self) -> u16 {
        self.year
    }

    /// The month, 1 to 12.
    #[must_use]
    pub const fn month(self) -> u8 {
        self.month
    }

    /// The day of the month, 1 to 31.
    #[must_use]
    pub const fn day(self) -> u8 {
        self.day
    }

    /// Days since 1970-01-01: the value of a DuckDB `Date32`.
    #[must_use]
    pub const fn days_since_unix_epoch(self) -> i32 {
        // Between -719162 and 2932896 for the years 1 to 9999.
        days_from_civil(self.year as i32, self.month, self.day) as i32
    }

    /// The date `days` days after 1970-01-01, or `None` outside the years 1 to
    /// 9999.
    #[must_use]
    pub const fn from_days_since_unix_epoch(days: i32) -> Option<Date> {
        let (year, month, day) = civil_from_days(days);
        if year >= 1 && year <= 9999 {
            Some(Date {
                year: year as u16,
                month,
                day,
            })
        } else {
            None
        }
    }

    /// Midnight at the start of this day.
    #[must_use]
    pub const fn at_midnight(self) -> Timestamp {
        Timestamp((self.days_since_unix_epoch() as i64 + UNIX_EPOCH_DAY_NUMBER) * TICKS_PER_DAY)
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl Serialize for Date {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

// ---------------------------------------------------------------- RawDate

/// The three components of a `date_raw` field exactly as stored (02 §4.12).
///
/// Nothing is validated: the value may be partial (`2024-00-00`), impossible
/// (31 February) or, with a damaged year, negative. [`RawDate::date`] is the
/// conversion Banking4 applies when it needs a real date. A stored date of all
/// zeros is not a `RawDate`: the reader returns `None` for it.
///
/// Serialises as the stored components in the shape `YYYY-MM-DD`, which is ISO
/// 8601 text whenever the value is a real date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RawDate {
    /// The year as stored. Negative when the high bit of the 16-bit year is
    /// set, as in Banking4's packed representation.
    pub year: i16,
    /// The month as stored, 0 to 255.
    pub month: u8,
    /// The day as stored, 0 to 255.
    pub day: u8,
}

impl RawDate {
    /// Whether the components are a real calendar day of the years 1 to 9999.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.year >= 1 && Date::new(self.year as u16, self.month, self.day).is_some()
    }

    /// Whether this is 9999-12-31, which Banking4 reads as "end of time".
    #[must_use]
    pub const fn is_end_of_time(self) -> bool {
        self.year == 9999 && self.month == 12 && self.day == 31
    }

    /// The real date Banking4 makes of the stored components (02 §4.12): the
    /// year is clamped to 1..=9999, the month to 1..=12 and the day to the
    /// length of that month, so a year-only value becomes 1 January and
    /// 31 February the last day of February. A value that clamps to 0001-01-01
    /// is "no date" (02 §8 rule 9).
    #[must_use]
    pub const fn date(self) -> Option<Date> {
        let year = if self.year < 1 {
            1
        } else if self.year > 9999 {
            9999
        } else {
            self.year as u16
        };
        let month = if self.month < 1 {
            1
        } else if self.month > 12 {
            12
        } else {
            self.month
        };
        let last = days_in_month(year as i32, month);
        let day = if self.day < 1 {
            1
        } else if self.day > last {
            last
        } else {
            self.day
        };
        if year == 1 && month == 1 && day == 1 {
            None
        } else {
            Some(Date { year, month, day })
        }
    }

    /// The timestamp Banking4 makes of the stored components: midnight of
    /// [`RawDate::date`], except that 9999-12-31 becomes the last representable
    /// instant, [`Timestamp::MAX`].
    #[must_use]
    pub const fn timestamp(self) -> Option<Timestamp> {
        if self.is_end_of_time() {
            return Some(Timestamp::MAX);
        }
        match self.date() {
            Some(date) => Some(date.at_midnight()),
            None => None,
        }
    }
}

impl fmt::Display for RawDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl Serialize for RawDate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

// -------------------------------------------------------------- Timestamp

/// A point on the calendar with a resolution of 100 ns and **no time zone**,
/// decoded from a `ticks` field (02 §4.13).
///
/// Whether a field holds UTC or local wall-clock time is a property of the
/// field, not of the value (02 §9 question 1). "No timestamp" is not a
/// `Timestamp`: the reader returns `None` for a tick count of zero.
///
/// Serialises as `YYYY-MM-DDThh:mm:ss.fffffff`, ISO 8601 text with all seven
/// fractional digits and no zone suffix, which is also how the record fixtures
/// spell timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// 0001-01-01T00:00:00, Banking4's representation of "no date".
    pub const MIN: Timestamp = Timestamp(0);
    /// 9999-12-31T23:59:59.9999999, the last representable instant.
    pub const MAX: Timestamp = Timestamp(MAX_TICKS);

    /// The timestamp `ticks` units of 100 ns after 0001-01-01T00:00:00, or
    /// `None` if that is before the year 1 or after the year 9999.
    #[must_use]
    pub const fn from_ticks(ticks: i64) -> Option<Timestamp> {
        if ticks >= 0 && ticks <= MAX_TICKS {
            Some(Timestamp(ticks))
        } else {
            None
        }
    }

    /// Units of 100 ns since 0001-01-01T00:00:00.
    #[must_use]
    pub const fn ticks(self) -> i64 {
        self.0
    }

    /// Microseconds since 1970-01-01T00:00:00 on the same unzoned calendar,
    /// rounded down: the value of a DuckDB `TIMESTAMP`.
    #[must_use]
    pub const fn unix_micros(self) -> i64 {
        (self.0 - UNIX_EPOCH_TICKS).div_euclid(10)
    }

    /// Whole seconds since 1970-01-01T00:00:00, rounded down (02 §4.13).
    #[must_use]
    pub const fn unix_seconds(self) -> i64 {
        (self.0 - UNIX_EPOCH_TICKS).div_euclid(TICKS_PER_SECOND)
    }

    /// The calendar day.
    #[must_use]
    pub const fn date(self) -> Date {
        // 0 ..= 3652058 days from 0001-01-01, so the day count fits and the
        // year is within 1..=9999.
        let (year, month, day) =
            civil_from_days((self.0 / TICKS_PER_DAY - UNIX_EPOCH_DAY_NUMBER) as i32);
        Date {
            year: year as u16,
            month,
            day,
        }
    }

    /// The time of day, cut to whole seconds.
    #[must_use]
    pub const fn time(self) -> TimeOfDay {
        let seconds = self.0 % TICKS_PER_DAY / TICKS_PER_SECOND;
        TimeOfDay {
            hour: (seconds / 3600) as u8,
            minute: (seconds / 60 % 60) as u8,
            second: (seconds % 60) as u8,
        }
    }

    /// The fraction of the second, in units of 100 ns (0 to 9999999).
    #[must_use]
    pub const fn subsecond_ticks(self) -> u32 {
        (self.0 % TICKS_PER_SECOND) as u32
    }

    /// ISO 8601 text without a zone: `YYYY-MM-DDThh:mm:ss.fffffff`.
    #[must_use]
    pub fn to_iso(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}T{}.{:07}",
            self.date(),
            self.time(),
            self.subsecond_ticks()
        )
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

// -------------------------------------------------------------- TimeOfDay

/// A time of day with a resolution of one second, decoded from a `time_hms`
/// field (02 §4.14). Serialises as `hh:mm:ss`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimeOfDay {
    hour: u8,
    minute: u8,
    second: u8,
}

impl TimeOfDay {
    /// Midnight, 00:00:00.
    pub const MIDNIGHT: TimeOfDay = TimeOfDay {
        hour: 0,
        minute: 0,
        second: 0,
    };

    /// The time with these components, or `None` outside 00:00:00 to 23:59:59.
    #[must_use]
    pub const fn new(hour: u8, minute: u8, second: u8) -> Option<TimeOfDay> {
        if hour <= 23 && minute <= 59 && second <= 59 {
            Some(TimeOfDay {
                hour,
                minute,
                second,
            })
        } else {
            None
        }
    }

    /// The hour, 0 to 23.
    #[must_use]
    pub const fn hour(self) -> u8 {
        self.hour
    }

    /// The minute, 0 to 59.
    #[must_use]
    pub const fn minute(self) -> u8 {
        self.minute
    }

    /// The second, 0 to 59.
    #[must_use]
    pub const fn second(self) -> u8 {
        self.second
    }

    /// Seconds since midnight, 0 to 86399.
    #[must_use]
    pub const fn seconds_since_midnight(self) -> u32 {
        self.hour as u32 * 3600 + self.minute as u32 * 60 + self.second as u32
    }

    /// Microseconds since midnight: the value of a DuckDB `TIME`.
    #[must_use]
    pub const fn micros_since_midnight(self) -> i64 {
        self.seconds_since_midnight() as i64 * 1_000_000
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }
}

impl Serialize for TimeOfDay {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

// ------------------------------------------------------------------- Guid

/// A GUID, decoded from a `guid` field (02 §4.11).
///
/// The all-zero GUID is how "no GUID" is stored. Serialises as the usual
/// lower-case text, `00112233-4455-6677-8899-aabbccddeeff`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Guid([u8; 16]);

impl Guid {
    /// The all-zero GUID.
    pub const NIL: Guid = Guid([0; 16]);

    /// The GUID stored as these 16 bytes: the first three groups little-endian,
    /// the last two in display order.
    #[must_use]
    pub const fn from_wire_bytes(bytes: [u8; 16]) -> Guid {
        Guid(bytes)
    }

    /// The 16 bytes as stored.
    #[must_use]
    pub const fn to_wire_bytes(self) -> [u8; 16] {
        self.0
    }

    /// Whether this is the all-zero GUID, "no GUID".
    #[must_use]
    pub fn is_nil(self) -> bool {
        self.0 == [0; 16]
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Groups one to three are stored little-endian (02 §4.11).
        let [a3, a2, a1, a0, b1, b0, c1, c0, d0, d1, tail @ ..] = self.0;
        write!(
            f,
            "{a0:02x}{a1:02x}{a2:02x}{a3:02x}-{b0:02x}{b1:02x}-{c0:02x}{c1:02x}-{d0:02x}{d1:02x}-"
        )?;
        tail.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Guid({self})")
    }
}

impl Serialize for Guid {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

// ------------------------------------------------------------------ Stamp

/// Whole seconds since 2000-01-01T00:00:00 **UTC** (02 §5.1): the unit of
/// object ids, update stamps and the container's own time stamps.
///
/// An object id read as a stamp is the creation time only approximately; ids
/// run ahead of the clock when many objects are created within one second.
/// Serialises as ISO 8601 text with a `Z` suffix, `YYYY-MM-DDThh:mm:ssZ`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp(i32);

impl Stamp {
    /// 2000-01-01T00:00:00Z.
    pub const EPOCH: Stamp = Stamp(0);

    /// The stamp with this stored value. Negative values are times before
    /// 2000 and are valid.
    #[must_use]
    pub const fn new(raw: i32) -> Stamp {
        Stamp(raw)
    }

    /// The stored value.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }

    /// Seconds since 1970-01-01T00:00:00Z.
    #[must_use]
    pub const fn unix_seconds(self) -> i64 {
        self.0 as i64 + STAMP_EPOCH_UNIX_SECONDS
    }

    /// Microseconds since 1970-01-01T00:00:00Z: the value of a DuckDB
    /// `TIMESTAMP`.
    #[must_use]
    pub const fn unix_micros(self) -> i64 {
        self.unix_seconds() * 1_000_000
    }

    /// The same instant as a [`Timestamp`] on the UTC calendar.
    #[must_use]
    pub const fn timestamp(self) -> Timestamp {
        // Between 1931 and 2068 for every `i32`, so always in range.
        Timestamp(self.0 as i64 * TICKS_PER_SECOND + STAMP_EPOCH_TICKS)
    }

    /// ISO 8601 text in UTC: `YYYY-MM-DDThh:mm:ssZ`.
    #[must_use]
    pub fn to_iso(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let timestamp = self.timestamp();
        write!(f, "{}T{}Z", timestamp.date(), timestamp.time())
    }
}

impl Serialize for Stamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

// ---------------------------------------------------------------- decimal

/// Serialise a decimal as text with its scale intact (`"1.10"` stays
/// `"1.10"`), for `#[serde(serialize_with = "crate::wire::serialize_decimal")]`.
///
/// # Errors
///
/// Only those of the serializer.
pub fn serialize_decimal<S: Serializer>(value: &Decimal, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

/// Like [`serialize_decimal`] for an optional decimal; `None` becomes null.
///
/// # Errors
///
/// Only those of the serializer.
pub fn serialize_optional_decimal<S: Serializer>(
    value: &Option<Decimal>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => serializer.collect_str(value),
        None => serializer.serialize_none(),
    }
}

// ------------------------------------------------------------------- text

/// The lines of a multi-line text stored in one `string` (02 §5.2): split at
/// CR, LF or CR LF, empty lines dropped, nothing trimmed. `None` (null) has no
/// lines.
#[must_use]
pub fn text_lines(text: Option<&str>) -> Vec<&str> {
    text.map(|text| {
        text.split(['\r', '\n'])
            .filter(|line| !line.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_day_numbers_match_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
        assert_eq!(days_from_civil(1, 1, 1), -UNIX_EPOCH_DAY_NUMBER);
        assert_eq!(days_from_civil(9999, 12, 31), 2_932_896);
    }

    #[test]
    fn civil_conversion_round_trips_over_every_day_of_the_range() {
        let first = Date::MIN.days_since_unix_epoch();
        let last = Date::MAX.days_since_unix_epoch();
        let (mut year, mut month, mut day) = (1_i32, 1_u8, 1_u8);
        for days in first..=last {
            assert_eq!(civil_from_days(days), (year, month, day));
            assert_eq!(days_from_civil(year, month, day), i64::from(days));
            day += 1;
            if day > days_in_month(year, month) {
                day = 1;
                month += 1;
                if month > 12 {
                    month = 1;
                    year += 1;
                }
            }
        }
        assert_eq!((year, month, day), (10_000, 1, 1));
    }

    #[test]
    fn civil_conversion_survives_the_extremes_of_its_argument_types() {
        for days in [i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), i64::from(days));
        }
        // Unvalidated arguments must not overflow either.
        let _ = days_from_civil(i32::MIN, 0, 0);
        let _ = days_from_civil(i32::MAX, 255, 255);
    }

    #[test]
    fn leap_years_follow_the_gregorian_rule() {
        assert!(is_leap_year(2024) && is_leap_year(2000));
        assert!(!is_leap_year(2023) && !is_leap_year(1900));
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2024, 0), 0);
        assert_eq!(days_in_month(2024, 13), 0);
    }

    #[test]
    fn date_accepts_real_days_only() {
        assert!(Date::new(2024, 2, 29).is_some());
        assert!(Date::new(2023, 2, 29).is_none());
        assert!(Date::new(0, 1, 1).is_none());
        assert!(Date::new(10_000, 1, 1).is_none());
        assert!(Date::new(2024, 0, 1).is_none());
        assert!(Date::new(2024, 13, 1).is_none());
        assert!(Date::new(2024, 4, 31).is_none());
        assert_eq!(Date::new(1, 1, 1), Some(Date::MIN));
        assert_eq!(Date::new(9999, 12, 31), Some(Date::MAX));
    }

    #[test]
    fn date_converts_to_day_numbers_and_back() {
        let date = Date::new(2024, 2, 29).unwrap();
        assert_eq!(date.days_since_unix_epoch(), 19_782);
        assert_eq!(Date::from_days_since_unix_epoch(19_782), Some(date));
        assert_eq!(Date::from_days_since_unix_epoch(-719_162), Some(Date::MIN));
        assert_eq!(Date::from_days_since_unix_epoch(-719_163), None);
        assert_eq!(Date::from_days_since_unix_epoch(2_932_896), Some(Date::MAX));
        assert_eq!(Date::from_days_since_unix_epoch(2_932_897), None);
        assert_eq!(date.to_string(), "2024-02-29");
        assert_eq!(Date::MIN.to_string(), "0001-01-01");
    }

    #[test]
    fn timestamp_splits_into_date_time_and_fraction() {
        // The example of 02 §4.13.
        let timestamp = Timestamp::from_ticks(638_448_111_300_000_000).unwrap();
        assert_eq!(timestamp.date(), Date::new(2024, 2, 29).unwrap());
        assert_eq!(timestamp.time(), TimeOfDay::new(13, 45, 30).unwrap());
        assert_eq!(timestamp.subsecond_ticks(), 0);
        assert_eq!(timestamp.to_iso(), "2024-02-29T13:45:30.0000000");
        assert_eq!(timestamp.unix_seconds(), 1_709_214_330);
        assert_eq!(timestamp.unix_micros(), 1_709_214_330_000_000);
    }

    #[test]
    fn timestamp_bounds_are_the_years_1_and_9999() {
        assert_eq!(Timestamp::from_ticks(0), Some(Timestamp::MIN));
        assert_eq!(Timestamp::from_ticks(-1), None);
        assert_eq!(Timestamp::from_ticks(MAX_TICKS + 1), None);
        assert_eq!(Timestamp::MIN.to_iso(), "0001-01-01T00:00:00.0000000");
        assert_eq!(Timestamp::MAX.to_iso(), "9999-12-31T23:59:59.9999999");
        assert_eq!(Timestamp::MAX.date(), Date::MAX);
    }

    #[test]
    fn timestamp_before_1970_rounds_micros_down() {
        // One tick before the Unix epoch is in the microsecond before it.
        let timestamp = Timestamp::from_ticks(UNIX_EPOCH_TICKS - 1).unwrap();
        assert_eq!(timestamp.unix_micros(), -1);
        assert_eq!(timestamp.unix_seconds(), -1);
        assert_eq!(
            Timestamp::from_ticks(UNIX_EPOCH_TICKS)
                .unwrap()
                .unix_micros(),
            0
        );
        assert_eq!(
            Timestamp::from_ticks(UNIX_EPOCH_TICKS + 19)
                .unwrap()
                .unix_micros(),
            1
        );
        assert_eq!(Timestamp::MIN.unix_micros(), -62_135_596_800_000_000);
        assert_eq!(Timestamp::MAX.unix_micros(), 253_402_300_799_999_999);
    }

    #[test]
    fn midnight_of_a_date_is_a_whole_number_of_days() {
        let date = Date::new(2024, 2, 29).unwrap();
        assert_eq!(date.at_midnight().ticks(), 638_447_616_000_000_000);
        assert_eq!(date.at_midnight().date(), date);
        assert_eq!(Date::MIN.at_midnight(), Timestamp::MIN);
    }

    #[test]
    fn raw_date_converts_like_banking4() {
        let raw = |year, month, day| RawDate { year, month, day };
        assert_eq!(raw(2024, 2, 29).date(), Date::new(2024, 2, 29));
        // Partial and impossible values are clamped.
        assert_eq!(raw(2024, 0, 0).date(), Date::new(2024, 1, 1));
        assert_eq!(raw(2023, 2, 31).date(), Date::new(2023, 2, 28));
        assert_eq!(raw(2024, 13, 40).date(), Date::new(2024, 12, 31));
        assert_eq!(raw(32_767, 255, 255).date(), Some(Date::MAX));
        assert_eq!(raw(-1, 255, 255).date(), Date::new(1, 12, 31));
        // Everything that clamps to 0001-01-01 is "no date".
        assert_eq!(raw(1, 1, 1).date(), None);
        assert_eq!(raw(0, 0, 1).date(), None);
        assert_eq!(raw(-1, 1, 1).date(), None);
        // 9999-12-31 is the end of time as a timestamp, a plain day as a date.
        assert_eq!(raw(9999, 12, 31).timestamp(), Some(Timestamp::MAX));
        assert_eq!(raw(9999, 12, 31).date(), Some(Date::MAX));
        assert_eq!(
            raw(9999, 12, 30).timestamp(),
            Date::new(9999, 12, 30).map(Date::at_midnight)
        );
        assert!(raw(2024, 2, 29).is_valid());
        assert!(!raw(2024, 2, 30).is_valid());
        assert!(!raw(2024, 0, 0).is_valid());
        assert!(!raw(-1, 1, 1).is_valid());
        assert_eq!(raw(2024, 2, 29).to_string(), "2024-02-29");
        assert_eq!(raw(2024, 0, 0).to_string(), "2024-00-00");
    }

    #[test]
    fn time_of_day_is_validated() {
        assert_eq!(TimeOfDay::new(0, 0, 0), Some(TimeOfDay::MIDNIGHT));
        assert!(TimeOfDay::new(24, 0, 0).is_none());
        assert!(TimeOfDay::new(23, 60, 0).is_none());
        assert!(TimeOfDay::new(23, 59, 60).is_none());
        let time = TimeOfDay::new(13, 45, 30).unwrap();
        assert_eq!(time.to_string(), "13:45:30");
        assert_eq!(time.seconds_since_midnight(), 49_530);
        assert_eq!(time.micros_since_midnight(), 49_530_000_000);
    }

    #[test]
    fn guid_text_uses_the_mixed_endian_layout() {
        // The example of 02 §4.11.
        let guid = Guid::from_wire_bytes([
            0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ]);
        assert_eq!(guid.to_string(), "00112233-4455-6677-8899-aabbccddeeff");
        assert!(!guid.is_nil());
        assert!(Guid::NIL.is_nil());
        assert_eq!(Guid::default(), Guid::NIL);
        assert_eq!(
            Guid::NIL.to_string(),
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(
            format!("{guid:?}"),
            "Guid(00112233-4455-6677-8899-aabbccddeeff)"
        );
    }

    #[test]
    fn stamp_counts_seconds_from_2000_utc() {
        // The example of 02 §5.1.
        let stamp = Stamp::new(762_529_530);
        assert_eq!(stamp.unix_seconds(), 1_709_214_330);
        assert_eq!(stamp.unix_micros(), 1_709_214_330_000_000);
        assert_eq!(stamp.to_iso(), "2024-02-29T13:45:30Z");
        assert_eq!(stamp.timestamp().ticks(), 638_448_111_300_000_000);
        assert_eq!(Stamp::EPOCH.to_iso(), "2000-01-01T00:00:00Z");
        assert_eq!(Stamp::new(-1).to_iso(), "1999-12-31T23:59:59Z");
        assert_eq!(Stamp::new(i32::MAX).to_iso(), "2068-01-19T03:14:07Z");
        assert_eq!(Stamp::new(i32::MIN).to_iso(), "1931-12-13T20:45:52Z");
    }

    #[test]
    fn values_serialise_as_text() {
        #[derive(Serialize)]
        struct All {
            date: Date,
            raw: RawDate,
            timestamp: Timestamp,
            time: TimeOfDay,
            guid: Guid,
            stamp: Stamp,
            none: Option<Date>,
            #[serde(serialize_with = "serialize_decimal")]
            amount: Decimal,
            #[serde(serialize_with = "serialize_optional_decimal")]
            rate: Option<Decimal>,
            #[serde(serialize_with = "serialize_optional_decimal")]
            no_rate: Option<Decimal>,
        }
        let all = All {
            date: Date::new(2024, 2, 29).unwrap(),
            raw: RawDate {
                year: 2011,
                month: 1,
                day: 1,
            },
            timestamp: Timestamp::from_ticks(638_448_111_300_000_001).unwrap(),
            time: TimeOfDay::new(10, 0, 11).unwrap(),
            guid: Guid::NIL,
            stamp: Stamp::new(762_529_530),
            none: None,
            amount: Decimal::from_parts(110, 0, 0, false, 2),
            rate: Some(Decimal::from_parts(282_800, 0, 0, true, 4)),
            no_rate: None,
        };
        assert_eq!(
            serde_json::to_string(&all).unwrap(),
            concat!(
                r#"{"date":"2024-02-29","raw":"2011-01-01","#,
                r#""timestamp":"2024-02-29T13:45:30.0000001","time":"10:00:11","#,
                r#""guid":"00000000-0000-0000-0000-000000000000","#,
                r#""stamp":"2024-02-29T13:45:30Z","none":null,"#,
                r#""amount":"1.10","rate":"-28.2800","no_rate":null}"#
            )
        );
    }

    #[test]
    fn text_lines_split_at_cr_and_lf_and_drop_empty_lines() {
        assert_eq!(text_lines(None), Vec::<&str>::new());
        assert_eq!(text_lines(Some("one")), ["one"]);
        assert_eq!(
            text_lines(Some("one\ntwo\r\nthree\rfour")),
            ["one", "two", "three", "four"]
        );
        assert_eq!(text_lines(Some("\n\none\n\n\ntwo\n")), ["one", "two"]);
        // Lines are not trimmed, and only CR and LF separate them.
        assert_eq!(
            text_lines(Some("  \n\u{85}a\u{2028}b\u{b}")),
            ["  ", "\u{85}a\u{2028}b\u{b}"]
        );
    }
}
