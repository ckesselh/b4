//! A sequential reader for the fixed-layout structures of the container: the
//! header (01 §3.1) and directory entries (01 §7.1).

/// Takes one field after the other from a byte slice. A read that does not
/// fit returns `None` and consumes nothing.
pub(super) struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    /// Start reading at the beginning of `bytes`.
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    /// The next `N` bytes.
    pub(super) fn bytes<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }

    /// The next byte.
    pub(super) fn u8(&mut self) -> Option<u8> {
        self.bytes().map(|[byte]| byte)
    }

    /// The next little-endian `i32` (01 §1).
    pub(super) fn i32(&mut self) -> Option<i32> {
        self.bytes().map(i32::from_le_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fields_in_order_and_stops_at_the_end() {
        let mut fields = Fields::new(&[1, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 7, 8, 9]);
        assert_eq!(fields.i32(), Some(1));
        assert_eq!(fields.i32(), Some(-1));
        assert_eq!(fields.u8(), Some(7));
        // Three bytes are needed, two are left: nothing is consumed.
        assert_eq!(fields.bytes::<3>(), None);
        assert_eq!(fields.bytes::<2>(), Some([8, 9]));
        assert_eq!(fields.u8(), None);
        assert_eq!(fields.i32(), None);
    }
}
