//! The retention period: how long the application keeps bookings, payments
//! and documents of an account (03 "Retention period").
//!
//! The value has two wire forms, a number of days and a short text, and the
//! account uses both. `b4` only decodes the period. It never derives a purge
//! date from it, so the one failure Banking4 has there (a period reaching
//! back before the year 1) does not exist here.

use serde::Serialize;

use crate::model::base::code_enum;

code_enum! {
    /// The unit a [`RetentionPeriod`] counts in (03 "Retention unit").
    pub enum RetentionUnit: u8 {
        /// Days.
        Days = 0 => "days",
        /// Whole calendar months.
        CalendarMonths = 1 => "calendar_months",
        /// Whole calendar years.
        CalendarYears = 2 => "calendar_years",
    }
}

/// How long something is kept: a count of days, calendar months or calendar
/// years (03 "Retention period").
///
/// Two counts are special: 0 is "not set" and 2147483647 is "unlimited".
/// Every other count is kept as read, negative ones included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct RetentionPeriod {
    /// What `count` counts.
    pub unit: RetentionUnit,
    /// How many of them.
    pub count: i32,
}

impl RetentionPeriod {
    /// The count that means "keep for ever".
    pub const UNLIMITED_COUNT: i32 = i32::MAX;

    /// No period was chosen.
    pub const NOT_SET: Self = Self::new(RetentionUnit::Days, 0);

    /// Keep for ever.
    pub const UNLIMITED: Self = Self::new(RetentionUnit::Days, Self::UNLIMITED_COUNT);

    /// The preset periods of the day form with their length in days, shortest
    /// first (03 "Day form").
    const PRESETS: [(i32, Self); 7] = [
        (30, Self::new(RetentionUnit::CalendarMonths, 1)),
        (90, Self::new(RetentionUnit::CalendarMonths, 3)),
        (180, Self::new(RetentionUnit::CalendarMonths, 6)),
        (365, Self::new(RetentionUnit::CalendarYears, 1)),
        (730, Self::new(RetentionUnit::CalendarYears, 2)),
        (2190, Self::new(RetentionUnit::CalendarYears, 6)),
        (3650, Self::new(RetentionUnit::CalendarYears, 10)),
    ];

    /// A period of `count` units.
    #[must_use]
    pub const fn new(unit: RetentionUnit, count: i32) -> Self {
        Self { unit, count }
    }

    /// Whether no period was chosen (a count of 0).
    #[must_use]
    pub const fn is_not_set(&self) -> bool {
        self.count == 0
    }

    /// Whether the period is "keep for ever".
    #[must_use]
    pub const fn is_unlimited(&self) -> bool {
        self.count == Self::UNLIMITED_COUNT
    }

    /// Decode the day form (03 "Day form"): 0 is "not set", any other number
    /// selects the shortest preset that is at least that long, and a number
    /// above the longest preset is "unlimited". Every negative number
    /// therefore gives one month.
    ///
    /// The form is lossy by design: the writer stores a length in days and
    /// only the presets come back as they were.
    #[must_use]
    pub fn from_days(days: i32) -> Self {
        if days == 0 {
            return Self::NOT_SET;
        }
        Self::PRESETS
            .iter()
            .find(|(length, _)| *length >= days)
            .map_or(Self::UNLIMITED, |(_, period)| *period)
    }

    /// Decode the text form (03 "Text form"). No text is "unlimited".
    /// Otherwise the first character selects the unit (`D`, `M`, `Y`, anything
    /// else days) and the rest is the count: a signed 32-bit decimal number,
    /// optionally surrounded by white space and followed by NUL characters.
    ///
    /// Returns `None` for a text that is not of that shape. Banking4 rejects
    /// such a text and loses the whole account file with it; a caller here
    /// keeps the text and carries on (03 open question 12).
    #[must_use]
    pub fn from_text(text: Option<&str>) -> Option<Self> {
        let Some(text) = text.filter(|text| !text.is_empty()) else {
            return Some(Self::UNLIMITED);
        };
        let mut characters = text.chars();
        let first = characters.next()?;
        // Banking4 takes the first UTF-16 unit as the unit letter. For a
        // character outside the basic plane the second unit would then start
        // the count, which cannot parse.
        if first.len_utf16() != 1 {
            return None;
        }
        let unit = match first {
            'M' => RetentionUnit::CalendarMonths,
            'Y' => RetentionUnit::CalendarYears,
            _ => RetentionUnit::Days,
        };
        parse_count(characters.as_str()).map(|count| Self::new(unit, count))
    }
}

/// The white space the count of the text form may be surrounded by: `09` to
/// `0D` and the blank (03 "Text form"). Narrower than Unicode white space, so
/// `u8::is_ascii_whitespace` and `str::trim` are not used here.
const fn is_count_space(byte: u8) -> bool {
    matches!(byte, 0x09..=0x0D | b' ')
}

/// Parse the count of the text form: white space, an optional sign, at least
/// one ASCII digit, white space, NUL characters, and nothing else. `None` if
/// the text has another shape or the number does not fit 32 bits.
fn parse_count(text: &str) -> Option<i32> {
    let mut rest = text.as_bytes();
    while let [byte, tail @ ..] = rest {
        if !is_count_space(*byte) {
            break;
        }
        rest = tail;
    }
    let negative = match rest {
        [b'-', tail @ ..] => {
            rest = tail;
            true
        }
        [b'+', tail @ ..] => {
            rest = tail;
            false
        }
        _ => false,
    };
    // One past the largest magnitude: the negative range reaches one further.
    let limit = i64::from(i32::MAX) + 1;
    let mut magnitude: i64 = 0;
    let mut digits = 0_usize;
    while let [byte @ b'0'..=b'9', tail @ ..] = rest {
        magnitude = magnitude * 10 + i64::from(*byte - b'0');
        // Checked on every digit, so the accumulator stays far from the
        // limits of an i64 however many digits a hostile text has.
        if magnitude > limit {
            return None;
        }
        digits += 1;
        rest = tail;
    }
    if digits == 0 {
        return None;
    }
    while let [byte, tail @ ..] = rest {
        if !is_count_space(*byte) {
            break;
        }
        rest = tail;
    }
    if rest.iter().any(|byte| *byte != 0) {
        return None;
    }
    i32::try_from(if negative { -magnitude } else { magnitude }).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn months(count: i32) -> RetentionPeriod {
        RetentionPeriod::new(RetentionUnit::CalendarMonths, count)
    }

    const fn years(count: i32) -> RetentionPeriod {
        RetentionPeriod::new(RetentionUnit::CalendarYears, count)
    }

    const fn days(count: i32) -> RetentionPeriod {
        RetentionPeriod::new(RetentionUnit::Days, count)
    }

    #[test]
    fn day_form_selects_the_first_preset_that_is_long_enough() {
        let cases = [
            (0, RetentionPeriod::NOT_SET),
            (1, months(1)),
            (30, months(1)),
            (31, months(3)),
            (45, months(3)),
            (90, months(3)),
            (91, months(6)),
            (180, months(6)),
            (181, years(1)),
            (365, years(1)),
            (366, years(2)),
            (730, years(2)),
            (731, years(6)),
            (2190, years(6)),
            (2191, years(10)),
            (3650, years(10)),
            (3651, RetentionPeriod::UNLIMITED),
            (36500, RetentionPeriod::UNLIMITED),
            (i32::MAX, RetentionPeriod::UNLIMITED),
            // "Every negative number therefore gives 1 month."
            (-1, months(1)),
            (i32::MIN, months(1)),
        ];
        for (stored, expected) in cases {
            assert_eq!(
                RetentionPeriod::from_days(stored),
                expected,
                "{stored} days"
            );
        }
    }

    #[test]
    fn day_form_special_values_read_back_in_days() {
        assert_eq!(RetentionPeriod::from_days(0).unit, RetentionUnit::Days);
        assert!(RetentionPeriod::from_days(0).is_not_set());
        assert_eq!(RetentionPeriod::from_days(4000).unit, RetentionUnit::Days);
        assert!(RetentionPeriod::from_days(4000).is_unlimited());
    }

    #[test]
    fn text_form_accepts_what_the_document_lists() {
        let cases = [
            // No text is unlimited.
            (None, RetentionPeriod::UNLIMITED),
            (Some(""), RetentionPeriod::UNLIMITED),
            // The unit letters, case sensitive; anything else is days.
            (Some("D90"), days(90)),
            (Some("M6"), months(6)),
            (Some("Y10"), years(10)),
            (Some("m5"), days(5)),
            (Some("?7"), days(7)),
            (Some("55"), days(5)),
            // "Not set" keeps its unit.
            (Some("D0"), days(0)),
            (Some("M0"), months(0)),
            (Some("Y0"), years(0)),
            // What the integer parser accepts.
            (Some("D 5"), days(5)),
            (Some("D+5"), days(5)),
            (Some("D05"), days(5)),
            (Some("D-1"), days(-1)),
            (Some("D\t\n\x0B\x0C\r 5 \t"), days(5)),
            (Some("D5\0\0"), days(5)),
            (Some("D5 \0"), days(5)),
            (Some("D000000000000000000000000000005"), days(5)),
            (Some("D-0"), days(0)),
            // Not range-checked beyond 32 bits.
            (Some("Y3000"), years(3000)),
            (Some("D36500"), days(36500)),
            (Some("D2147483647"), RetentionPeriod::UNLIMITED),
            (Some("M2147483647"), months(i32::MAX)),
            (Some("D-2147483648"), days(i32::MIN)),
        ];
        for (text, expected) in cases {
            assert_eq!(RetentionPeriod::from_text(text), Some(expected), "{text:?}");
        }
    }

    #[test]
    fn text_form_refuses_what_banking4_rejects() {
        let cases = [
            // No count at all.
            "D",
            "5",
            "D ",
            "D+",
            "D-",
            // A unit letter after white space.
            " D5",
            // Any other character.
            "D5x",
            "D1,5",
            "D5.0",
            "D5 5",
            "D+-5",
            "D\0",
            "D\x005",
            "D5\0 ",
            // White space the integer parser does not skip.
            "D\u{A0}5",
            "D\u{85}5",
            "D\u{2003}5",
            "D\x1C5",
            // Digits that are not ASCII.
            "D\u{0665}",
            "D\u{FF15}",
            // Outside 32 bits.
            "D2147483648",
            "D-2147483649",
            "D99999999999999999999999999999999999999",
            // A first character of two UTF-16 units leaves half of it in
            // front of the count.
            "\u{1F600}5",
        ];
        for text in cases {
            assert_eq!(RetentionPeriod::from_text(Some(text)), None, "{text:?}");
        }
    }

    #[test]
    fn serialises_as_unit_name_and_count() {
        assert_eq!(
            serde_json::to_string(&months(6)).unwrap(),
            r#"{"unit":"calendar_months","count":6}"#
        );
    }
}
