//! Errors of the record codec.
//!
//! The two classes follow the consequences docs/format/README.md ("Coping with
//! other versions") attaches to them: a broken chunk structure stops the file,
//! a value Banking4 would reject makes one record undecodable.
//! Messages carry positions and field names only, never bytes of a record.

/// Why reading a record failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The chunk structure of the record is broken (02 §2.3, §7). The caller
    /// stops reading the file and keeps the records before this one.
    #[error("chunk {chunk} at byte {offset} of the record: {problem}")]
    Chunk {
        /// Number of the chunk that could not be opened, counted from 1.
        chunk: u32,
        /// Where its header was expected, relative to the record body.
        offset: usize,
        /// What is wrong with it.
        problem: ChunkProblem,
    },
    /// A field holds a value that cannot be decoded (02 §7, §8 rule 8). The
    /// record is undecodable; the next record of the file is unaffected.
    #[error("field `{field}` at byte {offset} of the record: {problem}")]
    Value {
        /// Name of the field as the record decoder passed it to the read.
        field: &'static str,
        /// Where the value starts, relative to the record body.
        offset: usize,
        /// What is wrong with it.
        problem: ValueProblem,
    },
}

impl WireError {
    /// Whether the error means the file cannot be read any further (a broken
    /// chunk structure) rather than that one record is undecodable.
    #[must_use]
    pub const fn stops_file(&self) -> bool {
        matches!(self, Self::Chunk { .. })
    }
}

/// What is wrong with a chunk header (02 §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChunkProblem {
    /// Fewer than four bytes are left where a chunk header must be.
    #[error("no room for a chunk header")]
    HeaderTruncated,
    /// The size in the header is below four, the size of the header itself.
    #[error("chunk size below the size of its header")]
    SizeTooSmall,
    /// The chunk would end after the record.
    #[error("chunk runs past the end of the record")]
    PastRecordEnd,
}

/// What is wrong with a value (02 §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ValueProblem {
    /// A `decimal` with a scale above 28 or reserved flag bits set (02 §4.7).
    #[error("invalid decimal encoding")]
    InvalidDecimal,
    /// A `date_strict` that is no calendar date (02 §4.12).
    #[error("impossible calendar date")]
    ImpossibleDate,
    /// A `ticks` count below zero or after the year 9999 (02 §4.13).
    #[error("timestamp out of range")]
    TicksOutOfRange,
    /// A `time_hms` outside 00:00:00 to 23:59:59 (02 §4.14).
    #[error("time of day out of range")]
    TimeOutOfRange,
    /// A `blob` longer than the rest of its chunk (02 §3).
    #[error("byte array longer than its chunk")]
    BlobTooLong,
    /// A long-form `string` with a negative length (02 §3 step 5, §8 rule 7).
    #[error("negative text length")]
    NegativeStringLength,
    /// A count of list entries that is negative or larger than the rest of
    /// the chunk could hold (03, "Short, long and damaged records").
    #[error("list count negative or larger than its chunk can hold")]
    CountOutOfRange,
}

/// Why a collection file could not be cut into records any further (02 §1.2).
///
/// Records before the error are valid; there is no way to find the next
/// record after it, so reading stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FramingError {
    /// The record size is below 4 or above [`MAX_RECORD_SIZE`](super::MAX_RECORD_SIZE).
    #[error("record {index} at offset {offset}: record size out of bounds")]
    SizeOutOfBounds {
        /// Number of the record in the file, counted from 0.
        index: usize,
        /// Offset of its size prefix in the file.
        offset: usize,
    },
    /// The file ends inside the record body.
    #[error(
        "record {index} at offset {offset}: the file ends inside the record ({available} bytes left)"
    )]
    Truncated {
        /// Number of the record in the file, counted from 0.
        index: usize,
        /// Offset of its size prefix in the file.
        offset: usize,
        /// Bytes that follow the size prefix.
        available: usize,
    },
}

impl FramingError {
    /// Number of the record the file broke at, counted from 0.
    #[must_use]
    pub const fn index(&self) -> usize {
        match *self {
            Self::SizeOutOfBounds { index, .. } | Self::Truncated { index, .. } => index,
        }
    }

    /// Offset in the file of the size prefix of that record.
    #[must_use]
    pub const fn offset(&self) -> usize {
        match *self {
            Self::SizeOutOfBounds { offset, .. } | Self::Truncated { offset, .. } => offset,
        }
    }
}
