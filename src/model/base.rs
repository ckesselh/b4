//! The record base and the collection loader shared by all record types.
//!
//! Every record starts with the [`RecordBase`]; every collection file is
//! loaded by [`load_collection`], which applies the tolerance rules of
//! docs/format/README.md ("Coping with other versions") in one place and
//! reports what it tolerated as counts in a [`LoadReport`]. The macros
//! [`code_enum!`](crate::code_enum) and [`flag_set!`](crate::flag_set) give
//! all record modules one representation for coded values.
//!
//! # Writing a record decoder
//!
//! A record type is a plain struct and an implementation of [`Record`] that
//! reads the fields in the order of the table in docs/format:
//!
//! ```
//! use b4::model::base::{load_collection, DecodeError, Record, RecordBase};
//! use b4::wire::{Reader, Timestamp};
//! use rust_decimal::Decimal;
//!
//! /// One quote of `ExchangeRatesBIN` (05 §3.5).
//! #[derive(Debug, Clone, PartialEq, serde::Serialize)]
//! pub struct ExchangeRate {
//!     #[serde(flatten)]
//!     pub base: RecordBase,
//!     pub base_currency: Option<String>,
//!     pub target_currency: Option<String>,
//!     pub quote_time: Option<Timestamp>,
//!     #[serde(serialize_with = "b4::wire::serialize_decimal")]
//!     pub quote_rate: Decimal,
//! }
//!
//! impl Record for ExchangeRate {
//!     const NAME: &'static str = "exchange rate";
//!
//!     fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
//!         // Wire order is source order: struct fields are evaluated top down.
//!         Ok(Self {
//!             base: RecordBase::read(r),
//!             base_currency: r.code(3),
//!             target_currency: r.code(3),
//!             quote_time: r.ticks("quote_time")?,
//!             quote_rate: r.decimal("quote_rate")?,
//!         })
//!     }
//!
//!     fn oid(&self) -> i32 {
//!         self.base.oid
//!     }
//! }
//!
//! // One record: size, chunk header, oid, update stamp, two codes, a time
//! // and the rate 1.0850.
//! let file = [
//!     0x2A, 0, 0, 0, 0x2A, 0, 0, 0, 0xFA, 0x46, 0x73, 0x2D, 0xFB, 0x46, 0x73, 0x2D,
//!     b'E', b'U', b'R', b'U', b'S', b'D', 0x00, 0x39, 0x8E, 0xB1, 0x2C, 0x39, 0xDC, 0x08,
//!     0x62, 0x2A, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0,
//! ];
//! let rates = load_collection::<ExchangeRate>(&file);
//! assert!(rates.report.is_clean(), "{:?}", rates.report.warnings("ExchangeRatesBIN"));
//! assert_eq!(
//!     serde_json::to_string(&rates.records[0]).unwrap(),
//!     r#"{"oid":762529530,"update_stamp":762529531,"base_currency":"EUR","target_currency":"USD","quote_time":"2024-02-29T13:45:30.0000000","quote_rate":"1.0850"}"#
//! );
//! ```
//!
//! A decoder never checks lengths: a chunk that ends early gives defaults
//! (02 §3). Reads that can meet an undecodable value take the field name and
//! are followed by `?`. A record with a second chunk calls
//! [`Reader::next_chunk`] where the table says so and ignores the result;
//! the payment base asks [`Reader::more_chunks`] to tell the legacy layout
//! (04 §5.2). Fields that are read only to advance use
//! [`Reader::skip_string`] or a plain `let _ =`.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;

use crate::wire::{records, FramingError, Reader, RecordStats, Stamp, UnreadBytes, WireError};

// The macros are exported at the crate root (`macro_rules!` has no other way
// to be public); these make them importable from here, next to the rest.
pub use crate::{code_enum, flag_set};

/// How many undecodable records a [`LoadReport`] describes one by one. The
/// rest is in the count: a damaged file must not grow the report without
/// bound.
const UNDECODABLE_SAMPLES: usize = 8;
/// How many entries of the unread-bytes histogram one warning spells out.
const HISTOGRAM_ENTRIES_IN_WARNING: usize = 4;

// ------------------------------------------------------------ record base

/// The two fields every record starts with (03 "Record base", 02 §5.1). Both
/// are stamps: whole seconds since 2000-01-01 UTC, kept as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct RecordBase {
    /// Object id. Unique within one collection file at most (02 §5.1); other
    /// records refer to it. Read as a time it is the creation time, roughly.
    pub oid: i32,
    /// Time of the last modification. [`RecordBase::NOT_STAMPED`] means the
    /// object was never stamped, which a file should not contain.
    pub update_stamp: i32,
}

impl RecordBase {
    /// The in-memory marker for "not stamped yet" in `update_stamp`.
    pub const NOT_STAMPED: i32 = -1;

    /// Read the two base fields. Every record decoder calls this first. It
    /// cannot fail: a record too short for them gets zeros.
    pub fn read(r: &mut Reader<'_>) -> Self {
        Self {
            oid: r.i32(),
            update_stamp: r.i32(),
        }
    }

    /// The object id read as a time: when the object was created,
    /// approximately. Ids run ahead of the clock by one second per object
    /// when many are created at once (02 §5.1).
    #[must_use]
    pub const fn created(&self) -> Stamp {
        Stamp::new(self.oid)
    }

    /// When the object was last modified, or `None` if it was never stamped.
    #[must_use]
    pub const fn updated(&self) -> Option<Stamp> {
        if self.update_stamp == Self::NOT_STAMPED {
            None
        } else {
            Some(Stamp::new(self.update_stamp))
        }
    }
}

/// The base on its own is a record too: it decodes the leading fields of any
/// collection, which is enough to count and identify records of a type whose
/// layout is not known.
impl Record for RecordBase {
    const NAME: &'static str = "record";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self::read(r))
    }

    fn oid(&self) -> i32 {
        self.oid
    }
}

// ----------------------------------------------------------------- Record

/// A record type: how to decode one record and what to call it.
pub trait Record: Sized {
    /// Name of the record type for messages: lower case, singular, as in
    /// docs/format ("booking", "standing order").
    const NAME: &'static str;

    /// Decode one record. The reader is positioned at the first byte of the
    /// first chunk; the decoder reads the fields in wire order, starting with
    /// [`RecordBase::read`], and calls [`Reader::next_chunk`] where the
    /// layout changes chunks. It need not consume the record: what it leaves
    /// unread is counted by the loader.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] if the record holds a value that cannot be decoded.
    /// A chunk too short for the layout is not an error (02 §3).
    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError>;

    /// The record's object id, for finding ids repeated within one file.
    fn oid(&self) -> i32;

    /// Whether the record was in the legacy single-chunk layout of the
    /// payment family (04 §5.2). The loader counts these per file.
    fn is_legacy(&self) -> bool {
        false
    }
}

/// Why one record could not be decoded. The message names the field and the
/// position, never a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// A primitive could not be read.
    #[error(transparent)]
    Wire(#[from] WireError),
    /// A value was read but the record cannot be built from it: a rule of the
    /// record type, not of the wire format.
    #[error("field `{field}`: {problem}")]
    Field {
        /// Name of the field.
        field: &'static str,
        /// What is wrong, in a few lower-case words. A constant, so that it
        /// cannot carry a value from the record.
        problem: &'static str,
    },
}

impl DecodeError {
    /// An error about the value of `field`.
    #[must_use]
    pub const fn field(field: &'static str, problem: &'static str) -> Self {
        Self::Field { field, problem }
    }

    /// Whether the error means the file cannot be read any further (a broken
    /// chunk structure) rather than that this one record is undecodable.
    #[must_use]
    pub const fn stops_file(&self) -> bool {
        match self {
            Self::Wire(error) => error.stops_file(),
            Self::Field { .. } => false,
        }
    }
}

// ----------------------------------------------------------------- loader

/// The records of one collection file and what loading them ran into.
#[derive(Debug, Clone, PartialEq)]
pub struct Collection<T> {
    /// The decoded records, in file order. Undecodable records are missing,
    /// so a position here is not the record's index in the file.
    pub records: Vec<T>,
    /// What was tolerated, skipped or cut off.
    pub report: LoadReport,
}

/// How many records of a file show some condition, and the first of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tally {
    /// Number of records.
    pub records: usize,
    /// Index in the file (counted from 0) of the first such record.
    pub first: Option<usize>,
}

impl Tally {
    /// Count the record at `index`.
    fn note(&mut self, index: usize) {
        self.records += 1;
        self.first.get_or_insert(index);
    }

    /// Whether no record shows the condition.
    #[must_use]
    pub const fn is_zero(&self) -> bool {
        self.records == 0
    }
}

/// A record that was skipped because it could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("record {index} at offset {offset}: {error}")]
pub struct Undecodable {
    /// Index of the record in the file, counted from 0.
    pub index: usize,
    /// Offset of the record's size prefix in the file.
    pub offset: usize,
    /// Which field, and why.
    pub error: DecodeError,
}

/// Why reading a file stopped before its end. The records before that point
/// were kept; there is no way to find the next record after it (02 §1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Stop {
    /// A record size out of bounds, or a record cut off by the end of the file.
    #[error(transparent)]
    Framing(#[from] FramingError),
    /// A record whose chunk structure is broken.
    #[error(transparent)]
    Record(Undecodable),
}

impl Stop {
    /// Index in the file (counted from 0) of the record reading stopped at.
    #[must_use]
    pub const fn index(&self) -> usize {
        match self {
            Self::Framing(error) => error.index(),
            Self::Record(record) => record.index,
        }
    }
}

/// What loading one collection file ran into, as counts.
///
/// The fields follow the table "Coping with other versions" of
/// docs/format/README.md. [`LoadReport::warnings`] renders the conditions
/// that table wants reported; the others are here so that a format change
/// shows up as a number. Nothing in a report is a value from a record.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoadReport {
    /// [`Record::NAME`] of the record type the file was decoded as.
    pub record_type: &'static str,
    /// Length of the file in bytes.
    pub file_len: usize,
    /// Records decoded and returned.
    pub decoded: usize,
    /// Records skipped because a value in them cannot be decoded (a value
    /// Banking4 rejects).
    pub undecodable: usize,
    /// The first few of those, with the field and the reason.
    pub undecodable_samples: Vec<Undecodable>,
    /// Records with bytes left unread at the end of a chunk the decoder
    /// visited: fields appended by a newer program version.
    pub unread_bytes: Tally,
    /// The same by chunk and number of bytes: how many records left exactly
    /// that many bytes unread in that chunk.
    pub unread_histogram: BTreeMap<UnreadBytes, usize>,
    /// Records with chunks the decoder never visited: a layout from a newer
    /// program version. Banking4 rejects the whole file for one of these.
    pub extra_chunks: Tally,
    /// Number of such chunks in the whole file.
    pub extra_chunk_count: usize,
    /// Records in which a text ran past the end of its chunk and the rest of
    /// that chunk therefore read as defaults (02 §3 step 4).
    pub overlong_strings: Tally,
    /// Records with unused bytes between the last chunk and the end of the
    /// record. Not reported; Banking4 never writes them.
    pub slack: Tally,
    /// Records in which a chunk ended before the last field of the layout, so
    /// that fields took their defaults: written by an older program version.
    /// Not reported.
    pub short_chunks: Tally,
    /// Records in the legacy single-chunk layout ([`Record::is_legacy`]).
    pub legacy: Tally,
    /// Records whose object id an earlier record of the file already had.
    /// Both are kept.
    pub duplicate_oids: Tally,
    /// Bytes after the last record that are too few for a record size (0 to
    /// 3). Not reported.
    pub stray_bytes: usize,
    /// Set when reading stopped before the end of the file.
    pub stopped: Option<Stop>,
}

impl LoadReport {
    /// Records the framing found and a decoder was run on: decoded plus
    /// undecodable. The record a file stopped at is not counted.
    #[must_use]
    pub const fn framed(&self) -> usize {
        self.decoded + self.undecodable
    }

    /// Whether the file was read to its end, every record decoded and nothing
    /// worth a warning was found.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.stopped.is_none()
            && self.undecodable == 0
            && self.unread_bytes.is_zero()
            && self.extra_chunks.is_zero()
            && self.overlong_strings.is_zero()
            && self.legacy.is_zero()
            && self.duplicate_oids.is_zero()
    }

    /// The warnings to show for this file, one per kind of condition, with
    /// counts instead of one line per record. `file` is the name of the vault
    /// entry. Empty when [`LoadReport::is_clean`].
    #[must_use]
    pub fn warnings(&self, file: &str) -> Vec<String> {
        let kind = self.record_type;
        let mut warnings = Vec::new();
        if let Some(stop) = &self.stopped {
            warnings.push(format!(
                "{file}: reading stopped at {stop}; kept the {} before it",
                counted(self.decoded, kind)
            ));
        }
        if self.undecodable > 0 {
            let first = self
                .undecodable_samples
                .first()
                .map(|sample| format!(" (first: {sample})"))
                .unwrap_or_default();
            warnings.push(format!(
                "{file}: skipped {} that cannot be decoded{first}",
                counted(self.undecodable, kind)
            ));
        }
        if !self.unread_bytes.is_zero() {
            warnings.push(format!(
                "{file}: bytes this version does not read in {} of {} ({})",
                self.unread_bytes.records,
                counted(self.decoded, kind),
                self.histogram_text()
            ));
        }
        if !self.extra_chunks.is_zero() {
            warnings.push(format!(
                "{file}: chunks this version does not read in {} of {} ({} skipped)",
                self.extra_chunks.records,
                counted(self.decoded, kind),
                self.extra_chunk_count
            ));
        }
        if !self.overlong_strings.is_zero() {
            warnings.push(format!(
                "{file}: a text runs past its chunk in {}; the fields after it were read as empty",
                counted(self.overlong_strings.records, kind)
            ));
        }
        if !self.legacy.is_zero() {
            warnings.push(format!(
                "{file}: legacy single-chunk layout in {}",
                counted(self.legacy.records, kind)
            ));
        }
        if !self.duplicate_oids.is_zero() {
            warnings.push(format!(
                "{file}: repeated object id in {}; all were kept",
                counted(self.duplicate_oids.records, kind)
            ));
        }
        warnings
    }

    /// The unread-bytes histogram as text: "chunk 1: 10 bytes in 18 records".
    fn histogram_text(&self) -> String {
        let mut parts: Vec<String> = self
            .unread_histogram
            .iter()
            .take(HISTOGRAM_ENTRIES_IN_WARNING)
            .map(|(unread, records)| {
                format!(
                    "chunk {}: {} byte{} in {}",
                    unread.chunk,
                    unread.bytes,
                    if unread.bytes == 1 { "" } else { "s" },
                    counted(*records, "record")
                )
            })
            .collect();
        if self.unread_histogram.len() > HISTOGRAM_ENTRIES_IN_WARNING {
            parts.push("...".to_owned());
        }
        parts.join(", ")
    }

    /// The first condition that [`load_collection_strict`] refuses, if any.
    fn strict_violation(&self) -> Option<StrictError> {
        let record = |tally: &Tally, problem| {
            tally.first.map(|index| StrictError::Record {
                index,
                records: tally.records,
                problem,
            })
        };
        if let Some(stop) = self.stopped {
            return Some(StrictError::Stopped(stop));
        }
        if let Some(sample) = self.undecodable_samples.first() {
            return Some(StrictError::Undecodable(*sample));
        }
        record(
            &self.unread_bytes,
            "bytes left unread at the end of a chunk",
        )
        .or_else(|| record(&self.extra_chunks, "chunks the decoder did not visit"))
        .or_else(|| record(&self.overlong_strings, "a text that runs past its chunk"))
        .or_else(|| record(&self.slack, "unused bytes after the last chunk"))
        .or((self.stray_bytes > 0).then_some(StrictError::StrayBytes(self.stray_bytes)))
    }
}

/// "1 booking record", "3 booking records".
fn counted(count: usize, kind: &str) -> String {
    if kind == "record" {
        format!("{count} record{}", if count == 1 { "" } else { "s" })
    } else {
        format!("{count} {kind} record{}", if count == 1 { "" } else { "s" })
    }
}

/// A condition [`load_collection`] tolerates and [`load_collection_strict`]
/// does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StrictError {
    /// Reading stopped before the end of the file.
    #[error("reading stopped at {0}")]
    Stopped(Stop),
    /// A record could not be decoded.
    #[error("undecodable {0}")]
    Undecodable(Undecodable),
    /// A record was not consumed exactly.
    #[error("record {index} and {} more: {problem}", .records.saturating_sub(1))]
    Record {
        /// Index in the file (counted from 0) of the first such record.
        index: usize,
        /// How many records of the file show the condition.
        records: usize,
        /// The condition.
        problem: &'static str,
    },
    /// Bytes after the last record.
    #[error("{0} stray bytes after the last record")]
    StrayBytes(usize),
}

/// Load a collection file: cut it into records, decode each as `T`, and apply
/// the whole tolerance table of docs/format/README.md.
///
/// | In the file | Here |
/// |---|---|
/// | no bytes (also: the file does not exist) | empty collection |
/// | chunk shorter than the layout | defaults; counted in `short_chunks` |
/// | bytes left at the end of a chunk | skipped; counted in `unread_bytes`, `unread_histogram` |
/// | well-formed chunks the decoder does not visit | skipped; counted in `extra_chunks` |
/// | unused bytes after the last chunk | skipped; counted in `slack` |
/// | 1 to 3 bytes after the last record | ignored; `stray_bytes` |
/// | a value Banking4 rejects | record skipped; `undecodable`, with index, field and reason |
/// | a repeated object id | both records kept; `duplicate_oids` |
/// | record size or chunk size out of bounds, record cut off | reading stops, records before it are kept; `stopped` |
///
/// The last row holds for every chunk of a record, also for one the decoder
/// never visits and for one behind a value that made the record undecodable:
/// skipping the chunks of a newer program version is tolerance, reading on
/// past a record whose chunk structure is broken is not.
///
/// It never fails and never panics, whatever the bytes are, and it allocates
/// nothing by a length taken from the file.
#[must_use]
pub fn load_collection<T: Record>(bytes: &[u8]) -> Collection<T> {
    let mut report = LoadReport {
        record_type: T::NAME,
        file_len: bytes.len(),
        ..LoadReport::default()
    };
    let mut decoded = Vec::new();
    let mut seen_oids = HashSet::new();
    let mut framing = records(bytes);
    for raw in framing.by_ref() {
        let raw = match raw {
            Ok(raw) => raw,
            Err(error) => {
                report.stopped = Some(Stop::Framing(error));
                break;
            }
        };
        let outcome =
            raw.reader()
                .map_err(DecodeError::from)
                .and_then(|mut reader| match T::decode(&mut reader) {
                    Err(error) if error.stops_file() => Err(error),
                    // The rest of the chunk structure is checked whatever the
                    // decoder made of the record, and a broken chunk outranks an
                    // undecodable value: it stops the file (docs/format/README.md,
                    // "Coping with other versions"; 02 §8 rule 2).
                    decoded => {
                        let stats = reader.finish()?;
                        decoded.map(|record| (record, stats))
                    }
                });
        match outcome {
            Ok((record, stats)) => {
                report.note_decoded(raw.index, &stats);
                if record.is_legacy() {
                    report.legacy.note(raw.index);
                }
                if !seen_oids.insert(record.oid()) {
                    report.duplicate_oids.note(raw.index);
                }
                decoded.push(record);
            }
            Err(error) => {
                let record = Undecodable {
                    index: raw.index,
                    offset: raw.offset,
                    error,
                };
                if error.stops_file() {
                    report.stopped = Some(Stop::Record(record));
                    break;
                }
                report.undecodable += 1;
                if report.undecodable_samples.len() < UNDECODABLE_SAMPLES {
                    report.undecodable_samples.push(record);
                }
            }
        }
    }
    report.stray_bytes = framing.stray_bytes();
    report.decoded = decoded.len();
    Collection {
        records: decoded,
        report,
    }
}

impl LoadReport {
    /// Add what the decoder left behind in the record at `index`.
    fn note_decoded(&mut self, index: usize, stats: &RecordStats) {
        if !stats.unread.is_empty() {
            self.unread_bytes.note(index);
            for unread in &stats.unread {
                *self.unread_histogram.entry(*unread).or_default() += 1;
            }
        }
        if stats.unvisited_chunks > 0 {
            self.extra_chunks.note(index);
            self.extra_chunk_count = self
                .extra_chunk_count
                .saturating_add(usize::try_from(stats.unvisited_chunks).unwrap_or(usize::MAX));
        }
        if stats.overlong_strings > 0 {
            self.overlong_strings.note(index);
        }
        if stats.slack_bytes > 0 {
            self.slack.note(index);
        }
        if stats.short_reads > 0 {
            self.short_chunks.note(index);
        }
    }
}

/// [`load_collection`] for tests: the cases it tolerates are errors.
///
/// It refuses a file that stops early, an undecodable record, bytes left
/// unread at the end of a chunk, chunks the decoder did not visit, a text
/// that runs past its chunk, slack after the last chunk and stray bytes after
/// the last record. In other words: it passes only when the decoder consumed
/// every byte of a file as Banking4 writes it.
///
/// Three things are still allowed, because well-formed files contain them:
/// chunks shorter than the layout (older writers), legacy single-chunk
/// records and repeated object ids. A test that wants to rule those out
/// checks `short_chunks`, `legacy` and `duplicate_oids` of the report.
///
/// # Errors
///
/// The first [`StrictError`] in the order above.
pub fn load_collection_strict<T: Record>(bytes: &[u8]) -> Result<Collection<T>, StrictError> {
    let collection = load_collection::<T>(bytes);
    match collection.report.strict_violation() {
        Some(error) => Err(error),
        None => Ok(collection),
    }
}

// ----------------------------------------------------------- coded values

/// Whether `name` is a lower-case snake_case identifier: `a`-`z`, digits and
/// single underscores, starting with a letter. The macros check the names of
/// enumeration values and flags with it at compile time.
#[doc(hidden)]
#[must_use]
pub const fn is_snake_case(name: &str) -> bool {
    let mut rest = name.as_bytes();
    let mut first = true;
    let mut after_underscore = false;
    while let [byte, tail @ ..] = rest {
        let ok = match *byte {
            b'a'..=b'z' => true,
            b'0'..=b'9' => !first,
            b'_' => !first && !after_underscore,
            _ => false,
        };
        if !ok {
            return false;
        }
        after_underscore = *byte == b'_';
        first = false;
        rest = tail;
    }
    !first && !after_underscore
}

/// Define an enumeration over an integer code: named variants plus
/// `Unknown(raw)` for every other number.
///
/// The format has no version, so a newer Banking4 may store a number this
/// version has no name for. Such a value must survive decoding (README,
/// "Coping with other versions"), which is why every enumeration is open.
///
/// Each line is `Variant = number => "name"`. The name is what the database
/// and JSON show; it must be lower-case snake_case, which is checked at
/// compile time. A number listed twice is a compile error too.
///
/// The macro generates, for `enum E: T`:
///
/// - `E::from_raw(raw: T) -> E`, never failing, and `From<T>`;
/// - `e.raw() -> T`, the stored number;
/// - `e.name() -> Cow<'static, str>`: the name, or the decimal number as text
///   when unknown; `Display` prints the same;
/// - `e.is_unknown() -> bool`;
/// - `Default` as `from_raw(0)`, which is what a field missing from a short
///   chunk decodes to;
/// - `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`, `Hash`, and `Serialize` as
///   the name.
///
/// Always go through `from_raw`; building `Unknown(n)` by hand for a number
/// that has a name gives a value that is not equal to the named variant.
///
/// # Example
///
/// ```
/// b4::code_enum! {
///     /// Who pays the charges of a foreign payment (04 §11.3).
///     pub enum CostBearer: u8 {
///         None = 0 => "none",
///         /// The payer (`OUR`).
///         Ours = 1 => "ours",
///         Shared = 2 => "shared",
///         Beneficiary = 3 => "beneficiary",
///     }
/// }
///
/// let bearer = CostBearer::from_raw(2);
/// assert_eq!(bearer, CostBearer::Shared);
/// assert_eq!(bearer.raw(), 2);
/// assert_eq!(bearer.name(), "shared");
/// assert_eq!(serde_json::to_string(&bearer).unwrap(), r#""shared""#);
///
/// let newer = CostBearer::from_raw(9);
/// assert_eq!(newer, CostBearer::Unknown(9));
/// assert_eq!(newer.raw(), 9);
/// assert_eq!(newer.name(), "9");
/// assert!(newer.is_unknown());
/// assert_eq!(CostBearer::default(), CostBearer::None);
/// ```
///
/// In a decoder: `cost_bearer: CostBearer::from_raw(r.u8())`.
#[macro_export]
macro_rules! code_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident: $repr:ty {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident = $value:literal => $text:literal
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name {
            $(
                $(#[$variant_meta])*
                $variant,
            )+
            /// A number this version has no name for, kept as stored.
            Unknown($repr),
        }

        impl $name {
            /// The value for a stored number. A number without a name becomes
            /// `Unknown`.
            #[must_use]
            pub const fn from_raw(raw: $repr) -> Self {
                match raw {
                    $( $value => Self::$variant, )+
                    other => Self::Unknown(other),
                }
            }

            /// The number as stored.
            #[must_use]
            pub const fn raw(self) -> $repr {
                match self {
                    $( Self::$variant => $value, )+
                    Self::Unknown(raw) => raw,
                }
            }

            /// The lower-case name of the value, or the decimal number as
            /// text when it has none.
            #[must_use]
            pub fn name(self) -> ::std::borrow::Cow<'static, str> {
                match self {
                    $( Self::$variant => ::std::borrow::Cow::Borrowed($text), )+
                    Self::Unknown(raw) => ::std::borrow::Cow::Owned(::std::string::ToString::to_string(&raw)),
                }
            }

            /// Whether the stored number has no name in this version.
            #[must_use]
            pub const fn is_unknown(self) -> bool {
                matches!(self, Self::Unknown(_))
            }
        }

        /// The value of the number zero, which is what a field missing from a
        /// short chunk decodes to.
        impl ::std::default::Default for $name {
            fn default() -> Self {
                Self::from_raw(0)
            }
        }

        impl ::std::convert::From<$repr> for $name {
            fn from(raw: $repr) -> Self {
                Self::from_raw(raw)
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.name())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(
                &self,
                serializer: S,
            ) -> ::std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.name())
            }
        }

        const _: () = {
            $(
                assert!(
                    $crate::model::base::is_snake_case($text),
                    "the name of an enumeration value must be lower-case snake_case"
                );
            )+
        };
    };
}

/// Define a flag set over an integer: a set of named bits that keeps every
/// bit it was given.
///
/// Bits without a name are not lost (README, "Coping with other versions"):
/// they stay in `raw()` and are reported by `names()` as one hexadecimal
/// item.
///
/// Each line is `NAME = bits => "name"`, in the order the names are to be
/// listed. List single flags only, not the aliases and masks the documents
/// mention, and not the value 0. The name must be lower-case snake_case and
/// the bits non-zero; both are checked at compile time.
///
/// The macro generates, for `struct F: T`:
///
/// - one constant `F::NAME` per flag;
/// - `F::from_raw(raw: T) -> F`, `From<T>` and `f.raw() -> T`;
/// - `f.contains(F::NAME) -> bool`: all bits of the argument are set;
/// - `f.is_empty() -> bool` and `f.unknown_bits() -> T`;
/// - `f.names() -> Vec<Cow<'static, str>>`: the names of the set flags in
///   declaration order, then the unnamed bits as one item like `"0x400"`;
/// - `Display` as those names joined with commas (the database form);
/// - `Default` as the empty set, `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`,
///   `Hash`, and `Serialize` as the list of names.
///
/// # Example
///
/// ```
/// b4::flag_set! {
///     /// Markers of a booking (04 §11.5).
///     pub struct EntryBits: u8 {
///         /// The user has seen the booking.
///         READ = 0x01 => "read",
///         EXPORTED_TO_BOOKS = 0x02 => "exported_to_books",
///         UPLOADED = 0x04 => "uploaded",
///     }
/// }
///
/// let bits = EntryBits::from_raw(0x45);
/// assert_eq!(bits.raw(), 0x45);
/// assert!(bits.contains(EntryBits::READ));
/// assert!(!bits.contains(EntryBits::EXPORTED_TO_BOOKS));
/// assert_eq!(bits.names(), ["read", "uploaded", "0x40"]);
/// assert_eq!(bits.to_string(), "read,uploaded,0x40");
/// assert_eq!(bits.unknown_bits(), 0x40);
/// assert_eq!(serde_json::to_string(&bits).unwrap(), r#"["read","uploaded","0x40"]"#);
/// assert!(EntryBits::default().is_empty());
/// ```
///
/// In a decoder: `entry_bits: EntryBits::from_raw(r.u8())`. A set that is
/// read from an `i32` can be declared over `i32`, or over `u32` with
/// `from_raw(r.i32() as u32)`.
#[macro_export]
macro_rules! flag_set {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident: $repr:ty {
            $(
                $(#[$flag_meta:meta])*
                $flag:ident = $value:literal => $text:literal
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        $vis struct $name($repr);

        impl $name {
            $(
                $(#[$flag_meta])*
                pub const $flag: Self = Self($value);
            )+

            /// The set with exactly the bits of a stored number, named or not.
            #[must_use]
            pub const fn from_raw(raw: $repr) -> Self {
                Self(raw)
            }

            /// The number as stored, with every bit.
            #[must_use]
            pub const fn raw(self) -> $repr {
                self.0
            }

            /// Whether all bits of `flags` are set.
            #[must_use]
            pub const fn contains(self, flags: Self) -> bool {
                self.0 & flags.0 == flags.0
            }

            /// Whether no bit is set.
            #[must_use]
            pub const fn is_empty(self) -> bool {
                self.0 == 0
            }

            /// The set bits that have no name in this version.
            #[must_use]
            pub const fn unknown_bits(self) -> $repr {
                self.0 & !(0 $( | $value )+)
            }

            /// The names of the set flags in declaration order, followed by
            /// the bits without a name as one hexadecimal item.
            #[must_use]
            pub fn names(self) -> ::std::vec::Vec<::std::borrow::Cow<'static, str>> {
                let mut names = ::std::vec::Vec::new();
                $(
                    if self.contains(Self::$flag) {
                        names.push(::std::borrow::Cow::Borrowed($text));
                    }
                )+
                let unknown = self.unknown_bits();
                if unknown != 0 {
                    names.push(::std::borrow::Cow::Owned(::std::format!("{unknown:#x}")));
                }
                names
            }
        }

        impl ::std::convert::From<$repr> for $name {
            fn from(raw: $repr) -> Self {
                Self(raw)
            }
        }

        /// The names joined with commas; empty for the empty set.
        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.names().join(","))
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(
                &self,
                serializer: S,
            ) -> ::std::result::Result<S::Ok, S::Error> {
                serializer.collect_seq(self.names())
            }
        }

        const _: () = {
            $(
                assert!(
                    $crate::model::base::is_snake_case($text),
                    "the name of a flag must be lower-case snake_case"
                );
                assert!($value != 0, "a flag needs at least one bit");
            )+
        };
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{ChunkProblem, ValueProblem};

    /// A record with a string and a decimal after the base, for the loader.
    #[derive(Debug, Clone, PartialEq)]
    struct Sample {
        base: RecordBase,
        name: Option<String>,
        amount: rust_decimal::Decimal,
    }

    impl Record for Sample {
        const NAME: &'static str = "sample";

        fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
            let base = RecordBase::read(r);
            let name = r.string("name")?;
            if name.as_deref() == Some("refuse") {
                return Err(DecodeError::field("name", "refused by the test"));
            }
            r.next_chunk()?;
            Ok(Self {
                base,
                name,
                amount: r.decimal("amount")?,
            })
        }

        fn oid(&self) -> i32 {
            self.base.oid
        }

        fn is_legacy(&self) -> bool {
            self.name.as_deref() == Some("legacy")
        }
    }

    /// One chunk: header and payload.
    fn chunk(payload: &[u8], more: bool) -> Vec<u8> {
        let size = u32::try_from(payload.len() + 4).unwrap() | if more { 0x8000_0000 } else { 0 };
        let mut bytes = size.to_le_bytes().to_vec();
        bytes.extend_from_slice(payload);
        bytes
    }

    /// A record: size prefix and body.
    fn record(body: &[u8]) -> Vec<u8> {
        let mut bytes = i32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
        bytes.extend_from_slice(body);
        bytes
    }

    /// The first chunk of a `Sample`: base and name.
    fn head(oid: i32, name: &str) -> Vec<u8> {
        let mut payload = oid.to_le_bytes().to_vec();
        payload.extend(7_i32.to_le_bytes());
        payload.extend(u16::try_from(name.len()).unwrap().to_le_bytes());
        payload.extend(name.as_bytes());
        payload
    }

    /// The 16 bytes of the decimal `value` with scale 2.
    fn cents(value: u32) -> Vec<u8> {
        let mut bytes = value.to_le_bytes().to_vec();
        bytes.extend([0; 8]);
        bytes.extend(0x0002_0000_u32.to_le_bytes());
        bytes
    }

    /// A well-formed two-chunk `Sample` record.
    fn sample(oid: i32, name: &str, amount: u32) -> Vec<u8> {
        let mut body = chunk(&head(oid, name), true);
        body.extend(chunk(&cents(amount), false));
        record(&body)
    }

    #[test]
    fn record_base_reads_two_stamps() {
        let body = chunk(&[0xFA, 0x46, 0x73, 0x2D, 0xFF, 0xFF, 0xFF, 0xFF], false);
        let mut r = Reader::new(&body).unwrap();
        let base = RecordBase::read(&mut r);
        assert_eq!(
            base,
            RecordBase {
                oid: 762_529_530,
                update_stamp: -1
            }
        );
        assert_eq!(base.created().to_iso(), "2024-02-29T13:45:30Z");
        assert_eq!(base.updated(), None);
        assert_eq!(
            RecordBase {
                oid: 1,
                update_stamp: 2
            }
            .updated(),
            Some(Stamp::new(2))
        );
        assert_eq!(
            serde_json::to_string(&base).unwrap(),
            r#"{"oid":762529530,"update_stamp":-1}"#
        );
        assert!(r.finish().unwrap().is_exact());
    }

    #[test]
    fn record_base_of_a_short_record_is_zero() {
        let body = chunk(&[1, 0, 0], false);
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(
            RecordBase::read(&mut r),
            RecordBase {
                oid: 0,
                update_stamp: 0
            }
        );
    }

    #[test]
    fn clean_file_loads_without_remarks() {
        let mut file = sample(1, "one", 110);
        file.extend(sample(2, "two", 220));
        let collection = load_collection_strict::<Sample>(&file).unwrap();
        assert_eq!(collection.records.len(), 2);
        assert_eq!(collection.records[1].name.as_deref(), Some("two"));
        assert_eq!(collection.records[1].amount.to_string(), "2.20");
        let report = &collection.report;
        assert!(report.is_clean());
        assert_eq!(
            (report.decoded, report.framed(), report.file_len),
            (2, 2, file.len())
        );
        assert_eq!(report.record_type, "sample");
        assert!(report.short_chunks.is_zero());
        assert_eq!(report.warnings("Samples"), Vec::<String>::new());
    }

    #[test]
    fn empty_file_is_an_empty_collection() {
        let collection = load_collection_strict::<Sample>(&[]).unwrap();
        assert!(collection.records.is_empty());
        assert!(collection.report.is_clean());
    }

    #[test]
    fn unread_bytes_and_extra_chunks_are_counted_not_refused() {
        // Record 0 is clean. Record 1 has three more bytes in chunk 1 and two
        // in chunk 2. Record 2 has two more chunks. Record 3 has slack.
        let mut file = sample(1, "one", 110);
        let mut longer_head = head(2, "two");
        longer_head.extend([9, 9, 9]);
        let mut longer_tail = cents(220);
        longer_tail.extend([9, 9]);
        let mut body = chunk(&longer_head, true);
        body.extend(chunk(&longer_tail, false));
        file.extend(record(&body));
        let mut body = chunk(&head(3, "three"), true);
        body.extend(chunk(&cents(330), true));
        body.extend(chunk(&[1, 2, 3], true));
        body.extend(chunk(&[], false));
        file.extend(record(&body));
        let mut body = chunk(&head(4, "four"), true);
        body.extend(chunk(&cents(440), false));
        body.extend([0xAA; 5]);
        file.extend(record(&body));

        let collection = load_collection::<Sample>(&file);
        assert_eq!(collection.records.len(), 4);
        assert_eq!(collection.records[2].amount.to_string(), "3.30");
        let report = &collection.report;
        assert!(!report.is_clean());
        assert_eq!(
            report.unread_bytes,
            Tally {
                records: 1,
                first: Some(1)
            }
        );
        assert_eq!(
            report.unread_histogram,
            BTreeMap::from([
                (UnreadBytes { chunk: 1, bytes: 3 }, 1),
                (UnreadBytes { chunk: 2, bytes: 2 }, 1)
            ])
        );
        assert_eq!(
            report.extra_chunks,
            Tally {
                records: 1,
                first: Some(2)
            }
        );
        assert_eq!(report.extra_chunk_count, 2);
        assert_eq!(
            report.slack,
            Tally {
                records: 1,
                first: Some(3)
            }
        );
        assert_eq!(
            report.warnings("Samples"),
            [
                "Samples: bytes this version does not read in 1 of 4 sample records \
                 (chunk 1: 3 bytes in 1 record, chunk 2: 2 bytes in 1 record)",
                "Samples: chunks this version does not read in 1 of 4 sample records (2 skipped)",
            ]
        );
        assert_eq!(
            load_collection_strict::<Sample>(&file),
            Err(StrictError::Record {
                index: 1,
                records: 1,
                problem: "bytes left unread at the end of a chunk"
            })
        );
    }

    #[test]
    fn strict_mode_refuses_each_tolerated_case() {
        let strict = |file: &[u8]| load_collection_strict::<Sample>(file).unwrap_err();

        let mut body = chunk(&head(1, "one"), true);
        body.extend(chunk(&cents(1), true));
        body.extend(chunk(&[], false));
        assert_eq!(
            strict(&record(&body)),
            StrictError::Record {
                index: 0,
                records: 1,
                problem: "chunks the decoder did not visit"
            }
        );

        let mut body = chunk(&head(1, "one"), true);
        body.extend(chunk(&cents(1), false));
        body.push(0);
        assert_eq!(
            strict(&record(&body)),
            StrictError::Record {
                index: 0,
                records: 1,
                problem: "unused bytes after the last chunk"
            }
        );

        let mut file = sample(1, "one", 1);
        file.extend([0, 0]);
        assert_eq!(strict(&file), StrictError::StrayBytes(2));
        assert_eq!(
            strict(&file).to_string(),
            "2 stray bytes after the last record"
        );

        // A name that claims more bytes than its chunk has.
        let mut payload = head(1, "");
        payload.truncate(8);
        payload.extend([5, 0, b'a']);
        let mut body = chunk(&payload, true);
        body.extend(chunk(&cents(1), false));
        let file = record(&body);
        assert_eq!(
            strict(&file),
            StrictError::Record {
                index: 0,
                records: 1,
                problem: "a text that runs past its chunk"
            }
        );
        let collection = load_collection::<Sample>(&file);
        assert_eq!(collection.records[0].name, None);
        assert_eq!(collection.records[0].amount.to_string(), "0.01");
        assert_eq!(
            collection.report.warnings("Samples"),
            ["Samples: a text runs past its chunk in 1 sample record; \
              the fields after it were read as empty"]
        );
    }

    #[test]
    fn short_chunks_legacy_records_and_repeated_ids_pass_strict_mode() {
        // Record 0 lacks its second chunk and most of its decimal; record 1
        // repeats the id of record 0.
        let mut file = record(&chunk(&head(5, "legacy"), false));
        file.extend(sample(5, "two", 2));
        let collection = load_collection_strict::<Sample>(&file).unwrap();
        assert_eq!(collection.records.len(), 2);
        assert_eq!(collection.records[0].amount, rust_decimal::Decimal::ZERO);
        let report = &collection.report;
        assert_eq!(
            report.short_chunks,
            Tally {
                records: 1,
                first: Some(0)
            }
        );
        assert_eq!(
            report.legacy,
            Tally {
                records: 1,
                first: Some(0)
            }
        );
        assert_eq!(
            report.duplicate_oids,
            Tally {
                records: 1,
                first: Some(1)
            }
        );
        assert!(!report.is_clean());
        assert_eq!(
            report.warnings("Acct1/Stmt"),
            [
                "Acct1/Stmt: legacy single-chunk layout in 1 sample record",
                "Acct1/Stmt: repeated object id in 1 sample record; all were kept",
            ]
        );
    }

    #[test]
    fn undecodable_records_are_skipped_and_the_file_goes_on() {
        let mut bad_amount = cents(1);
        bad_amount[14] = 29;
        let mut body = chunk(&head(2, "two"), true);
        body.extend(chunk(&bad_amount, false));
        let mut file = sample(1, "one", 1);
        file.extend(record(&body));
        file.extend(sample(3, "refuse", 3));
        file.extend(sample(4, "four", 4));

        let collection = load_collection::<Sample>(&file);
        let oids: Vec<i32> = collection.records.iter().map(Record::oid).collect();
        assert_eq!(oids, [1, 4]);
        let report = &collection.report;
        assert_eq!(
            (report.decoded, report.undecodable, report.framed()),
            (2, 2, 4)
        );
        assert_eq!(report.stopped, None);
        let first = Undecodable {
            index: 1,
            offset: 41,
            error: DecodeError::Wire(WireError::Value {
                field: "amount",
                offset: 21,
                problem: ValueProblem::InvalidDecimal,
            }),
        };
        let second = Undecodable {
            index: 2,
            offset: 82,
            error: DecodeError::field("name", "refused by the test"),
        };
        assert_eq!(report.undecodable_samples, [first, second]);
        assert_eq!(
            report.warnings("Samples"),
            [
                "Samples: skipped 2 sample records that cannot be decoded (first: record 1 at \
              offset 41: field `amount` at byte 21 of the record: invalid decimal encoding)"
            ]
        );
        assert_eq!(
            load_collection_strict::<Sample>(&file),
            Err(StrictError::Undecodable(first))
        );
    }

    #[test]
    fn only_the_first_undecodable_records_are_described() {
        let file: Vec<u8> = (0..20).flat_map(|oid| sample(oid, "refuse", 1)).collect();
        let report = load_collection::<Sample>(&file).report;
        assert_eq!(report.undecodable, 20);
        assert_eq!(report.undecodable_samples.len(), UNDECODABLE_SAMPLES);
        assert_eq!(report.decoded, 0);
    }

    #[test]
    fn truncated_tail_stops_the_file_and_keeps_what_came_before() {
        let mut file = sample(1, "one", 1);
        let second = sample(2, "two", 2);
        file.extend(&second[..second.len() - 1]);
        let collection = load_collection::<Sample>(&file);
        assert_eq!(collection.records.len(), 1);
        let stop = Stop::Framing(FramingError::Truncated {
            index: 1,
            offset: 41,
            available: second.len() - 5,
        });
        assert_eq!(collection.report.stopped, Some(stop));
        assert_eq!(stop.index(), 1);
        assert_eq!(
            collection.report.warnings("Samples"),
            [
                "Samples: reading stopped at record 1 at offset 41: the file ends inside the \
              record (36 bytes left); kept the 1 sample record before it"
            ]
        );
        assert_eq!(
            load_collection_strict::<Sample>(&file),
            Err(StrictError::Stopped(stop))
        );
    }

    #[test]
    fn broken_chunk_stops_the_file_without_resynchronising() {
        // The second record announces a second chunk that is not there; the
        // third record is intact but is not looked for.
        let mut file = sample(1, "one", 1);
        file.extend(record(&chunk(&head(2, "two"), true)));
        file.extend(sample(3, "three", 3));
        let collection = load_collection::<Sample>(&file);
        assert_eq!(collection.records.len(), 1);
        assert_eq!(
            collection.report.stopped,
            Some(Stop::Record(Undecodable {
                index: 1,
                offset: 41,
                error: DecodeError::Wire(WireError::Chunk {
                    chunk: 2,
                    offset: 17,
                    problem: ChunkProblem::HeaderTruncated,
                }),
            }))
        );
        assert_eq!(collection.report.undecodable, 0);

        // A record whose first chunk is broken stops the file as well.
        let mut file = sample(1, "one", 1);
        file.extend(record(&[9, 0, 0, 0, 1]));
        file.extend(sample(3, "three", 3));
        let collection = load_collection::<Sample>(&file);
        assert_eq!(collection.records.len(), 1);
        assert_eq!(collection.report.stopped.map(|stop| stop.index()), Some(1));
    }

    #[test]
    fn a_broken_chunk_the_decoder_never_visits_stops_the_file() {
        // The first record announces a second chunk of size 3, less than a
        // chunk header. `RecordBase` reads the first chunk only. That is no
        // chunk of a newer program version to skip: the record is corrupt,
        // reading stops and the intact record behind it is not looked for.
        let broken = [
            0x10, 0x00, 0x00, 0x00, // record_size 16
            0x0c, 0x00, 0x00, 0x80, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // chunk 1
            0x03, 0x00, 0x00, 0x00, // chunk 2: size 3
        ];
        let intact = [
            0x0c, 0x00, 0x00, 0x00, // record_size 12
            0x0c, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let stop = |index, offset| {
            Stop::Record(Undecodable {
                index,
                offset,
                error: DecodeError::Wire(WireError::Chunk {
                    chunk: 2,
                    offset: 12,
                    problem: ChunkProblem::SizeTooSmall,
                }),
            })
        };

        let file = [&broken[..], &intact[..]].concat();
        let collection = load_collection::<RecordBase>(&file);
        assert_eq!(collection.records, []);
        let report = &collection.report;
        assert_eq!(report.stopped, Some(stop(0, 0)));
        assert!(report.extra_chunks.is_zero());
        assert_eq!((report.decoded, report.undecodable), (0, 0));
        assert_eq!(
            report.warnings("Samples"),
            [
                "Samples: reading stopped at record 0 at offset 0: chunk 2 at byte 12 of the \
                 record: chunk size below the size of its header; kept the 0 records before it"
            ]
        );
        assert_eq!(
            load_collection_strict::<RecordBase>(&file),
            Err(StrictError::Stopped(stop(0, 0)))
        );

        // The records before it are kept.
        let file = [&intact[..], &broken[..], &intact[..]].concat();
        let collection = load_collection::<RecordBase>(&file);
        assert_eq!(
            collection.records,
            [RecordBase {
                oid: 2,
                update_stamp: 0
            }]
        );
        assert_eq!(collection.report.stopped, Some(stop(1, 16)));
    }

    #[test]
    fn a_broken_chunk_outranks_an_undecodable_value() {
        // The decoder gives up in the first chunk; the second chunk it never
        // gets to runs past the record. The file stops there all the same.
        let mut body = chunk(&head(2, "refuse"), true);
        body.extend([0x40, 0, 0, 0, 1]);
        let mut file = sample(1, "one", 1);
        file.extend(record(&body));
        file.extend(sample(3, "three", 3));
        let collection = load_collection::<Sample>(&file);
        assert_eq!(collection.records.len(), 1);
        assert_eq!(collection.report.undecodable, 0);
        assert_eq!(
            collection.report.stopped,
            Some(Stop::Record(Undecodable {
                index: 1,
                offset: 41,
                error: DecodeError::Wire(WireError::Chunk {
                    chunk: 2,
                    offset: 20,
                    problem: ChunkProblem::PastRecordEnd,
                }),
            }))
        );

        // With a sound second chunk the same record is skipped and the file
        // read on.
        let mut file = sample(1, "one", 1);
        file.extend(sample(2, "refuse", 2));
        file.extend(sample(3, "three", 3));
        let collection = load_collection::<Sample>(&file);
        assert_eq!(collection.records.len(), 2);
        assert_eq!(collection.report.undecodable, 1);
        assert_eq!(collection.report.stopped, None);
    }

    #[test]
    fn record_base_loads_any_collection() {
        let mut file = sample(11, "one", 1);
        file.extend(sample(12, "two", 2));
        let collection = load_collection::<RecordBase>(&file);
        assert_eq!(
            collection.records,
            [
                RecordBase {
                    oid: 11,
                    update_stamp: 7
                },
                RecordBase {
                    oid: 12,
                    update_stamp: 7
                }
            ]
        );
        assert_eq!(collection.report.unread_bytes.records, 2);
        assert_eq!(collection.report.extra_chunks.records, 2);
        assert_eq!(
            collection.report.warnings("Samples")[0],
            "Samples: bytes this version does not read in 2 of 2 records \
             (chunk 1: 5 bytes in 2 records)"
        );
    }

    code_enum! {
        /// Colour mark of a booking (04 §11.6), with a negative value.
        enum Colour: i32 {
            Undefined = -1 => "undefined",
            None = 0 => "none",
            Ok = 1 => "ok",
            /// A doc comment on a variant.
            Red = 2 => "red",
        }
    }

    flag_set! {
        /// Account flags (03), over a signed integer as read.
        struct Flags: i32 {
            HIDDEN = 0x1 => "hidden",
            EXCLUDE_FROM_TOTALS = 0x2 => "exclude_from_totals",
            /// A doc comment on a flag.
            WEEKLY = 0x100 => "statement_schedule_weekly",
        }
    }

    #[test]
    fn code_enum_maps_numbers_and_names_both_ways() {
        for (raw, value, name) in [
            (-1, Colour::Undefined, "undefined"),
            (0, Colour::None, "none"),
            (1, Colour::Ok, "ok"),
            (2, Colour::Red, "red"),
            (8, Colour::Unknown(8), "8"),
            (i32::MIN, Colour::Unknown(i32::MIN), "-2147483648"),
        ] {
            assert_eq!(Colour::from_raw(raw), value);
            assert_eq!(Colour::from(raw), value);
            assert_eq!(value.raw(), raw);
            assert_eq!(value.name(), name);
            assert_eq!(value.to_string(), name);
            assert_eq!(
                serde_json::to_string(&value).unwrap(),
                format!("\"{name}\"")
            );
            assert_eq!(value.is_unknown(), !(-1..=2).contains(&raw));
        }
        assert_eq!(Colour::default(), Colour::None);
        assert!(matches!(Colour::Red.name(), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn flag_set_keeps_every_bit_and_names_the_known_ones() {
        let flags = Flags::from_raw(0x1101);
        assert_eq!(flags.raw(), 0x1101);
        assert!(flags.contains(Flags::HIDDEN));
        assert!(flags.contains(Flags::WEEKLY));
        assert!(!flags.contains(Flags::EXCLUDE_FROM_TOTALS));
        assert_eq!(flags.unknown_bits(), 0x1000);
        assert_eq!(
            flags.names(),
            ["hidden", "statement_schedule_weekly", "0x1000"]
        );
        assert_eq!(flags.to_string(), "hidden,statement_schedule_weekly,0x1000");
        assert_eq!(
            serde_json::to_string(&flags).unwrap(),
            r#"["hidden","statement_schedule_weekly","0x1000"]"#
        );
        assert_eq!(Flags::from(3), Flags::from_raw(3));
        assert_eq!(
            Flags::from_raw(3).names(),
            ["hidden", "exclude_from_totals"]
        );

        let empty = Flags::default();
        assert!(empty.is_empty());
        assert!(empty.contains(Flags::default()));
        assert_eq!(empty.names(), Vec::<String>::new());
        assert_eq!(empty.to_string(), "");
        assert_eq!(serde_json::to_string(&empty).unwrap(), "[]");

        // All bits of a signed set: the unknown ones print as one item.
        let all = Flags::from_raw(-1);
        assert_eq!(
            all.names().last().map(|name| name.as_ref()),
            Some("0xfffffefc")
        );
        assert_eq!(all.names().len(), 4);
    }

    #[test]
    fn snake_case_check_accepts_only_plain_lower_case_names() {
        for name in [
            "a",
            "none",
            "sepa_direct_debit_b2b",
            "x1",
            "exclude_from_totals",
        ] {
            assert!(is_snake_case(name), "{name}");
        }
        for name in [
            "",
            "A",
            "camelCase",
            "1st",
            "_x",
            "x_",
            "a__b",
            "with space",
            "ü",
            "a-b",
        ] {
            assert!(!is_snake_case(name), "{name}");
        }
    }
}
