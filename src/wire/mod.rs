//! Record framing and the primitive values records are made of
//! (docs/format/02-records-wire-format.md).
//!
//! A collection file is records back to back ([`records`]); a record is one
//! or more chunks, read field by field through a [`Reader`]. The reader
//! implements the format's whole versioning mechanism: a read that does not
//! fit into the rest of its chunk returns a default and does not move, and
//! bytes left at the end of a chunk are skipped.
//!
//! What is an error and what is not follows 02 §7 and §8:
//!
//! | Condition | Result |
//! |---|---|
//! | chunk shorter than the fields read from it | defaults, never an error |
//! | bytes or whole, well-formed chunks the decoder does not read | counted in [`RecordStats`] |
//! | a `string` longer than its chunk | `None`, rest of the chunk reads as defaults, counted |
//! | invalid `decimal`, impossible `date_strict`, `ticks` or `time_hms` out of range, over-long `blob`, negative `string` length | [`WireError::Value`]: the record is undecodable |
//! | chunk header missing, chunk size below 4 or past the record, in a chunk the decoder reads or in one it never visits | [`WireError::Chunk`]: the file cannot be read further |
//! | record size out of bounds, record cut off by the end of the file | [`FramingError`]: the file cannot be read further |
//!
//! No error message and no `Debug` output of this module contains bytes of a
//! record.

mod error;
mod framing;
mod reader;
mod value;

pub use error::{ChunkProblem, FramingError, ValueProblem, WireError};
pub use framing::{records, RawRecord, Records, MAX_RECORD_SIZE};
pub use reader::{Reader, RecordStats, UnreadBytes};
pub use value::{
    civil_from_days, days_from_civil, days_in_month, is_leap_year, serialize_decimal,
    serialize_optional_decimal, text_lines, Date, Guid, RawDate, Stamp, TimeOfDay, Timestamp,
};
