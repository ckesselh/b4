//! Securities and their price series: stock instruments with their splits
//! (05 §3.6) and stock prices (05 §3.7).

use std::collections::HashSet;

use rust_decimal::Decimal;
use serde::Serialize;

use super::security_id::{parse_security_id, SecurityId, SecurityIdKind};
use crate::model::base::{DecodeError, Record, RecordBase};
use crate::wire::{Date, RawDate, Reader};

/// Prefix of the directory that holds the price series of one instrument
/// (05 §1): `Stock<oid>`.
const STOCK_DIRECTORY_PREFIX: &str = "Stock";

// ------------------------------------------------------------------ splits

/// One stock split of an instrument (05 §3.6.1).
///
/// Banking4 adjusts a price observed before `date` to today's share count:
/// divided by `factor` for an ordinary split, multiplied by it for a reverse
/// split (not confirmed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct StockSplit {
    /// The day of the split.
    pub date: Date,
    /// Whether this is a reverse split.
    pub reverse: bool,
    /// The split factor, as written: zero and negative values are not
    /// refused.
    pub factor: i32,
}

/// The splits read from a `splits` text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SplitList {
    /// The lines that parsed, in the order of the text.
    pub splits: Vec<StockSplit>,
    /// How many lines did not parse. Banking4 fails on the first of them and
    /// stops loading the file; here the other lines are still read.
    pub malformed_lines: u32,
}

/// Parse a `splits` text (05 §3.6.1): one split per line, lines separated by
/// LF, each line `<yyyymmdd>/<R or empty>/<factor>/`.
///
/// `None` gives an empty list. The text is cut at every LF and every piece
/// is parsed, an empty one too, so a text that ends with LF has one
/// malformed line. Whatever follows the third `/` of a line is ignored.
#[must_use]
pub fn parse_splits(text: Option<&str>) -> SplitList {
    let mut list = SplitList::default();
    let Some(text) = text else {
        return list;
    };
    for line in text.split('\n') {
        match parse_split_line(line) {
            Some(split) => list.splits.push(split),
            None => list.malformed_lines = list.malformed_lines.saturating_add(1),
        }
    }
    list
}

/// One line of a `splits` text, or `None` where Banking4 fails on it.
fn parse_split_line(line: &str) -> Option<StockSplit> {
    // A token is the text up to the next `/`; without a further `/` it is
    // missing. The fourth part is whatever follows the third `/`.
    let mut parts = line.splitn(4, '/');
    let (date, direction, factor) = (parts.next()?, parts.next()?, parts.next()?);
    parts.next()?;

    if date.len() != 8 || !date.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let date = Date::new(
        date.get(..4)?.parse().ok()?,
        date.get(4..6)?.parse().ok()?,
        date.get(6..)?.parse().ok()?,
    )?;

    // The white space Banking4 skips around the number when it parses the
    // factor: tab, LF, VT, FF, CR and the blank. Not the Unicode set.
    let factor = factor.trim_matches(|c| matches!(c, '\t'..='\r' | ' '));
    // An optional sign and decimal digits, in the signed 32-bit range; that
    // is exactly what the standard parser accepts.
    let factor: i32 = factor.parse().ok()?;

    Some(StockSplit {
        date,
        // Exactly `R`; anything else is an ordinary split.
        reverse: direction == "R",
        factor,
    })
}

// -------------------------------------------------------- stock instrument

/// One record of `StockInstruments` (05 §3.6): a security for which the
/// vault keeps a price series.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StockInstrument {
    /// Object id and update stamp. The object id names the directory of the
    /// price series ([`StockInstrument::price_directory`]).
    #[serde(flatten)]
    pub base: RecordBase,
    /// The canonical ISIN of the stored identifier (05 §5.1), `None` if it
    /// has none. Portfolio items find their instrument by it (05 §5.2). Not
    /// necessarily unique: see [`duplicate_isins`].
    pub isin: Option<String>,
    /// The identifier text as stored. A current writer stores the canonical
    /// ISIN or nothing; other spellings are accepted all the same.
    pub stored_isin: Option<String>,
    /// What kind of identifier `stored_isin` is.
    pub isin_kind: SecurityIdKind,
    /// Name of the security.
    pub name: Option<String>,
    /// Currency of the price series.
    pub price_currency: Option<String>,
    /// The stock splits as stored: one line per split (05 §3.6.1).
    pub splits: Option<String>,
    /// The lines of `splits` that parsed.
    pub split_list: Vec<StockSplit>,
    /// How many lines of `splits` did not parse. Banking4 would not load a
    /// file with such a record.
    pub malformed_split_lines: u32,
}

impl Record for StockInstrument {
    const NAME: &'static str = "stock instrument";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let base = RecordBase::read(r);
        let stored_isin = r.string("isin")?;
        let name = r.string("name")?;
        let price_currency = r.code(3);
        let splits = r.string("splits")?;

        let id = parse_security_id(stored_isin.as_deref());
        let split_list = parse_splits(splits.as_deref());
        Ok(Self {
            base,
            isin: id.canonical_isin,
            stored_isin,
            isin_kind: id.kind,
            name,
            price_currency,
            splits,
            split_list: split_list.splits,
            malformed_split_lines: split_list.malformed_lines,
        })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

impl StockInstrument {
    /// The stored identifier, interpreted (05 §5.1).
    #[must_use]
    pub fn security_id(&self) -> SecurityId {
        parse_security_id(self.stored_isin.as_deref())
    }

    /// The name of the vault directory that holds this instrument's price
    /// series, `Stock<oid>`; the series is the file `StockPrices` in it
    /// (05 §5.2). Price records carry no reference to their instrument: the
    /// directory name is the only link.
    #[must_use]
    pub fn price_directory(&self) -> String {
        format!("{STOCK_DIRECTORY_PREFIX}{}", self.base.oid)
    }
}

/// The object id of the stock instrument a directory `Stock<oid>` belongs
/// to, or `None` if `directory` is not such a name. Only the spelling
/// [`StockInstrument::price_directory`] produces is accepted: decimal, no
/// leading zeros, no plus sign.
#[must_use]
pub fn stock_directory_oid(directory: &str) -> Option<i32> {
    let digits = directory.strip_prefix(STOCK_DIRECTORY_PREFIX)?;
    let oid: i32 = digits.parse().ok()?;
    (oid.to_string() == digits).then_some(oid)
}

/// The instrument with the canonical ISIN `isin`, which is how a portfolio
/// item or transaction finds its instrument (05 §5.2). The comparison is
/// exact; of several instruments with that ISIN the first one is returned.
#[must_use]
pub fn instrument_for_isin<'a>(
    instruments: &'a [StockInstrument],
    isin: &str,
) -> Option<&'a StockInstrument> {
    instruments
        .iter()
        .find(|instrument| instrument.isin.as_deref() == Some(isin))
}

/// The positions in `instruments` of every record whose ISIN an earlier
/// record already has (05 §3.6 "Loader"). Banking4 fails at the first of
/// them and does not load the rest of the file; a reader keeps all records
/// and reports these. Records without an ISIN are never duplicates.
#[must_use]
pub fn duplicate_isins(instruments: &[StockInstrument]) -> Vec<usize> {
    let mut seen = HashSet::new();
    instruments
        .iter()
        .enumerate()
        .filter_map(|(index, instrument)| {
            let isin = instrument.isin.as_deref()?;
            (!seen.insert(isin)).then_some(index)
        })
        .collect()
}

// ------------------------------------------------------------- stock price

/// One record of `Stock<oid>/StockPrices` (05 §3.7): the price of one
/// instrument on one day, in the instrument's `price_currency`. The record
/// does not say which instrument: that is the `<oid>` of its directory
/// ([`stock_directory_oid`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StockPrice {
    /// Object id and update stamp.
    #[serde(flatten)]
    pub base: RecordBase,
    /// Trading day of the quote, as stored.
    pub date: Option<RawDate>,
    /// The price.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub price: Decimal,
}

impl Record for StockPrice {
    const NAME: &'static str = "stock price";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            base: RecordBase::read(r),
            date: r.date_raw(),
            price: r.decimal("price")?,
        })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(year: u16, month: u8, day: u8, reverse: bool, factor: i32) -> StockSplit {
        StockSplit {
            date: Date::new(year, month, day).unwrap(),
            reverse,
            factor,
        }
    }

    /// The splits of a text that must parse completely.
    fn parsed(text: &str) -> Vec<StockSplit> {
        let list = parse_splits(Some(text));
        assert_eq!(list.malformed_lines, 0, "{text:?}");
        list.splits
    }

    /// The number of malformed lines of a text, next to the lines that parsed.
    fn malformed(text: &str) -> (usize, u32) {
        let list = parse_splits(Some(text));
        (list.splits.len(), list.malformed_lines)
    }

    #[test]
    fn split_lines_follow_the_example_of_the_document() {
        assert_eq!(
            parsed("20200131/R/4/\n20210228//10/"),
            [split(2020, 1, 31, true, 4), split(2021, 2, 28, false, 10)]
        );
        assert_eq!(parse_splits(None), SplitList::default());
    }

    #[test]
    fn only_a_capital_r_means_reverse() {
        for (direction, reverse) in [
            ("R", true),
            ("", false),
            ("r", false),
            ("RR", false),
            (" R", false),
            ("X", false),
        ] {
            assert_eq!(
                parsed(&format!("20200131/{direction}/4/")),
                [split(2020, 1, 31, reverse, 4)],
                "{direction:?}"
            );
        }
    }

    #[test]
    fn factor_may_carry_a_sign_and_white_space() {
        for (factor, value) in [
            ("4", 4),
            ("+4", 4),
            ("-4", -4),
            ("0", 0),
            ("-0", 0),
            ("007", 7),
            (" 4 ", 4),
            ("\t4\r", 4),
            ("\u{b}\u{c}4", 4),
            ("2147483647", i32::MAX),
            ("-2147483648", i32::MIN),
        ] {
            assert_eq!(
                parsed(&format!("20200131//{factor}/")),
                [split(2020, 1, 31, false, value)],
                "{factor:?}"
            );
        }
    }

    #[test]
    fn text_after_the_third_slash_is_ignored() {
        assert_eq!(parsed("20200131/R/4/\r"), [split(2020, 1, 31, true, 4)]);
        assert_eq!(
            parsed("20200131/R/4/anything/else/\r\n20210228//10/"),
            [split(2020, 1, 31, true, 4), split(2021, 2, 28, false, 10)]
        );
    }

    #[test]
    fn malformed_lines_are_counted_and_the_others_kept() {
        for line in [
            "",
            "20200131",
            "20200131/",
            "20200131/R",
            "20200131/R/",
            "20200131/R/4",
            "2020013/R/4/",
            "202001311/R/4/",
            "2020-01-31/R/4/",
            " 20200131/R/4/",
            "+2020131/R/4/",
            "20200230/R/4/",
            "20201301/R/4/",
            "20200100/R/4/",
            "00000101/R/4/",
            "20200131/R//",
            "20200131/R/x/",
            "20200131/R/4.0/",
            "20200131/R/4 4/",
            "20200131/R/+/",
            "20200131/R/2147483648/",
            "20200131/R/-2147483649/",
            "20200131/R/99999999999999999999999999/",
            "20200131/R/\u{a0}4/",
            "20200131/R/４/",
            "２０２００１３１/R/4/",
        ] {
            assert_eq!(malformed(line), (0, 1), "{line:?}");
        }
        // The first and last representable days are fine.
        assert_eq!(parsed("00010101//2/"), [split(1, 1, 1, false, 2)]);
        assert_eq!(parsed("99991231//2/"), [split(9999, 12, 31, false, 2)]);
        assert_eq!(parsed("20240229//2/"), [split(2024, 2, 29, false, 2)]);

        // A trailing LF leaves an empty piece, which is malformed.
        assert_eq!(malformed("20200131/R/4/\n"), (1, 1));
        assert_eq!(malformed("\n"), (0, 2));
        assert_eq!(
            malformed("bad\n20200131/R/4/\nworse\n20210228//10/"),
            (2, 2)
        );
    }

    fn instrument(oid: i32, stored_isin: Option<&str>) -> StockInstrument {
        let id = parse_security_id(stored_isin);
        StockInstrument {
            base: RecordBase {
                oid,
                update_stamp: 0,
            },
            isin: id.canonical_isin,
            stored_isin: stored_isin.map(str::to_owned),
            isin_kind: id.kind,
            name: None,
            price_currency: None,
            splits: None,
            split_list: Vec::new(),
            malformed_split_lines: 0,
        }
    }

    #[test]
    fn price_directory_and_its_object_id_are_inverse() {
        for oid in [0, 1, 762_506_001, i32::MAX, -1, i32::MIN] {
            let directory = instrument(oid, None).price_directory();
            assert_eq!(stock_directory_oid(&directory), Some(oid), "{directory}");
        }
        assert_eq!(
            instrument(762_506_001, None).price_directory(),
            "Stock762506001"
        );
        for other in [
            "Stock",
            "Stock+1",
            "Stock01",
            "Stock1x",
            "Stock 1",
            "Stock2147483648",
            "stock1",
            "Acct1",
            "StockPrices",
            "Stock-0",
            "",
        ] {
            assert_eq!(stock_directory_oid(other), None, "{other:?}");
        }
    }

    #[test]
    fn instruments_are_found_by_their_canonical_isin() {
        let instruments = [
            instrument(1, Some("US0378331005")),
            instrument(2, None),
            instrument(3, Some("WKN 123456")),
            instrument(4, Some("/TS/ABC")),
            instrument(5, Some("DE0001234565")),
            instrument(6, Some("US0378331005")),
        ];
        assert_eq!(instruments[2].isin.as_deref(), Some("DE0001234565"));
        assert_eq!(instruments[2].security_id().kind, SecurityIdKind::National);
        let found = |isin| instrument_for_isin(&instruments, isin).map(Record::oid);
        assert_eq!(found("US0378331005"), Some(1));
        assert_eq!(found("DE0001234565"), Some(3));
        assert_eq!(found("us0378331005"), None);
        assert_eq!(found("FR0000120271"), None);
        // Records without an ISIN are never duplicates of each other.
        assert_eq!(duplicate_isins(&instruments), [4, 5]);
        assert!(duplicate_isins(&instruments[..4]).is_empty());
        assert!(duplicate_isins(&[]).is_empty());
    }
}
