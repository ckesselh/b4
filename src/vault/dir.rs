//! Directories (01 §7): chains of pages of 64-byte entries, stored in the
//! clear.

use super::fields::Fields;

/// Size of one directory slot in bytes (01 §7.1).
const ENTRY_SIZE: usize = 64;

/// Longest stored name in bytes (01 §7.1).
const MAX_NAME_LEN: usize = 54;

/// Flag bit 0: the contents are encrypted (01 §7.1).
const FLAG_ENCRYPTED: u8 = 1;
/// Flag bit 1: the entry is a directory.
const FLAG_DIRECTORY: u8 = 2;

/// Where an entry is stored: the cluster of its directory page and the number
/// of its slot in that page. No two entries of a vault share a place, which
/// makes it the identity of an entry (their names and chains can coincide in
/// a damaged vault).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Origin {
    page: u32,
    slot: usize,
}

impl Origin {
    pub(crate) fn new(page: u32, slot: usize) -> Self {
        Self { page, slot }
    }
}

/// One live entry of a directory: a file or a subdirectory (01 §7).
///
/// An entry is what a directory page says about a file: its name, size and
/// flags, and where its contents start. Getting an entry reads nothing of the
/// contents. Pass it back to the [`Vault`](super::Vault) it came from to read
/// the file or list the subdirectory; an entry is meaningless for any other
/// vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    name: String,
    name_bytes: Vec<u8>,
    size: u32,
    flags: u8,
    first_cluster: i32,
    origin: Origin,
}

impl Entry {
    /// The name as text. Names are UTF-8; a name that is not valid UTF-8
    /// (the writer cuts names at 54 bytes, even inside a character) shows
    /// U+FFFD for each broken sequence (01 §7.2).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name exactly as stored, up to 54 bytes. This is the identity of
    /// the entry; [`name`](Self::name) is its display form (01 §7.2).
    pub fn name_bytes(&self) -> &[u8] {
        &self.name_bytes
    }

    /// Length of the file in bytes. For a directory the stored value has no
    /// meaning (it is normally 0).
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Whether the entry is a directory (flag bit 1).
    pub fn is_dir(&self) -> bool {
        self.flags & FLAG_DIRECTORY != 0
    }

    /// Whether the file's contents are encrypted (flag bit 0). The choice is
    /// made per file; a directory's pages are never encrypted, whatever this
    /// says (01 §7.4).
    pub fn is_encrypted(&self) -> bool {
        self.flags & FLAG_ENCRYPTED != 0
    }

    /// The flags byte as stored. Bits other than 0 and 1 have no meaning
    /// (01 §7.4).
    pub fn flags(&self) -> u8 {
        self.flags
    }

    /// The stored reference to the first cluster of the entry's chain; -1 if
    /// it has none (01 §7.5). In a damaged vault a directory can lead back to
    /// one of its ancestors (01 §7.4); a caller need not guard against that,
    /// because [`Vault::read_dir`](super::Vault::read_dir) refuses a directory
    /// whose pages another entry has already been read from.
    pub fn first_cluster(&self) -> i32 {
        self.first_cluster
    }

    /// Where the entry is stored.
    pub(crate) fn origin(&self) -> Origin {
        self.origin
    }

    /// How an error message names the entry: the name is chosen by whoever
    /// wrote the vault, so control characters are escaped.
    pub(crate) fn label(&self) -> String {
        format!("entry {:?}", self.name)
    }

    /// Decode one 64-byte slot (01 §7.1, §7.2), stored at `origin`. `None`
    /// for a free slot and for a slot Banking4 treats as free.
    fn parse(slot: &[u8], origin: Origin) -> Option<Self> {
        let mut fields = Fields::new(slot);
        let first_cluster = fields.i32()?;
        let size = fields.i32()?;
        let flags = fields.u8()?;
        let name_len = usize::from(fields.u8()?);
        let name_area: [u8; MAX_NAME_LEN] = fields.bytes()?;

        if first_cluster == 0 {
            return None;
        }
        // A negative size or an over-long name leaves the slot unused in
        // Banking4; `get` covers the name length.
        let size = u32::try_from(size).ok()?;
        let name_bytes = name_area.get(..name_len)?;
        Some(Self {
            name: String::from_utf8_lossy(name_bytes).into_owned(),
            name_bytes: name_bytes.to_vec(),
            size,
            flags,
            first_cluster,
            origin,
        })
    }
}

/// The first entry of `entries` whose stored name is `name`, compared byte
/// for byte: case-sensitive, no normalisation, no trimming (01 §7.3).
///
/// A directory can hold several entries with the same name; like Banking4
/// this returns the first in listing order. An entry whose stored
/// name is not valid UTF-8 (a name that was cut inside a character) matches
/// no text: compare [`Entry::name_bytes`] or [`Entry::name`] yourself to
/// reach it.
pub fn find_entry<'a>(entries: &'a [Entry], name: &str) -> Option<&'a Entry> {
    entries
        .iter()
        .find(|entry| entry.name_bytes == name.as_bytes())
}

/// Every live entry of the directory pages `pages`, page by page in the order
/// given and slot by slot within a page (01 §7.2). Each page comes with the
/// number of its cluster, which is half of what identifies an entry.
///
/// The caller has walked the directory's chain to its end before calling
/// this, so that a chain that loops fails without a list of the entries it
/// repeats ever being built.
pub(crate) fn entries(pages: &[(u32, &[u8])]) -> Vec<Entry> {
    let mut entries = Vec::new();
    for &(page, data) in pages {
        let slots = data.chunks_exact(ENTRY_SIZE).enumerate();
        entries
            .extend(slots.filter_map(|(slot, bytes)| Entry::parse(bytes, Origin::new(page, slot))));
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a slot as if it were the first of the page in cluster 1.
    fn parse(slot: &[u8]) -> Option<Entry> {
        Entry::parse(slot, Origin::new(1, 0))
    }

    fn slot(first_cluster: i32, size: i32, flags: u8, name_len: u8, name: &[u8]) -> [u8; 64] {
        let mut s = [0u8; 64];
        s[0..4].copy_from_slice(&first_cluster.to_le_bytes());
        s[4..8].copy_from_slice(&size.to_le_bytes());
        s[8] = flags;
        s[9] = name_len;
        s[10..10 + name.len()].copy_from_slice(name);
        s
    }

    #[test]
    fn decodes_the_entries_of_the_worked_example() {
        // 01 §12.5.
        let hello = parse(&slot(2, 20, 1, 9, b"hello.txt")).unwrap();
        assert_eq!(hello.name(), "hello.txt");
        assert_eq!(hello.name_bytes(), b"hello.txt");
        assert_eq!(hello.size(), 20);
        assert_eq!(hello.first_cluster(), 2);
        assert!(hello.is_encrypted() && !hello.is_dir());

        let plain = parse(&slot(3, 5, 0, 9, b"plain.txt")).unwrap();
        assert!(!plain.is_encrypted() && !plain.is_dir());

        let dir = parse(&slot(4, 0, 2, 3, b"Dir")).unwrap();
        assert!(dir.is_dir() && !dir.is_encrypted());
        assert_eq!(dir.flags(), 2);
    }

    #[test]
    fn free_and_unusable_slots_are_skipped() {
        // 01 §7.2.
        assert_eq!(parse(&[0u8; 64]), None);
        assert_eq!(parse(&slot(0, 5, 1, 1, b"x")), None);
        assert_eq!(parse(&slot(2, -1, 1, 1, b"x")), None);
        assert_eq!(parse(&slot(2, i32::MIN, 1, 1, b"x")), None);
        assert_eq!(parse(&slot(2, 5, 1, 55, b"x")), None);
        assert_eq!(parse(&slot(2, 5, 1, 255, b"x")), None);
        // Not a whole slot.
        assert_eq!(parse(&[1u8; 63]), None);
    }

    #[test]
    fn odd_but_live_entries_are_kept() {
        // 01 §7.2, §7.4, §7.5 and the cases of §13.2.
        let empty_name = parse(&slot(2, 5, 1, 0, b"ignored")).unwrap();
        assert_eq!(empty_name.name(), "");
        assert_eq!(empty_name.name_bytes(), b"");

        let being_written = parse(&slot(-1, 0, 1, 1, b"x")).unwrap();
        assert_eq!(being_written.first_cluster(), -1);
        assert_eq!(being_written.size(), 0);

        let far = parse(&slot(i32::MAX, i32::MAX, 0, 1, b"x")).unwrap();
        assert_eq!(far.size(), i32::MAX as u32);

        // name_len decides, not the zero padding.
        assert_eq!(parse(&slot(2, 0, 0, 3, b"abcdef")).unwrap().name(), "abc");
        let padded = parse(&slot(2, 0, 0, 5, b"abc")).unwrap();
        assert_eq!(padded.name_bytes(), b"abc\0\0");
        let embedded = parse(&slot(2, 0, 0, 3, b"a\0c")).unwrap();
        assert_eq!(embedded.name(), "a\0c");

        // Bits above 1 are ignored.
        for (flags, encrypted, dir) in [
            (0x04, false, false),
            (0x05, true, false),
            (0x81, true, false),
            (0x03, true, true),
            (0xFE, false, true),
        ] {
            let entry = parse(&slot(2, 0, flags, 1, b"x")).unwrap();
            assert_eq!(
                (entry.is_encrypted(), entry.is_dir(), entry.flags()),
                (encrypted, dir, flags)
            );
        }

        let full = parse(&slot(2, 0, 0, 54, &[b'n'; 54])).unwrap();
        assert_eq!(full.name_bytes().len(), 54);
    }

    #[test]
    fn broken_names_get_replacement_characters() {
        // 01 §7.2: one U+FFFD per broken sequence; the stored bytes stay.
        let cut_euro = parse(&slot(2, 0, 0, 3, &[b'a', b'b', 0xE2])).unwrap();
        assert_eq!(cut_euro.name(), "ab\u{FFFD}");
        assert_eq!(cut_euro.name_bytes(), &[b'a', b'b', 0xE2]);

        let cut_emoji = parse(&slot(2, 0, 0, 3, &[b'a', 0xF0, 0x9F])).unwrap();
        assert_eq!(cut_emoji.name(), "a\u{FFFD}");

        let lone_ff = parse(&slot(2, 0, 0, 3, &[b'a', 0xFF, b'b'])).unwrap();
        assert_eq!(lone_ff.name(), "a\u{FFFD}b");

        let two_broken = parse(&slot(2, 0, 0, 3, &[0x80, 0xC3, 0x41])).unwrap();
        assert_eq!(two_broken.name(), "\u{FFFD}\u{FFFD}A");
    }

    #[test]
    fn lookup_compares_stored_bytes_and_takes_the_first_match() {
        // 01 §7.3.
        let entries: Vec<Entry> = [
            slot(2, 1, 0, 4, b"Name"),
            slot(3, 2, 0, 4, b"name"),
            slot(4, 3, 0, 4, b"name"),
            slot(5, 4, 0, 5, b"name "),
            slot(6, 5, 0, 3, &[b'a', b'b', 0xE2]),
            slot(7, 6, 0, 3, "\u{e4}".repeat(2).as_bytes().get(..3).unwrap()),
        ]
        .iter()
        .filter_map(|s| parse(s))
        .collect();
        assert_eq!(entries.len(), 6);
        assert_eq!(find_entry(&entries, "name").unwrap().size(), 2);
        assert_eq!(find_entry(&entries, "Name").unwrap().size(), 1);
        assert_eq!(find_entry(&entries, "name ").unwrap().size(), 4);
        assert!(find_entry(&entries, "NAME").is_none());
        assert!(find_entry(&entries, " name").is_none());
        assert!(find_entry(&entries, "").is_none());
        // A name cut inside a character has no valid text of its own: its
        // display form is not its stored form, so text never finds it.
        assert!(find_entry(&entries, "ab\u{FFFD}").is_none());
    }
}
