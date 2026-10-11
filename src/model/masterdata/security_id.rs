//! Security identifiers (05 §5.1): the text by which stock instruments,
//! portfolio items and portfolio transactions name a security.

use serde::Serialize;

/// Length of an ISIN.
const ISIN_LENGTH: usize = 12;
/// Length of a German national securities number.
const GERMAN_NUMBER_LENGTH: usize = 6;
/// The country whose national numbers have a canonical ISIN.
const GERMANY: &str = "DE";

/// What kind of identifier a text is (05 §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityIdKind {
    /// No text, or white space only.
    #[default]
    None,
    /// A text that is none of the forms below. Banking4 treats it like no
    /// identifier; it is told apart here so that an unparsed text shows.
    Unrecognised,
    /// A valid ISIN.
    Isin,
    /// A national securities number with its country.
    National,
    /// A ticker symbol (`/TS/...`).
    Ticker,
    /// A bilaterally agreed identifier (`/XX/...`).
    Bilateral,
}

impl SecurityIdKind {
    /// The lower-case name, as it is serialised.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Unrecognised => "unrecognised",
            Self::Isin => "isin",
            Self::National => "national",
            Self::Ticker => "ticker",
            Self::Bilateral => "bilateral",
        }
    }

    /// Whether Banking4 sees an identifier in the text at all.
    #[must_use]
    pub const fn is_identifier(self) -> bool {
        !matches!(self, Self::None | Self::Unrecognised)
    }
}

/// A security identifier text, interpreted (05 §5.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize)]
pub struct SecurityId {
    /// The form of the text.
    pub kind: SecurityIdKind,
    /// For a national number: its two-letter country code.
    pub country: Option<String>,
    /// For a national number: the number part, as written.
    pub number: Option<String>,
    /// The ISIN the identifier stands for: the ISIN itself, or the one
    /// derived from a German national number of six characters. This is
    /// what links a portfolio item to its stock instrument (05 §5.2).
    pub canonical_isin: Option<String>,
    /// A German national number of six characters whose ISIN cannot be
    /// derived because a character is neither a letter `A` to `Z`, `a` to
    /// `z` nor a digit. Banking4 fails when it needs the ISIN of such an
    /// identifier; here it simply has none.
    pub derivation_failed: bool,
}

impl SecurityId {
    fn of_kind(kind: SecurityIdKind) -> Self {
        Self {
            kind,
            ..Self::default()
        }
    }

    fn isin(isin: &str) -> Self {
        Self {
            canonical_isin: Some(isin.to_owned()),
            ..Self::of_kind(SecurityIdKind::Isin)
        }
    }

    /// A national number. Only its length decides whether an ISIN is
    /// derived; the characters are not validated first (05 §5.1).
    fn national(country: &str, number: &str) -> Self {
        // "Six characters" is six UTF-16 units in Banking4.
        let derivable = country == GERMANY && number.encode_utf16().count() == GERMAN_NUMBER_LENGTH;
        let canonical_isin = derivable
            .then(|| {
                let stem = format!("{GERMANY}000{number}");
                let check = isin_check_digit(&stem)?;
                Some(format!("{stem}{check}"))
            })
            .flatten();
        Self {
            derivation_failed: derivable && canonical_isin.is_none(),
            canonical_isin,
            country: Some(country.to_owned()),
            number: Some(number.to_owned()),
            ..Self::of_kind(SecurityIdKind::National)
        }
    }
}

/// The value of one character in the digit string the check digit is
/// computed over: a digit is itself, a letter is 10 for `A` up to 35 for
/// `Z`. Lower-case letters count like upper-case ones (05 §5.1).
fn expansion(c: char) -> Option<u32> {
    match c {
        '0'..='9' | 'A'..='Z' | 'a'..='z' => c.to_digit(36),
        _ => None,
    }
}

/// The check digit of an ISIN, computed over its first eleven characters
/// `stem` (05 §5.1): every letter is replaced by its two-digit value, then,
/// going from the last digit to the first, every second digit starting with
/// the last one is doubled; the decimal digits of all results are added up
/// and the check digit is what is missing to the next multiple of ten.
///
/// `None` if `stem` contains a character that is neither a letter `A` to
/// `Z`, `a` to `z` nor a digit. The length of `stem` is not checked.
#[must_use]
pub fn isin_check_digit(stem: &str) -> Option<char> {
    let mut digits: Vec<u32> = Vec::with_capacity(stem.len().saturating_mul(2));
    for c in stem.chars() {
        let value = expansion(c)?;
        if value >= 10 {
            digits.push(value / 10);
        }
        digits.push(value % 10);
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(index, &digit)| {
            let value = if index % 2 == 0 { digit * 2 } else { digit };
            value / 10 + value % 10
        })
        // Each term is at most 9: the sum cannot overflow for any text that
        // fits in memory, and saturating keeps it that way regardless.
        .fold(0, u32::saturating_add);
    char::from_digit((10 - sum % 10) % 10, 10)
}

/// Whether `text` is a valid ISIN (05 §5.1): twelve characters, two
/// upper-case letters, nine upper-case letters or digits, one digit, and the
/// right check digit. A lower-case spelling is not an ISIN.
#[must_use]
pub fn is_valid_isin(text: &str) -> bool {
    let bytes = text.as_bytes();
    let [stem @ .., check] = bytes else {
        return false;
    };
    bytes.len() == ISIN_LENGTH
        && stem
            .iter()
            .enumerate()
            .all(|(index, byte)| byte.is_ascii_uppercase() || (index >= 2 && byte.is_ascii_digit()))
        && check.is_ascii_digit()
        // All ASCII at this point, so the cut is between characters.
        && text.get(..ISIN_LENGTH - 1).and_then(isin_check_digit) == Some(char::from(*check))
}

/// Interpret a security identifier text the way Banking4 does (05 §5.1).
/// Every test is case-sensitive, and no text makes this fail.
///
/// 1. White space at both ends is removed; nothing left is no identifier.
/// 2. A leading `ISIN` and one space are removed, whatever follows.
/// 3. The first form that fits decides: a valid ISIN; `WKN` or `WKNR` and
///    one space (a German national number); exactly six upper-case letters
///    or digits (the same); `/TS/` (ticker symbol); `/XX/` (bilateral
///    identifier); `/` two upper-case letters `/` and at least one more
///    character (a national number of that country). Anything else is
///    [`SecurityIdKind::Unrecognised`].
#[must_use]
pub fn parse_security_id(text: Option<&str>) -> SecurityId {
    let text = text.map_or("", str::trim);
    if text.is_empty() {
        return SecurityId::of_kind(SecurityIdKind::None);
    }
    let text = text.strip_prefix("ISIN ").unwrap_or(text);
    if is_valid_isin(text) {
        return SecurityId::isin(text);
    }
    if let Some(number) = text.strip_prefix("WKN ") {
        return SecurityId::national(GERMANY, number);
    }
    if let Some(number) = text.strip_prefix("WKNR ") {
        return SecurityId::national(GERMANY, number);
    }
    let bytes = text.as_bytes();
    if bytes.len() == GERMAN_NUMBER_LENGTH
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return SecurityId::national(GERMANY, text);
    }
    if text.starts_with("/TS/") {
        return SecurityId::of_kind(SecurityIdKind::Ticker);
    }
    if text.starts_with("/XX/") {
        return SecurityId::of_kind(SecurityIdKind::Bilateral);
    }
    // The four leading bytes are ASCII here, so cutting after them cannot
    // split a character.
    if let [b'/', first, second, b'/', _, ..] = bytes {
        if first.is_ascii_uppercase() && second.is_ascii_uppercase() {
            if let (Some(country), Some(number)) = (text.get(1..3), text.get(4..)) {
                return SecurityId::national(country, number);
            }
        }
    }
    SecurityId::of_kind(SecurityIdKind::Unrecognised)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kind, canonical ISIN and whether the derivation failed.
    fn parse(text: &str) -> (SecurityIdKind, Option<String>, bool) {
        let id = parse_security_id(Some(text));
        (id.kind, id.canonical_isin, id.derivation_failed)
    }

    fn with_isin(kind: SecurityIdKind, isin: &str) -> (SecurityIdKind, Option<String>, bool) {
        (kind, Some(isin.to_owned()), false)
    }

    fn without_isin(kind: SecurityIdKind) -> (SecurityIdKind, Option<String>, bool) {
        (kind, None, false)
    }

    #[test]
    fn check_digit_follows_the_luhn_rule_over_expanded_letters() {
        // Real-world ISINs of public companies.
        assert_eq!(isin_check_digit("US037833100"), Some('5'));
        assert_eq!(isin_check_digit("DE000BAY001"), Some('7'));
        assert_eq!(isin_check_digit("GB000263494"), Some('6'));
        assert_eq!(isin_check_digit("AU0000XVGZA"), Some('3'));
        // The document's example.
        assert_eq!(isin_check_digit("DE000123456"), Some('5'));
        // Lower-case letters count like upper-case ones.
        assert_eq!(isin_check_digit("DE000abcdef"), Some('2'));
        assert_eq!(isin_check_digit("DE000ABCDEF"), Some('2'));
        for bad in ["DE000 23456", "DE000-23456", "DE000ä23456", "DE0001234é6"] {
            assert_eq!(isin_check_digit(bad), None, "{bad}");
        }
        assert_eq!(isin_check_digit(""), Some('0'));
    }

    #[test]
    fn isin_needs_the_right_shape_and_check_digit() {
        for valid in [
            "US0378331005",
            "DE000BAY0017",
            "GB0002634946",
            "DE0001234565",
        ] {
            assert!(is_valid_isin(valid), "{valid}");
        }
        for invalid in [
            "",
            "US0378331006",
            "us0378331005",
            "US037833100",
            "US03783310055",
            "U10378331005",
            "1S0378331005",
            "US037833100X",
            "US03783310 5",
            "DE000abcdef2",
            "ÜS0378331005",
            "US037833100５",
        ] {
            assert!(!is_valid_isin(invalid), "{invalid}");
        }
    }

    #[test]
    fn every_form_of_the_table_is_recognised() {
        use SecurityIdKind::{Bilateral, Isin, National, None, Ticker, Unrecognised};
        assert_eq!(parse("US0378331005"), with_isin(Isin, "US0378331005"));
        assert_eq!(parse("  US0378331005\t"), with_isin(Isin, "US0378331005"));
        assert_eq!(parse("ISIN US0378331005"), with_isin(Isin, "US0378331005"));
        // The document's example: three spellings of one number.
        for text in ["123456", "WKN 123456", "WKNR 123456", "/DE/123456"] {
            assert_eq!(parse(text), with_isin(National, "DE0001234565"), "{text}");
        }
        assert_eq!(parse("A1B2C3"), with_isin(National, "DE000A1B2C35"));
        assert_eq!(parse("/TS/TEST"), without_isin(Ticker));
        assert_eq!(parse("/TS/"), without_isin(Ticker));
        assert_eq!(parse("/XX/TEST0008"), without_isin(Bilateral));
        assert_eq!(parse("/XX/"), without_isin(Bilateral));
        assert_eq!(parse("/US/123456789"), without_isin(National));
        assert_eq!(parse("/US/123456"), without_isin(National));
        assert_eq!(parse("/ZZ/1"), without_isin(National));
        assert_eq!(parse("/DE/1234567"), without_isin(National));
        assert_eq!(parse("WKN 12345"), without_isin(National));
        assert_eq!(parse("WKN x"), without_isin(National));
        assert_eq!(parse("Some fund"), without_isin(Unrecognised));
        assert_eq!(parse("   "), without_isin(None));
        assert_eq!(parse_security_id(Option::None).kind, None);
        assert_eq!(parse_security_id(Some("")), SecurityId::default());
    }

    #[test]
    fn near_misses_are_no_identifiers() {
        use SecurityIdKind::Unrecognised;
        for text in [
            "US0378331006", // wrong check digit
            "us0378331005", // lower case
            "12345",        // five characters
            "1234567",      // seven characters
            "a1b2c3",       // six, but lower case
            "12345ü",       // six characters, not all letters or digits
            "WKN123456",    // no space
            "WKN ",         // trimmed to the bare prefix first
            "wkn 123456",   // prefix in lower case
            "/de/123456",   // country in lower case
            "/D1/123456",   // not two letters
            "/DE/",         // nothing after the country
            "/DE",          // too short
            "DE/123456",    // no leading slash
            "/TS",          // prefix incomplete
            "ISIN",         // prefix without its space
            "ISIN ",        // trimmed to the bare word first
        ] {
            assert_eq!(parse(text), without_isin(Unrecognised), "{text:?}");
        }
    }

    #[test]
    fn isin_prefix_is_removed_before_every_other_test() {
        use SecurityIdKind::{National, Ticker, Unrecognised};
        assert_eq!(
            parse("ISIN WKN 123456"),
            with_isin(National, "DE0001234565")
        );
        assert_eq!(parse("ISIN 123456"), with_isin(National, "DE0001234565"));
        assert_eq!(parse("ISIN /TS/ABC"), without_isin(Ticker));
        // Nothing is trimmed again after the prefix is gone.
        assert_eq!(parse("ISIN  US0378331005"), without_isin(Unrecognised));
        // Only one prefix is removed.
        assert_eq!(parse("ISIN ISIN US0378331005"), without_isin(Unrecognised));
    }

    #[test]
    fn national_number_part_is_not_validated() {
        use SecurityIdKind::National;
        // Lower-case letters are copied as they are; the result is not a
        // valid ISIN when read again.
        assert_eq!(parse("WKN abcdef"), with_isin(National, "DE000abcdef2"));
        assert!(!is_valid_isin("DE000abcdef2"));
        // Characters that have no digit value: no ISIN, flagged.
        for text in ["WKN 12 456", "WKN 12-456", "/DE/12345ü", "WKNR ab.def"] {
            assert_eq!(parse(text), (National, Option::None, true), "{text}");
        }
        // The number part is whatever follows the prefix, untrimmed.
        let id = parse_security_id(Some("WKN  12345"));
        assert_eq!(id.number.as_deref(), Some(" 12345"));
        assert_eq!(id.country.as_deref(), Some("DE"));
        assert!(id.derivation_failed);
        let id = parse_security_id(Some("/FR/ABC"));
        assert_eq!(
            (id.country.as_deref(), id.number.as_deref()),
            (Some("FR"), Some("ABC"))
        );
        assert!(!id.derivation_failed);
        // Text outside ASCII after the country form does not split a
        // character.
        let id = parse_security_id(Some("/DE/ü"));
        assert_eq!(id.number.as_deref(), Some("ü"));
        assert_eq!(parse("/Dé/123456").0, SecurityIdKind::Unrecognised);
    }

    #[test]
    fn kinds_have_stable_names() {
        use SecurityIdKind::{Bilateral, Isin, National, None, Ticker, Unrecognised};
        for kind in [None, Unrecognised, Isin, National, Ticker, Bilateral] {
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.name())
            );
            assert_eq!(kind.is_identifier(), !matches!(kind, None | Unrecognised));
        }
    }
}
