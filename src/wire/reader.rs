//! The cursor over one record: chunks and primitives
//! (docs/format/02-records-wire-format.md §2 to §4).

use std::fmt;

use rust_decimal::Decimal;

use super::error::{ChunkProblem, ValueProblem, WireError};
use super::value::{days_in_month, Date, Guid, RawDate, TimeOfDay, Timestamp};

/// Size of a chunk header, which the chunk size includes (02 §2.1).
const CHUNK_HEADER_SIZE: usize = 4;
/// Bits 0..29 of a chunk header: the chunk size. Bit 30 is ignored (02 §2.1).
const CHUNK_SIZE_MASK: u32 = 0x3FFF_FFFF;
/// Bit 31 of a chunk header: another chunk follows.
const MORE_CHUNKS_FLAG: u32 = 0x8000_0000;
/// The `u16` length that announces a 32-bit length (02 §4.8).
const LONG_FORM_MARKER: u16 = 0xFFFF;
/// Bits of a decimal's flags word that may be set: sign and scale (02 §4.7).
const DECIMAL_FLAGS_MASK: u32 = 0x80FF_0000;
/// Largest scale of a decimal (02 §4.7).
const DECIMAL_MAX_SCALE: u32 = 28;

/// A cursor over the body of one record.
///
/// A record is one or more chunks; every read is bounded by the end of the
/// current chunk. That gives the format its versioning, and the reader
/// reproduces it exactly (02 §3, §8):
///
/// - A read that does not fit into the rest of the chunk returns the type's
///   default (`0`, `false`, `None`, the nil GUID) and **does not move**. A
///   record written by an older program version is simply shorter.
/// - Bytes left at the end of a chunk are skipped by [`Reader::next_chunk`].
///   A record written by a newer program version is longer.
/// - [`Reader::next_chunk`] without a next chunk changes nothing; reading
///   carries on in place.
///
/// Reads that cannot fail return the value itself. Reads in which Banking4
/// rejects certain bytes return a `Result` and take the name of
/// the field, which ends up in the warning about the undecodable record:
/// [`decimal`](Reader::decimal), [`string`](Reader::string),
/// [`blob`](Reader::blob), [`date_strict`](Reader::date_strict),
/// [`ticks`](Reader::ticks) and [`time_hms`](Reader::time_hms). A short chunk
/// is never an error.
///
/// All positions are relative to the first byte of the record body.
pub struct Reader<'a> {
    body: &'a [u8],
    cursor: usize,
    chunk_start: usize,
    chunk_end: usize,
    more_chunks: bool,
    consumed_size: usize,
    chunk_number: u32,
    unread: Vec<UnreadBytes>,
    short_reads: u32,
    overlong_strings: u32,
}

/// Bytes a record decoder left unread at the end of one chunk: fields a newer
/// program version appended (02 §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnreadBytes {
    /// Number of the chunk within its record, counted from 1.
    pub chunk: u32,
    /// How many bytes were left between the cursor and the end of the chunk.
    pub bytes: usize,
}

/// What a record decoder left behind, for the loader's warnings
/// ([`Reader::finish`]). Counts only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordStats {
    /// Chunks the decoder read from, at least 1.
    pub chunks_visited: u32,
    /// Chunks with bytes left unread, in chunk order. Empty when the decoder
    /// consumed every visited chunk to its last byte.
    pub unread: Vec<UnreadBytes>,
    /// Chunks after the last visited one, which the decoder never opened: a
    /// layout from a newer program version (02 §8 rule 6). Banking4 rejects
    /// the whole file for this. Every one of them is a well-formed chunk;
    /// [`Reader::finish`] fails on one that is not.
    pub unvisited_chunks: u32,
    /// Bytes between the last chunk and the end of the record (02 §2.3).
    pub slack_bytes: usize,
    /// Reads that returned a default because the chunk was too short: fields
    /// an older program version did not write (02 §3).
    pub short_reads: u32,
    /// Strings whose length ran past their chunk and that therefore read as
    /// null, with the rest of that chunk reading as defaults (02 §3 step 4).
    pub overlong_strings: u32,
}

impl RecordStats {
    /// Whether the decoder consumed the record exactly: every chunk visited
    /// and read to its last byte, no slack, no over-long string. Short reads
    /// do not count against it.
    #[must_use]
    pub fn is_exact(&self) -> bool {
        self.unread.is_empty()
            && self.unvisited_chunks == 0
            && self.slack_bytes == 0
            && self.overlong_strings == 0
    }
}

/// A chunk header that passed the checks of 02 §2.3.
struct ChunkHeader {
    size: usize,
    more_chunks: bool,
}

/// Read the chunk header at `at`, which must not be after the end of `body`.
fn chunk_header(body: &[u8], at: usize) -> Result<ChunkHeader, ChunkProblem> {
    let rest = body.get(at..).unwrap_or_default();
    let Some(header) = rest.first_chunk::<CHUNK_HEADER_SIZE>() else {
        return Err(ChunkProblem::HeaderTruncated);
    };
    let header = u32::from_le_bytes(*header);
    // A 30-bit size always fits `usize` on the platforms b4 builds for; if it
    // ever did not, the chunk could not lie inside the record either.
    let size = usize::try_from(header & CHUNK_SIZE_MASK).unwrap_or(usize::MAX);
    if size < CHUNK_HEADER_SIZE {
        return Err(ChunkProblem::SizeTooSmall);
    }
    if size > rest.len() {
        return Err(ChunkProblem::PastRecordEnd);
    }
    Ok(ChunkHeader {
        size,
        more_chunks: header & MORE_CHUNKS_FLAG != 0,
    })
}

impl<'a> Reader<'a> {
    /// Open the record whose body is `body` (without the size prefix of a
    /// collection file) and its first chunk.
    ///
    /// # Errors
    ///
    /// [`WireError::Chunk`] if the body does not start with a valid chunk.
    /// Every record has at least one chunk (02 §2.3).
    pub fn new(body: &'a [u8]) -> Result<Reader<'a>, WireError> {
        let mut reader = Reader {
            body,
            cursor: 0,
            chunk_start: 0,
            chunk_end: 0,
            more_chunks: false,
            consumed_size: 0,
            chunk_number: 0,
            unread: Vec::new(),
            short_reads: 0,
            overlong_strings: 0,
        };
        reader.open_chunk()?;
        Ok(reader)
    }

    /// Open the chunk whose header is at the cursor (02 §2.3).
    fn open_chunk(&mut self) -> Result<(), WireError> {
        let header = chunk_header(self.body, self.cursor).map_err(|problem| WireError::Chunk {
            chunk: self.chunk_number.saturating_add(1),
            offset: self.cursor,
            problem,
        })?;
        // No overflow: `chunk_header` checked that the chunk lies inside the
        // body, so both sums are at most `body.len()`.
        self.chunk_start = self.cursor;
        self.chunk_end = self.cursor + header.size;
        self.more_chunks = header.more_chunks;
        self.consumed_size += header.size;
        self.cursor += CHUNK_HEADER_SIZE;
        self.chunk_number = self.chunk_number.saturating_add(1);
        Ok(())
    }

    /// Note the bytes between the cursor and the end of the current chunk as
    /// left unread.
    fn note_unread(&mut self) {
        let bytes = self.remaining();
        if bytes > 0 {
            self.unread.push(UnreadBytes {
                chunk: self.chunk_number,
                bytes,
            });
        }
    }

    // ------------------------------------------------------------ chunks

    /// Move to the next chunk of the record (02 §2.3).
    ///
    /// Returns `Ok(true)` when a chunk was opened: the cursor is then at its
    /// first payload byte, wherever it was before, and bytes left in the old
    /// chunk are skipped. Returns `Ok(false)` and changes **nothing** when the
    /// current chunk is the last one: the cursor stays where it is and later
    /// reads carry on in the current chunk. Legacy single-chunk records rely
    /// on that (04 §5.2), so decoders call this unconditionally and ignore
    /// the `bool`.
    ///
    /// # Errors
    ///
    /// [`WireError::Chunk`] if the current chunk announces another one and no
    /// valid chunk follows it.
    pub fn next_chunk(&mut self) -> Result<bool, WireError> {
        if !self.more_chunks {
            return Ok(false);
        }
        self.note_unread();
        self.cursor = self.chunk_end;
        self.open_chunk()?;
        Ok(true)
    }

    /// Whether another chunk follows the current one (bit 31 of its header).
    /// The payment base uses this to tell current records from legacy ones
    /// (04 §5.2).
    #[must_use]
    pub const fn more_chunks(&self) -> bool {
        self.more_chunks
    }

    /// Bytes left between the cursor and the end of the current chunk. Zero
    /// after an over-long string has poisoned the chunk.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.chunk_end.saturating_sub(self.cursor)
    }

    /// Position of the next byte to read. After an over-long string this is
    /// the end of the record, beyond the end of the chunk (02 §3 step 4).
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Position of the current chunk's header.
    #[must_use]
    pub const fn chunk_start(&self) -> usize {
        self.chunk_start
    }

    /// First position after the current chunk.
    #[must_use]
    pub const fn chunk_end(&self) -> usize {
        self.chunk_end
    }

    /// Number of the current chunk within the record, counted from 1.
    #[must_use]
    pub const fn chunk_number(&self) -> u32 {
        self.chunk_number
    }

    /// Sum of the sizes of all chunks opened so far (02 §2.2).
    #[must_use]
    pub const fn consumed_size(&self) -> usize {
        self.consumed_size
    }

    /// Length of the record body.
    #[must_use]
    pub const fn record_len(&self) -> usize {
        self.body.len()
    }

    /// End the record and report what the decoder left behind: bytes unread
    /// per chunk, chunks never visited, slack, short reads, over-long strings.
    /// None of that is an error (02 §2.3 "close a record"); the loader turns
    /// the counts into warnings.
    ///
    /// The chunks the decoder never opened are walked to the end of the
    /// record, and each must be a chunk by the rules of 02 §2.3. Being
    /// tolerant of chunks from a newer program version (02 §8 rule 6) means
    /// skipping chunks that are there, not accepting a record whose chunk
    /// structure is broken behind the point where this decoder stopped
    /// reading.
    ///
    /// # Errors
    ///
    /// [`WireError::Chunk`] if a chunk is announced and no valid chunk
    /// follows: the same error [`Reader::next_chunk`] gives a decoder that
    /// does visit it, with the same consequence for the file (02 §8 rule 2).
    pub fn finish(mut self) -> Result<RecordStats, WireError> {
        self.note_unread();
        let mut unvisited_chunks = 0_u32;
        let mut at = self.chunk_end;
        let mut more_chunks = self.more_chunks;
        while more_chunks {
            unvisited_chunks = unvisited_chunks.saturating_add(1);
            let header = chunk_header(self.body, at).map_err(|problem| WireError::Chunk {
                chunk: self.chunk_number.saturating_add(unvisited_chunks),
                offset: at,
                problem,
            })?;
            // `chunk_header` checked that the chunk lies inside the body, so
            // the sum is at most `body.len()`.
            at = at.saturating_add(header.size);
            more_chunks = header.more_chunks;
        }
        Ok(RecordStats {
            chunks_visited: self.chunk_number,
            unread: self.unread,
            unvisited_chunks,
            slack_bytes: self.body.len().saturating_sub(at),
            short_reads: self.short_reads,
            overlong_strings: self.overlong_strings,
        })
    }

    // -------------------------------------------------------- raw access

    /// The next `n` bytes of the current chunk, or `None` without moving if
    /// fewer are left: the general rule of 02 §3.
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.remaining() {
            self.short_reads = self.short_reads.saturating_add(1);
            return None;
        }
        // `n <= chunk_end - cursor`, so the sum cannot overflow and the slice
        // lies inside the chunk.
        let end = self.cursor + n;
        let bytes = self.body.get(self.cursor..end)?;
        self.cursor = end;
        Some(bytes)
    }

    /// The next `N` bytes as an array, by the same rule as [`Reader::take`].
    fn take_array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    /// A value error for a field whose bytes start at `offset`.
    const fn value_error(field: &'static str, offset: usize, problem: ValueProblem) -> WireError {
        WireError::Value {
            field,
            offset,
            problem,
        }
    }

    // ----------------------------------------------------------- integers

    /// `u8` (02 §4.1): one unsigned byte. Short chunk: `0`.
    pub fn u8(&mut self) -> u8 {
        self.take_array::<1>().map_or(0, |[byte]| byte)
    }

    /// `bool` (02 §4.2): one byte, any non-zero value is true. Short chunk:
    /// `false`.
    pub fn bool(&mut self) -> bool {
        self.u8() != 0
    }

    /// `tristate` (02 §4.3): one byte, `00` false, `01` true, anything else
    /// unknown. Short chunk: unknown. No record of the documented Banking4
    /// version uses it.
    pub fn tristate(&mut self) -> Option<bool> {
        match self.take_array::<1>()? {
            [0] => Some(false),
            [1] => Some(true),
            _ => None,
        }
    }

    /// `u16` (02 §4.4): little-endian, unsigned. Short chunk: `0`.
    pub fn u16(&mut self) -> u16 {
        self.take_array().map_or(0, u16::from_le_bytes)
    }

    /// A `u16` reinterpreted as signed 16-bit (02 §4.4). Only two fields are
    /// read this way: the booking's `gvc` and `gvc_extension` (04 §6.1).
    pub fn u16_signed(&mut self) -> i16 {
        self.take_array().map_or(0, i16::from_le_bytes)
    }

    /// `i32` (02 §4.5): little-endian, signed. Also object ids, stamps and
    /// references to other objects. Short chunk: `0`.
    pub fn i32(&mut self) -> i32 {
        self.take_array().map_or(0, i32::from_le_bytes)
    }

    /// `i64` (02 §4.6): little-endian, signed. Short chunk: `0`. No record of
    /// the documented Banking4 version uses it on its own; it carries `ticks`.
    pub fn i64(&mut self) -> i64 {
        self.take_array().map_or(0, i64::from_le_bytes)
    }

    /// Check a list count that was read from the record before anything is
    /// allocated by it: the number of entries as a `usize`, provided it is not
    /// negative and that many entries of at least `min_entry_size` bytes each
    /// still fit into the rest of the chunk (a `string` takes at least 2).
    ///
    /// Banking4 trusts such counts and allocates first (03, "Short, long and
    /// damaged records"); a damaged count must not make `b4` do so.
    /// Whether zero or a negative count means "no list" is the decoder's
    /// business and is tested before calling this.
    ///
    /// # Errors
    ///
    /// [`ValueProblem::CountOutOfRange`] for a negative count or one the rest
    /// of the chunk cannot hold.
    pub fn list_len(
        &self,
        field: &'static str,
        count: i32,
        min_entry_size: usize,
    ) -> Result<usize, WireError> {
        usize::try_from(count)
            .ok()
            .filter(|&count| count <= self.remaining() / min_entry_size.max(1))
            .ok_or(Self::value_error(
                field,
                self.cursor,
                ValueProblem::CountOutOfRange,
            ))
    }

    // ------------------------------------------------------------ decimal

    /// `decimal` (02 §4.7): 16 bytes, a 96-bit magnitude with sign and scale.
    /// The scale is preserved: `1.10` and `1.1` are different values on the
    /// wire and print differently. Short chunk: `0`.
    ///
    /// A stored negative zero comes back as plain zero with its scale, because
    /// [`Decimal`] has no negative zero; the two compare equal anyway.
    ///
    /// # Errors
    ///
    /// [`ValueProblem::InvalidDecimal`] for a scale above 28 or a reserved bit
    /// in the flags word, which Banking4 rejects.
    pub fn decimal(&mut self, field: &'static str) -> Result<Decimal, WireError> {
        let offset = self.cursor;
        let Some(bytes) = self.take_array::<16>() else {
            return Ok(Decimal::ZERO);
        };
        decimal_from_wire(bytes).ok_or(Self::value_error(
            field,
            offset,
            ValueProblem::InvalidDecimal,
        ))
    }

    // ---------------------------------------------------- string and blob

    /// The length prefix shared by `string` and `blob` (02 §4.8): a `u16`, or
    /// `FF FF` and an `i32`. `None` when the prefix does not fit; after an
    /// incomplete long form the cursor has moved past the marker only (02 §3
    /// step 2).
    fn length_prefix(&mut self) -> Option<i64> {
        let short = u16::from_le_bytes(self.take_array()?);
        if short != LONG_FORM_MARKER {
            return Some(i64::from(short));
        }
        Some(i64::from(i32::from_le_bytes(self.take_array()?)))
    }

    /// The bytes of a `string`, by the six steps of 02 §3.
    fn string_bytes(&mut self, field: &'static str) -> Result<Option<&'a [u8]>, WireError> {
        let offset = self.cursor;
        let Some(length) = self.length_prefix() else {
            return Ok(None);
        };
        if length == 0 {
            return Ok(None);
        }
        // Step 5: Banking4 moves the cursor backwards here. 02 §8 rule 7
        // makes it an error instead.
        let Ok(length) = usize::try_from(length) else {
            return Err(Self::value_error(
                field,
                offset,
                ValueProblem::NegativeStringLength,
            ));
        };
        // Step 4, decided by comparing the length with what is left and never
        // by adding it to the cursor, so an absurd length cannot wrap around.
        if length > self.remaining() {
            self.cursor = self.body.len();
            self.overlong_strings = self.overlong_strings.saturating_add(1);
            return Ok(None);
        }
        Ok(self.take(length))
    }

    /// `string` (02 §4.8): length-prefixed UTF-8. A length of zero is null,
    /// and since the writer stores an empty string the same way, `None` stands
    /// for both; `Some` is never empty. Invalid UTF-8 becomes U+FFFD. Short
    /// chunk: `None`.
    ///
    /// A length that runs past the end of the chunk is not an error: the
    /// result is `None`, the cursor jumps to the end of the **record**, and
    /// every later read in this chunk returns its default. The next chunk, if
    /// any, reads normally (02 §3 step 4). [`RecordStats::overlong_strings`]
    /// counts these.
    ///
    /// # Errors
    ///
    /// [`ValueProblem::NegativeStringLength`] for a negative long-form length
    /// (02 §8 rule 7).
    pub fn string(&mut self, field: &'static str) -> Result<Option<String>, WireError> {
        Ok(self
            .string_bytes(field)?
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned()))
    }

    /// Step over a `string` without decoding it, moving the cursor exactly as
    /// [`Reader::string`] does. For fields that are read only to advance: the
    /// unused ones, and bank access data that must never reach a struct.
    ///
    /// # Errors
    ///
    /// As [`Reader::string`].
    pub fn skip_string(&mut self, field: &'static str) -> Result<(), WireError> {
        self.string_bytes(field).map(|_| ())
    }

    /// `blob` (02 §4.10): length-prefixed raw bytes. A length of zero or
    /// below is null. Short chunk: `None`.
    ///
    /// # Errors
    ///
    /// [`ValueProblem::BlobTooLong`] for a length that runs past the end of
    /// the chunk, which is fatal for Banking4 too (02 §3). Unlike
    /// [`Reader::string`] there is no lenient path.
    pub fn blob(&mut self, field: &'static str) -> Result<Option<Vec<u8>>, WireError> {
        let offset = self.cursor;
        let Some(length) = self.length_prefix() else {
            return Ok(None);
        };
        let length = match usize::try_from(length) {
            Ok(0) | Err(_) => return Ok(None),
            Ok(length) => length,
        };
        if length > self.remaining() {
            return Err(Self::value_error(field, offset, ValueProblem::BlobTooLong));
        }
        Ok(self.take(length).map(<[u8]>::to_vec))
    }

    // --------------------------------------------------------------- code

    /// `code(n)` (02 §4.9): `width` bytes holding one ISO 8859-1 character
    /// each, padded with NUL. Trailing NULs are removed, then surrounding
    /// white space; what is left is the code, or `None` if nothing is left.
    /// Short chunk: `None`. `code(0)` reads nothing and is always `None`.
    pub fn code(&mut self, width: usize) -> Option<String> {
        if width == 0 {
            return None;
        }
        let bytes = self.take(width)?;
        let text: String = bytes.iter().map(|&byte| char::from(byte)).collect();
        // Each step once, in this order: a NUL uncovered by the white space
        // trimming stays (02 §4.9).
        let text = text
            .trim_end_matches('\0')
            .trim_matches(is_code_white_space);
        (!text.is_empty()).then(|| text.to_owned())
    }

    // --------------------------------------------------------------- guid

    /// `guid` (02 §4.11): 16 bytes. Short chunk: the nil GUID, which is also
    /// how "no GUID" is stored.
    pub fn guid(&mut self) -> Guid {
        self.take_array().map_or(Guid::NIL, Guid::from_wire_bytes)
    }

    // -------------------------------------------------------------- dates

    /// `date_raw` (02 §4.12): year, month and day as stored, not validated.
    /// `None` for four zero bytes and for a short chunk. Use
    /// [`RawDate::date`] for the real date.
    pub fn date_raw(&mut self) -> Option<RawDate> {
        let [year_low, year_high, month, day] = self.take_array()?;
        let year = i16::from_le_bytes([year_low, year_high]);
        (year != 0 || month != 0 || day != 0).then_some(RawDate { year, month, day })
    }

    /// `date_clamped` (02 §4.12): the same four bytes read leniently. Any zero
    /// component is "no date"; otherwise the year is capped at 9999, the month
    /// at 12 and the day at the length of that month. 0001-01-01 is "no date"
    /// as well. Short chunk: `None`.
    pub fn date_clamped(&mut self) -> Option<Date> {
        let [year_low, year_high, month, day] = self.take_array()?;
        let year = u16::from_le_bytes([year_low, year_high]);
        if year == 0 || month == 0 || day == 0 {
            return None;
        }
        let year = year.min(9999);
        let month = month.min(12);
        let day = day.min(days_in_month(i32::from(year), month));
        Date::new(year, month, day).filter(|&date| date != Date::MIN)
    }

    /// `date_strict` (02 §4.12): the same four bytes read as three separate
    /// values (`u16`, `u8`, `u8`) and not clamped. Any zero component is "no
    /// date", and so is 0001-01-01.
    ///
    /// In a short chunk each of the three reads follows the short-chunk rule
    /// on its own, so the cursor advances over the parts that fit and the
    /// result is `None` (02 §3).
    ///
    /// # Errors
    ///
    /// [`ValueProblem::ImpossibleDate`] if the three values are not a calendar
    /// date of the years 1 to 9999, which Banking4 rejects.
    pub fn date_strict(&mut self, field: &'static str) -> Result<Option<Date>, WireError> {
        let offset = self.cursor;
        let year = self.u16();
        let month = self.u8();
        let day = self.u8();
        if year == 0 || month == 0 || day == 0 {
            return Ok(None);
        }
        match Date::new(year, month, day) {
            Some(date) => Ok(Some(date).filter(|&date| date != Date::MIN)),
            None => Err(Self::value_error(
                field,
                offset,
                ValueProblem::ImpossibleDate,
            )),
        }
    }

    /// `ticks` (02 §4.13): a timestamp without zone as an `i64` count of
    /// 100 ns units since 0001-01-01. `None` for a count of zero and for a
    /// short chunk.
    ///
    /// # Errors
    ///
    /// [`ValueProblem::TicksOutOfRange`] for a negative count or one after the
    /// year 9999, which Banking4 rejects.
    pub fn ticks(&mut self, field: &'static str) -> Result<Option<Timestamp>, WireError> {
        let offset = self.cursor;
        let Some(bytes) = self.take_array() else {
            return Ok(None);
        };
        match Timestamp::from_ticks(i64::from_le_bytes(bytes)) {
            Some(timestamp) => Ok(Some(timestamp).filter(|&timestamp| timestamp != Timestamp::MIN)),
            None => Err(Self::value_error(
                field,
                offset,
                ValueProblem::TicksOutOfRange,
            )),
        }
    }

    /// `time_hms` (02 §4.14): hour, minute and second, one byte each. `None`
    /// when the hour byte is `FF`, whatever follows, and for a short chunk;
    /// `00 00 00` is midnight, not null.
    ///
    /// # Errors
    ///
    /// [`ValueProblem::TimeOutOfRange`] outside 00:00:00 to 23:59:59, which
    /// Banking4 rejects.
    pub fn time_hms(&mut self, field: &'static str) -> Result<Option<TimeOfDay>, WireError> {
        let offset = self.cursor;
        let Some([hour, minute, second]) = self.take_array() else {
            return Ok(None);
        };
        if hour == 0xFF {
            return Ok(None);
        }
        match TimeOfDay::new(hour, minute, second) {
            Some(time) => Ok(Some(time)),
            None => Err(Self::value_error(
                field,
                offset,
                ValueProblem::TimeOutOfRange,
            )),
        }
    }
}

/// Positions only: the bytes of a record are vault content and must not end
/// up in a log through `{:?}`.
impl fmt::Debug for Reader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reader")
            .field("record_len", &self.body.len())
            .field("cursor", &self.cursor)
            .field("chunk_number", &self.chunk_number)
            .field("chunk_start", &self.chunk_start)
            .field("chunk_end", &self.chunk_end)
            .field("more_chunks", &self.more_chunks)
            .field("consumed_size", &self.consumed_size)
            .finish_non_exhaustive()
    }
}

/// The 16-byte decimal layout of 02 §4.7 as a [`Decimal`], or `None` for an
/// invalid flags word.
fn decimal_from_wire(bytes: [u8; 16]) -> Option<Decimal> {
    let [l0, l1, l2, l3, m0, m1, m2, m3, h0, h1, h2, h3, f0, f1, f2, f3] = bytes;
    let flags = u32::from_le_bytes([f0, f1, f2, f3]);
    let scale = (flags >> 16) & 0xFF;
    // Checked here because `Decimal::from_parts` panics on a scale above 28.
    if flags & !DECIMAL_FLAGS_MASK != 0 || scale > DECIMAL_MAX_SCALE {
        return None;
    }
    Some(Decimal::from_parts(
        u32::from_le_bytes([l0, l1, l2, l3]),
        u32::from_le_bytes([m0, m1, m2, m3]),
        u32::from_le_bytes([h0, h1, h2, h3]),
        flags & 0x8000_0000 != 0,
        scale,
    ))
}

/// The white space Banking4 trims from a `code(n)`: among the 256
/// single-byte characters exactly `09`..`0D`, `20`, `85` and `A0` (02 §4.9).
const fn is_code_white_space(c: char) -> bool {
    matches!(c, '\u{09}'..='\u{0D}' | '\u{20}' | '\u{85}' | '\u{A0}')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One chunk holding `payload`, last of its record.
    fn chunk(payload: &[u8]) -> Vec<u8> {
        let mut bytes = u32::try_from(payload.len() + 4)
            .unwrap()
            .to_le_bytes()
            .to_vec();
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn short_chunk_gives_defaults_and_does_not_move() {
        let body = chunk(&[0xAB]);
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(r.i32(), 0);
        assert_eq!(r.cursor(), 4);
        assert_eq!(r.u16(), 0);
        assert_eq!(r.decimal("d"), Ok(Decimal::ZERO));
        assert_eq!(r.guid(), Guid::NIL);
        assert_eq!(r.code(3), None);
        assert_eq!(r.date_raw(), None);
        assert_eq!(r.date_clamped(), None);
        assert_eq!(r.ticks("t"), Ok(None));
        assert_eq!(r.time_hms("t"), Ok(None));
        assert_eq!(r.string("s"), Ok(None));
        assert_eq!(r.blob("b"), Ok(None));
        assert_eq!(r.cursor(), 4);
        // The byte that was too small for all of them is still there.
        assert_eq!(r.u8(), 0xAB);
        assert_eq!(r.remaining(), 0);
        let stats = r.finish().unwrap();
        assert_eq!(stats.short_reads, 11);
        assert!(stats.is_exact());
    }

    #[test]
    fn overlong_string_poisons_the_chunk_but_not_the_next_one() {
        // The worked example of 02 §3.
        let body = [
            0x09, 0x00, 0x00, 0x80, 0x05, 0x00, 0x41, 0x42, 0x5A, 0x0C, 0x00, 0x00, 0x00, 0xEE,
            0xEE, 0xEE, 0xEE, 0xEE, 0xEE, 0xEE, 0xEE,
        ];
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(r.string("s"), Ok(None));
        assert_eq!(r.cursor(), 21);
        assert_eq!(r.remaining(), 0);
        assert_eq!(r.u8(), 0);
        assert_eq!(r.cursor(), 21);
        assert_eq!(r.next_chunk(), Ok(true));
        assert_eq!(r.cursor(), 13);
        assert_eq!(r.u8(), 0xEE);
        assert_eq!(r.cursor(), 14);
        let stats = r.finish().unwrap();
        assert_eq!(stats.overlong_strings, 1);
        assert_eq!(stats.unread, [UnreadBytes { chunk: 2, bytes: 7 }]);
        assert!(!stats.is_exact());
    }

    #[test]
    fn absurd_lengths_are_overlong_and_never_wrap() {
        // Long-form length 0x7FFFFFFF: Banking4's sum wraps (02 §3).
        let body = chunk(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F, 0x41, 0x42]);
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(r.string("s"), Ok(None));
        assert_eq!(r.cursor(), body.len());
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(r.skip_string("s"), Ok(()));
        assert_eq!(r.cursor(), body.len());
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(
            r.blob("b"),
            Err(WireError::Value {
                field: "b",
                offset: 4,
                problem: ValueProblem::BlobTooLong
            })
        );
    }

    #[test]
    fn negative_lengths_are_an_error_for_strings_and_null_for_blobs() {
        let body = chunk(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x41, 0x42]);
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(
            r.string("name"),
            Err(WireError::Value {
                field: "name",
                offset: 4,
                problem: ValueProblem::NegativeStringLength
            })
        );
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(r.blob("b"), Ok(None));
        assert_eq!(r.cursor(), 10);
    }

    #[test]
    fn skip_string_moves_like_string() {
        let body = chunk(&[
            0x02, 0x00, 0x41, 0x42, 0x00, 0x00, 0xFF, 0xFF, 0x01, 0x00, 0x00, 0x00, 0x43, 0x07,
        ]);
        let mut read = Reader::new(&body).unwrap();
        let mut skipped = Reader::new(&body).unwrap();
        for _ in 0..4 {
            let _ = read.string("s").unwrap();
            skipped.skip_string("s").unwrap();
            assert_eq!(read.cursor(), skipped.cursor());
        }
        assert_eq!(skipped.cursor(), 17);
        assert_eq!(skipped.u8(), 7);
    }

    #[test]
    fn decimal_rejects_invalid_flags_instead_of_panicking() {
        let mut payload = [0_u8; 16];
        payload[0] = 1;
        payload[14] = 29;
        let body = chunk(&payload);
        let mut r = Reader::new(&body).unwrap();
        let error = r.decimal("amount").unwrap_err();
        assert_eq!(
            error,
            WireError::Value {
                field: "amount",
                offset: 4,
                problem: ValueProblem::InvalidDecimal
            }
        );
        assert!(!error.stops_file());
        assert_eq!(
            error.to_string(),
            "field `amount` at byte 4 of the record: invalid decimal encoding"
        );
    }

    #[test]
    fn decimal_keeps_the_scale() {
        let mut payload = [0_u8; 32];
        payload[0] = 110;
        payload[14] = 2;
        payload[16] = 11;
        payload[30] = 1;
        let body = chunk(&payload);
        let mut r = Reader::new(&body).unwrap();
        let a = r.decimal("a").unwrap();
        let b = r.decimal("b").unwrap();
        assert_eq!(a, b);
        assert_eq!(
            (a.to_string(), b.to_string()),
            ("1.10".to_owned(), "1.1".to_owned())
        );
    }

    #[test]
    fn u16_signed_reinterprets_the_same_bytes() {
        let body = chunk(&[0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x80]);
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(r.u16(), 65_535);
        assert_eq!(r.u16_signed(), -1);
        assert_eq!(r.u16_signed(), i16::MIN);
        assert_eq!(r.u16_signed(), 0);
    }

    #[test]
    fn list_len_refuses_counts_the_chunk_cannot_hold() {
        let body = chunk(&[0; 7]);
        let r = Reader::new(&body).unwrap();
        assert_eq!(r.list_len("list", 0, 2), Ok(0));
        assert_eq!(r.list_len("list", 3, 2), Ok(3));
        assert_eq!(r.list_len("list", 7, 0), Ok(7));
        let refused = Err(WireError::Value {
            field: "list",
            offset: 4,
            problem: ValueProblem::CountOutOfRange,
        });
        assert_eq!(r.list_len("list", 4, 2), refused);
        assert_eq!(r.list_len("list", -1, 2), refused);
        assert_eq!(r.list_len("list", i32::MAX, 1), refused);
    }

    #[test]
    fn next_chunk_without_a_next_chunk_changes_nothing() {
        let body = chunk(&[1, 2, 3]);
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(r.u8(), 1);
        assert_eq!(r.next_chunk(), Ok(false));
        assert_eq!((r.cursor(), r.chunk_number(), r.consumed_size()), (5, 1, 7));
        assert_eq!(r.u8(), 2);
    }

    #[test]
    fn broken_chunks_are_structural_errors() {
        let cases: [(&[u8], ChunkProblem); 4] = [
            (&[], ChunkProblem::HeaderTruncated),
            (&[4, 0, 0], ChunkProblem::HeaderTruncated),
            (&[3, 0, 0, 0, 0x2A], ChunkProblem::SizeTooSmall),
            (&[6, 0, 0, 0, 0x2A], ChunkProblem::PastRecordEnd),
        ];
        for (body, problem) in cases {
            let error = Reader::new(body).unwrap_err();
            assert_eq!(
                error,
                WireError::Chunk {
                    chunk: 1,
                    offset: 0,
                    problem
                }
            );
            assert!(error.stops_file());
        }
        // A header with only bit 30 set is a chunk of size 0 (02 §2.1).
        assert!(Reader::new(&[0, 0, 0, 0x40]).is_err());
        let body = [5, 0, 0, 0x80, 1, 6, 0, 0, 0, 2];
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(
            r.next_chunk(),
            Err(WireError::Chunk {
                chunk: 2,
                offset: 5,
                problem: ChunkProblem::PastRecordEnd
            })
        );
        assert_eq!(
            r.next_chunk().unwrap_err().to_string(),
            "chunk 2 at byte 5 of the record: chunk runs past the end of the record"
        );
    }

    #[test]
    fn finish_counts_what_was_left_behind() {
        // Three chunks and two bytes of slack; the decoder visits two chunks.
        let body = [
            7, 0, 0, 0x80, 1, 2, 3, // chunk 1, three payload bytes
            6, 0, 0, 0x80, 4, 5, // chunk 2, two payload bytes
            5, 0, 0, 0, 6, // chunk 3
            0xAA, 0xAA, // slack
        ];
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(r.u8(), 1);
        assert_eq!(r.next_chunk(), Ok(true));
        assert_eq!(r.u16(), 0x0504);
        let stats = r.finish().unwrap();
        assert_eq!(
            stats,
            RecordStats {
                chunks_visited: 2,
                unread: vec![UnreadBytes { chunk: 1, bytes: 2 }],
                unvisited_chunks: 1,
                slack_bytes: 2,
                short_reads: 0,
                overlong_strings: 0,
            }
        );
        assert!(!stats.is_exact());
    }

    #[test]
    fn finish_refuses_a_broken_chunk_the_decoder_never_visited() {
        // 02 §2.3: a set flag with no valid chunk behind it is corrupt,
        // whether or not the decoder goes there. Each body announces a second
        // chunk after a first one of five bytes.
        let cases: [(&[u8], ChunkProblem); 5] = [
            // Nothing follows at all.
            (&[5, 0, 0, 0x80, 1], ChunkProblem::HeaderTruncated),
            (&[5, 0, 0, 0x80, 1, 4, 0, 0], ChunkProblem::HeaderTruncated),
            // A size below the size of the header, also with bit 30 set.
            (&[5, 0, 0, 0x80, 1, 3, 0, 0, 0], ChunkProblem::SizeTooSmall),
            (
                &[5, 0, 0, 0x80, 1, 0, 0, 0, 0x40],
                ChunkProblem::SizeTooSmall,
            ),
            // A size that runs past the record.
            (&[5, 0, 0, 0x80, 1, 9, 0, 0, 0], ChunkProblem::PastRecordEnd),
        ];
        for (body, problem) in cases {
            let error = Reader::new(body).unwrap().finish().unwrap_err();
            assert_eq!(
                error,
                WireError::Chunk {
                    chunk: 2,
                    offset: 5,
                    problem
                },
                "{body:?}"
            );
            assert!(error.stops_file());
            // The decoder that goes there gets the same error.
            assert_eq!(Reader::new(body).unwrap().next_chunk(), Err(error));
        }

        // The broken chunk can be further on: chunks 2 and 3 are fine and
        // never visited, chunk 4 is the one that is cut off.
        let body = [
            5, 0, 0, 0x80, 1, // chunk 1
            4, 0, 0, 0x80, // chunk 2, empty
            6, 0, 0, 0x80, 2, 3, // chunk 3
            8, 0, 0, 0, 4, // chunk 4 announces eight bytes, five are left
        ];
        assert_eq!(
            Reader::new(&body).unwrap().finish(),
            Err(WireError::Chunk {
                chunk: 4,
                offset: 15,
                problem: ChunkProblem::PastRecordEnd
            })
        );
        // Well-formed chunks the decoder does not visit stay a count.
        let body = [5, 0, 0, 0x80, 1, 4, 0, 0, 0x80, 6, 0, 0, 0, 2, 3, 0xAA];
        let stats = Reader::new(&body).unwrap().finish().unwrap();
        assert_eq!((stats.unvisited_chunks, stats.slack_bytes), (2, 1));
        assert_eq!(stats.unread, [UnreadBytes { chunk: 1, bytes: 1 }]);
    }

    #[test]
    fn debug_output_shows_positions_and_no_bytes() {
        let body = chunk(b"SECRET");
        let text = format!("{:?}", Reader::new(&body).unwrap());
        assert!(text.contains("cursor: 4"), "{text}");
        assert!(!text.contains("83"), "{text}");
        assert!(!text.contains("SECRET"), "{text}");
    }
}
