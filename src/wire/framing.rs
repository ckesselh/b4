//! Cutting a collection file into records
//! (docs/format/02-records-wire-format.md §1.2, §8 rule 1).

use std::fmt;
use std::iter::FusedIterator;

use super::error::{FramingError, WireError};
use super::reader::Reader;

/// Size of the `record_size` prefix, which the size does not count.
const SIZE_PREFIX: usize = 4;
/// Smallest record: one empty chunk.
const MIN_RECORD_SIZE: usize = 4;
/// Largest record body the format allows, inclusive (02 §1.2).
pub const MAX_RECORD_SIZE: usize = 0x4000_0000;

/// One record of a collection file: where it is and its body.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RawRecord<'a> {
    /// Number of the record in the file, counted from 0.
    pub index: usize,
    /// Offset of the record's size prefix in the file.
    pub offset: usize,
    /// The record body: the chunks, without the size prefix.
    pub body: &'a [u8],
}

impl<'a> RawRecord<'a> {
    /// Open the record for reading.
    ///
    /// # Errors
    ///
    /// [`WireError::Chunk`] if the body does not start with a valid chunk.
    pub fn reader(&self) -> Result<Reader<'a>, WireError> {
        Reader::new(self.body)
    }
}

/// Position and length only: the body is vault content and must not end up in
/// a log through `{:?}`.
impl fmt::Debug for RawRecord<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawRecord")
            .field("index", &self.index)
            .field("offset", &self.offset)
            .field("len", &self.body.len())
            .finish()
    }
}

/// Iterator over the records of a collection file, made by [`records`].
///
/// Yields the records in file order. A record size out of bounds or a body
/// cut off by the end of the file yields one `Err` and ends the iteration:
/// the records before it stand, and nothing after it can be found, because
/// the format has no way to resynchronise (02 §1.2).
#[derive(Clone)]
pub struct Records<'a> {
    data: &'a [u8],
    position: usize,
    index: usize,
    done: bool,
}

/// Cut a collection file into its records: `[i32 record_size][body]`, back to
/// back, with no header, count or terminator (02 §1.2).
///
/// An empty file is an empty collection. One to three stray bytes after the
/// last record are ignored, as Banking4 does; see
/// [`Records::stray_bytes`].
#[must_use]
pub const fn records(data: &[u8]) -> Records<'_> {
    Records {
        data,
        position: 0,
        index: 0,
        done: false,
    }
}

impl Records<'_> {
    /// Bytes after the last complete record that are too few to hold a record
    /// size (0 to 3). Meaningful once the iteration has ended without an
    /// error; 0 before that.
    #[must_use]
    pub fn stray_bytes(&self) -> usize {
        let rest = self.data.len().saturating_sub(self.position);
        if self.done && rest < SIZE_PREFIX {
            rest
        } else {
            0
        }
    }

    /// Offset in the file of the next record to be read, or of the point at
    /// which the iteration ended.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.position
    }
}

impl<'a> Iterator for Records<'a> {
    type Item = Result<RawRecord<'a>, FramingError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let rest = self.data.get(self.position..).unwrap_or_default();
        // Fewer than four bytes left is the end of the file, silently.
        let Some((prefix, after_prefix)) = rest.split_first_chunk::<SIZE_PREFIX>() else {
            self.done = true;
            return None;
        };
        let (index, offset) = (self.index, self.position);
        // The bound is tested before the body is looked at; zero and negative
        // sizes fail the conversion or the lower bound.
        let size = usize::try_from(i32::from_le_bytes(*prefix))
            .ok()
            .filter(|size| (MIN_RECORD_SIZE..=MAX_RECORD_SIZE).contains(size));
        let Some(size) = size else {
            self.done = true;
            return Some(Err(FramingError::SizeOutOfBounds { index, offset }));
        };
        let Some(body) = after_prefix.get(..size) else {
            self.done = true;
            return Some(Err(FramingError::Truncated {
                index,
                offset,
                available: after_prefix.len(),
            }));
        };
        // No overflow: the body lies inside `data`.
        self.position = offset + SIZE_PREFIX + size;
        self.index += 1;
        Some(Ok(RawRecord {
            index,
            offset,
            body,
        }))
    }
}

impl FusedIterator for Records<'_> {}

/// Counters only, for the same reason as [`RawRecord`].
impl fmt::Debug for Records<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Records")
            .field("len", &self.data.len())
            .field("position", &self.position)
            .field("index", &self.index)
            .field("done", &self.done)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two-record example of 02 §1.2.
    const TWO_RECORDS: [u8; 24] = [
        0x05, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x10, 0x0B, 0x00, 0x00, 0x00, 0x05, 0x00,
        0x00, 0x80, 0x12, 0x06, 0x00, 0x00, 0x00, 0x02, 0x01,
    ];

    #[test]
    fn records_are_cut_back_to_back() {
        let mut iter = records(&TWO_RECORDS);
        let first = iter.next().unwrap().unwrap();
        assert_eq!(
            (first.index, first.offset, first.body),
            (0, 0, &TWO_RECORDS[4..9])
        );
        let second = iter.next().unwrap().unwrap();
        assert_eq!(
            (second.index, second.offset, second.body),
            (1, 9, &TWO_RECORDS[13..])
        );
        assert!(iter.next().is_none());
        assert!(iter.next().is_none());
        assert_eq!(iter.stray_bytes(), 0);
        assert_eq!(iter.position(), 24);

        let mut reader = second.reader().unwrap();
        assert_eq!(reader.u8(), 0x12);
        assert_eq!(reader.next_chunk(), Ok(true));
        assert_eq!((reader.u8(), reader.u8()), (2, 1));
    }

    #[test]
    fn empty_file_is_an_empty_collection() {
        let mut iter = records(&[]);
        assert!(iter.next().is_none());
        assert_eq!(iter.stray_bytes(), 0);
    }

    #[test]
    fn stray_bytes_at_the_end_are_ignored_and_counted() {
        for stray in 1..=3 {
            let mut data = TWO_RECORDS.to_vec();
            data.extend(std::iter::repeat_n(0x05, stray));
            let mut iter = records(&data);
            assert_eq!(iter.by_ref().filter(Result::is_ok).count(), 2);
            assert_eq!(iter.stray_bytes(), stray);
        }
        let mut iter = records(&[0x05, 0x00, 0x00]);
        assert_eq!(iter.stray_bytes(), 0);
        assert!(iter.next().is_none());
        assert_eq!(iter.stray_bytes(), 3);
    }

    #[test]
    fn size_out_of_bounds_ends_the_file() {
        for size in [0_i32, 3, -1, i32::MIN, 0x4000_0001] {
            let mut data = TWO_RECORDS[..9].to_vec();
            data.extend(size.to_le_bytes());
            data.extend(&TWO_RECORDS[..9]);
            let mut iter = records(&data);
            assert!(iter.next().unwrap().is_ok());
            assert_eq!(
                iter.next(),
                Some(Err(FramingError::SizeOutOfBounds {
                    index: 1,
                    offset: 9
                }))
            );
            assert!(iter.next().is_none());
            assert_eq!(iter.stray_bytes(), 0);
        }
    }

    #[test]
    fn largest_record_size_is_accepted_and_then_found_truncated() {
        // 0x4000_0000 passes the bound (inclusive, 02 §1.2); the body is
        // simply not there. Nothing of that size is allocated.
        let mut data = 0x4000_0000_i32.to_le_bytes().to_vec();
        data.extend([5, 0, 0, 0, 0x2A]);
        let error = records(&data).next().unwrap().unwrap_err();
        assert_eq!(
            error,
            FramingError::Truncated {
                index: 0,
                offset: 0,
                available: 5
            }
        );
        assert_eq!((error.index(), error.offset()), (0, 0));
        assert_eq!(
            error.to_string(),
            "record 0 at offset 0: the file ends inside the record (5 bytes left)"
        );
    }

    #[test]
    fn truncated_body_ends_the_file_and_keeps_what_came_before() {
        for cut in 13..TWO_RECORDS.len() {
            let mut iter = records(&TWO_RECORDS[..cut]);
            assert!(iter.next().unwrap().is_ok());
            assert_eq!(
                iter.next(),
                Some(Err(FramingError::Truncated {
                    index: 1,
                    offset: 9,
                    available: cut - 13
                }))
            );
            assert!(iter.next().is_none());
        }
    }

    #[test]
    fn debug_output_shows_no_bytes() {
        let record = records(&TWO_RECORDS).next().unwrap().unwrap();
        assert_eq!(
            format!("{record:?}"),
            "RawRecord { index: 0, offset: 0, len: 5 }"
        );
    }
}
