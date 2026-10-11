//! The 512-byte container header (01 §3).
//!
//! The header is stored in the clear, so everything here is available without
//! the password.

use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::str::FromStr;

use super::error::VaultError;
use super::fields::Fields;

/// Size of the header in bytes (01 §3.1). It occupies the start of a slot of
/// one cluster.
pub const HEADER_SIZE: usize = 512;

/// The four magic bytes: `"SUB"` and 0x01 (01 §3.1).
const MAGIC: [u8; 4] = [0x53, 0x55, 0x42, 0x01];

/// Seconds between 1970-01-01 and 2000-01-01, the epoch of header timestamps
/// (01 §3.4).
const EPOCH_2000_UNIX: i64 = 946_684_800;

/// The `type_guid` of a Banking4 data vault,
/// `6ab4f284-82a0-46fd-83f4-438632912ba7` (01 §3.5). The container layer
/// never checks it and the value is not confirmed (01 §14, question 6), so a
/// different value is a remark, not an error.
pub const BANKING4_TYPE_GUID: Guid = Guid([
    0x84, 0xf2, 0xb4, 0x6a, 0xa0, 0x82, 0xfd, 0x46, 0x83, 0xf4, 0x43, 0x86, 0x32, 0x91, 0x2b, 0xa7,
]);

/// Bit 0 of `security_flags`: the vault is bound to a device (01 §3.6).
const FLAG_DEVICE_BOUND: u32 = 1;

/// The decoded container header.
///
/// Every field of 01 §3.1 is kept as stored and exposed under the name the
/// format document gives it. Parsing validates the magic bytes only, exactly
/// like Banking4 (01 §3.2): an unknown cipher or an odd flag value
/// is reported by the accessors, not rejected here, so `b4 info` can show it.
#[derive(Clone, PartialEq, Eq)]
pub struct Header {
    raw_cluster_size: i32,
    created_at: Timestamp,
    root_dir_cluster: i32,
    security_flags: u32,
    cipher_id: i32,
    kdf_rounds: i32,
    salt: [u8; 32],
    salt_check: [u8; 32],
    legacy_area: [u8; 20],
    file_guid: Guid,
    type_guid: Guid,
    failed_attempts: i32,
    token_salt: [u8; 32],
    token_check: [u8; 32],
    modified_at: Timestamp,
    reserved: [u8; 40],
    wrapped_key: [u8; 256],
}

impl Header {
    /// Read the header of the vault at `path`.
    ///
    /// Reads the first 512 bytes of the file and nothing else, with read-only
    /// access. No password is needed.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, VaultError> {
        let path = path.as_ref();
        let io = |source| VaultError::Io {
            path: path.to_path_buf(),
            source,
        };
        let mut file = File::open(path).map_err(io)?;
        let mut buf = [0u8; HEADER_SIZE];
        match file.read_exact(&mut buf) {
            Ok(()) => Self::parse(&buf),
            // 01 §3.2: fewer than 512 bytes is "not a container".
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => Err(VaultError::NotAVault),
            Err(e) => Err(io(e)),
        }
    }

    /// Decode a header from the first 512 bytes of `bytes`.
    ///
    /// Fails with [`VaultError::NotAVault`] if there are fewer than 512 bytes
    /// or the magic differs, and with [`VaultError::UnsupportedVersion`] if
    /// only the fourth magic byte differs.
    pub fn parse(bytes: &[u8]) -> Result<Self, VaultError> {
        let mut fields = Fields::new(bytes.get(..HEADER_SIZE).ok_or(VaultError::NotAVault)?);
        let magic: [u8; 4] = fields.bytes().ok_or(VaultError::NotAVault)?;
        match magic {
            MAGIC => {}
            [0x53, 0x55, 0x42, version] => return Err(VaultError::UnsupportedVersion { version }),
            _ => return Err(VaultError::NotAVault),
        }
        // The slice is exactly 512 bytes and the fields below add up to 508,
        // so none of these reads can come up short.
        Self::parse_fields(&mut fields).ok_or(VaultError::NotAVault)
    }

    /// The fields after the magic, in the order of the table in 01 §3.1.
    fn parse_fields(f: &mut Fields<'_>) -> Option<Self> {
        Some(Self {
            raw_cluster_size: f.i32()?,
            created_at: Timestamp(f.i32()?),
            root_dir_cluster: f.i32()?,
            security_flags: u32::from_le_bytes(f.bytes()?),
            cipher_id: f.i32()?,
            kdf_rounds: f.i32()?,
            salt: f.bytes()?,
            salt_check: f.bytes()?,
            legacy_area: f.bytes()?,
            file_guid: Guid(f.bytes()?),
            type_guid: Guid(f.bytes()?),
            failed_attempts: f.i32()?,
            token_salt: f.bytes()?,
            token_check: f.bytes()?,
            modified_at: Timestamp(f.i32()?),
            reserved: f.bytes()?,
            wrapped_key: f.bytes()?,
        })
    }

    /// The format version: the fourth magic byte, 1 in every known vault
    /// (01 §14, question 4).
    pub fn format_version(&self) -> u8 {
        MAGIC[3]
    }

    /// The allocation unit in bytes, after the coercion of 01 §3.3: 1024,
    /// 2048, 4096, 8192, 16384, 32768 and 65536 are taken as stored, every
    /// other stored value means 512.
    pub fn cluster_size(&self) -> u32 {
        match self.raw_cluster_size {
            1024 => 1024,
            2048 => 2048,
            4096 => 4096,
            8192 => 8192,
            16384 => 16384,
            32768 => 32768,
            65536 => 65536,
            _ => 512,
        }
    }

    /// `cluster_size` exactly as stored, before coercion.
    pub fn raw_cluster_size(&self) -> i32 {
        self.raw_cluster_size
    }

    /// When the vault was created (01 §3.4).
    pub fn created_at(&self) -> Timestamp {
        self.created_at
    }

    /// When the header was last written (01 §3.4). Banking4 rewrites the
    /// header when it closes a vault it changed, so this stamp tells whether a
    /// vault changed since it was last read.
    pub fn modified_at(&self) -> Timestamp {
        self.modified_at
    }

    /// First cluster of the root directory (01 §7).
    pub fn root_dir_cluster(&self) -> i32 {
        self.root_dir_cluster
    }

    /// The `security_flags` bit set as stored (01 §3.6). Only bit 0 has a
    /// known meaning, see [`is_device_bound`](Self::is_device_bound).
    pub fn security_flags(&self) -> u32 {
        self.security_flags
    }

    /// Whether the vault is bound to a device: bit 0 of `security_flags`.
    /// Such a vault needs a device GUID in addition to the password (01 §5.6).
    pub fn is_device_bound(&self) -> bool {
        self.security_flags & FLAG_DEVICE_BOUND != 0
    }

    /// Whether `type_guid` is [`BANKING4_TYPE_GUID`], the type Banking4 gives
    /// its data vaults (01 §3.5).
    pub fn is_banking4_vault(&self) -> bool {
        self.type_guid == BANKING4_TYPE_GUID
    }

    /// `cipher_id` as stored (01 §3.7).
    pub fn cipher_id(&self) -> i32 {
        self.cipher_id
    }

    /// The cipher of file contents, or `None` if `cipher_id` is not one of
    /// the three known ciphers.
    pub fn cipher(&self) -> Option<Cipher> {
        Cipher::from_id(self.cipher_id)
    }

    /// `kdf_rounds` as stored: the number of key-stretching rounds (01 §5.2).
    pub fn kdf_rounds(&self) -> i32 {
        self.kdf_rounds
    }

    /// The number of key-stretching rounds that are actually run: zero or a
    /// negative stored value means none (01 §5.2).
    pub fn effective_kdf_rounds(&self) -> u32 {
        u32::try_from(self.kdf_rounds).unwrap_or(0)
    }

    /// `salt`: input of key stretching and of the password check (01 §5.2).
    pub fn salt(&self) -> &[u8; 32] {
        &self.salt
    }

    /// `salt_check`: the salt encrypted with the pass key (01 §5.3).
    pub fn salt_check(&self) -> &[u8; 32] {
        &self.salt_check
    }

    /// `legacy_area`: 20 bytes nothing reads; zero in files written by
    /// Banking4 (01 §3.2).
    pub fn legacy_area(&self) -> &[u8; 20] {
        &self.legacy_area
    }

    /// `file_guid`: the identity of this vault file, assigned at creation
    /// (01 §3.5). This is the vault id b4 records. It is reported as stored:
    /// an all-zero value is not replaced.
    pub fn file_guid(&self) -> Guid {
        self.file_guid
    }

    /// `type_guid`: the content type chosen by the creating application
    /// (01 §3.5). Not checked by anything.
    pub fn type_guid(&self) -> Guid {
        self.type_guid
    }

    /// `failed_attempts`: a counter of wrong password attempts that
    /// Banking4 never maintains (01 §3.8).
    pub fn failed_attempts(&self) -> i32 {
        self.failed_attempts
    }

    /// `token_salt`: all zero unless an access token was issued (01 §5.6).
    pub fn token_salt(&self) -> &[u8; 32] {
        &self.token_salt
    }

    /// `token_check`: what a presented access token is verified against
    /// (01 §5.6). b4 does not use tokens.
    pub fn token_check(&self) -> &[u8; 32] {
        &self.token_check
    }

    /// Whether an access token was issued for the vault: `token_salt` is not
    /// all zero (01 §5.6).
    pub fn has_access_token(&self) -> bool {
        self.token_salt != [0u8; 32]
    }

    /// `reserved`: 40 unused bytes (01 §3.1).
    pub fn reserved(&self) -> &[u8; 40] {
        &self.reserved
    }

    /// `wrapped_key`: the content key encrypted with the pass key (01 §5.4).
    pub fn wrapped_key(&self) -> &[u8; 256] {
        &self.wrapped_key
    }
}

/// Shows the scalar fields. The salts and the wrapped key are public data but
/// would drown everything else, so they are left out.
impl fmt::Debug for Header {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Header")
            .field("cluster_size", &self.cluster_size())
            .field("raw_cluster_size", &self.raw_cluster_size)
            .field("created_at", &self.created_at)
            .field("modified_at", &self.modified_at)
            .field("root_dir_cluster", &self.root_dir_cluster)
            .field("security_flags", &self.security_flags)
            .field("cipher_id", &self.cipher_id)
            .field("kdf_rounds", &self.kdf_rounds)
            .field("file_guid", &format_args!("{}", self.file_guid))
            .field("type_guid", &format_args!("{}", self.type_guid))
            .field("failed_attempts", &self.failed_attempts)
            .field("has_access_token", &self.has_access_token())
            .finish_non_exhaustive()
    }
}

/// The cipher that protects file contents (01 §3.7). The password layer always
/// uses AES-256, whatever this says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cipher {
    /// AES with a 128-bit key, `cipher_id` 1.
    Aes128,
    /// AES with a 192-bit key, `cipher_id` 2.
    Aes192,
    /// AES with a 256-bit key, `cipher_id` 3. What Banking4 uses.
    Aes256,
}

impl Cipher {
    /// The cipher for a stored `cipher_id`, or `None` for an unknown id.
    /// Id 0 ("none") is unknown in this sense: no such vault can be read
    /// (01 §14, question 5).
    pub fn from_id(cipher_id: i32) -> Option<Self> {
        match cipher_id {
            1 => Some(Self::Aes128),
            2 => Some(Self::Aes192),
            3 => Some(Self::Aes256),
            _ => None,
        }
    }

    /// The `cipher_id` that stands for this cipher.
    pub fn id(self) -> i32 {
        match self {
            Self::Aes128 => 1,
            Self::Aes192 => 2,
            Self::Aes256 => 3,
        }
    }

    /// Length of the content key in bytes.
    pub fn key_len(self) -> usize {
        match self {
            Self::Aes128 => 16,
            Self::Aes192 => 24,
            Self::Aes256 => 32,
        }
    }

    /// The usual name of the cipher, for example `AES-256`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Aes128 => "AES-128",
            Self::Aes192 => "AES-192",
            Self::Aes256 => "AES-256",
        }
    }
}

impl fmt::Display for Cipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A header timestamp: whole seconds since 2000-01-01T00:00:00 UTC, where 0
/// means "not set" (01 §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i32);

impl Timestamp {
    /// Wrap a stored value.
    pub fn from_raw(seconds_since_2000: i32) -> Self {
        Self(seconds_since_2000)
    }

    /// The value as stored: seconds since 2000-01-01T00:00:00 UTC.
    pub fn raw(self) -> i32 {
        self.0
    }

    /// Whether the stamp holds a time. The stored value 0 means "not set".
    pub fn is_set(self) -> bool {
        self.0 != 0
    }

    /// The time as seconds since 1970-01-01T00:00:00 UTC, or `None` if not
    /// set.
    pub fn unix_seconds(self) -> Option<i64> {
        self.is_set().then(|| EPOCH_2000_UNIX + i64::from(self.0))
    }

    /// The time as a UTC calendar date and time, or `None` if not set.
    pub fn utc(self) -> Option<UtcDateTime> {
        self.unix_seconds().map(UtcDateTime::from_unix_seconds)
    }

    /// The time as ISO 8601 text such as `2026-01-01T00:00:00Z`, or `None` if
    /// not set.
    pub fn to_iso8601(self) -> Option<String> {
        self.utc().map(|t| t.to_string())
    }
}

/// A calendar date and time of day in UTC. Its `Display` form is ISO 8601:
/// `2026-01-01T00:00:00Z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcDateTime {
    /// Calendar year.
    pub year: i32,
    /// Month, 1 to 12.
    pub month: u8,
    /// Day of the month, 1 to 31.
    pub day: u8,
    /// Hour, 0 to 23.
    pub hour: u8,
    /// Minute, 0 to 59.
    pub minute: u8,
    /// Second, 0 to 59.
    pub second: u8,
}

impl UtcDateTime {
    /// Convert seconds since 1970-01-01T00:00:00 UTC (proleptic Gregorian
    /// calendar, no leap seconds).
    ///
    /// Header stamps span 1931 to 2068; the conversion is exact for any year
    /// that fits `i32` and saturates beyond.
    pub fn from_unix_seconds(seconds: i64) -> Self {
        let days = seconds.div_euclid(86_400);
        let second_of_day = seconds.rem_euclid(86_400);

        // Days to civil date, counting from 0000-03-01 so that the leap day
        // is the last day of the shifted year. 146097 days are one 400-year
        // cycle (an "era").
        let shifted = days.saturating_add(719_468);
        let era = shifted.div_euclid(146_097);
        let day_of_era = shifted.rem_euclid(146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        let year = year_of_era + era * 400 + i64::from(month <= 2);

        // Every value below is within the range of its type by construction
        // (month 1..=12, day 1..=31, time of day < 86400); only the year can
        // exceed it, for inputs no header can hold.
        Self {
            year: i32::try_from(year).unwrap_or(if year < 0 { i32::MIN } else { i32::MAX }),
            month: month as u8,
            day: day as u8,
            hour: (second_of_day / 3_600) as u8,
            minute: (second_of_day % 3_600 / 60) as u8,
            second: (second_of_day % 60) as u8,
        }
    }
}

impl fmt::Display for UtcDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// A GUID in the Windows in-memory layout the container stores (01 §3.5): the
/// first three groups of the text form are little-endian, the last two are in
/// reading order.
///
/// `Display` gives the usual lower-case text form,
/// `01234567-89ab-cdef-0123-456789abcdef`; `FromStr` parses it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Guid([u8; 16]);

impl Guid {
    /// Wrap 16 bytes in the stored layout.
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The 16 bytes in the stored layout.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Whether all 16 bytes are zero (the empty GUID).
    pub fn is_nil(&self) -> bool {
        self.0 == [0u8; 16]
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a0, a1, a2, a3, b0, b1, c0, c1, d0, d1, e0, e1, e2, e3, e4, e5] = self.0;
        write!(
            f,
            "{a3:02x}{a2:02x}{a1:02x}{a0:02x}-{b1:02x}{b0:02x}-{c1:02x}{c0:02x}-\
             {d0:02x}{d1:02x}-{e0:02x}{e1:02x}{e2:02x}{e3:02x}{e4:02x}{e5:02x}"
        )
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Guid({self})")
    }
}

/// The text is not a GUID of the form `01234567-89ab-cdef-0123-456789abcdef`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a GUID of the form 01234567-89ab-cdef-0123-456789abcdef")]
pub struct ParseGuidError;

impl FromStr for Guid {
    type Err = ParseGuidError;

    /// Parse the hyphenated text form; hex digits in either case.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.len() != 36 {
            return Err(ParseGuidError);
        }
        // The 32 hex digits in reading order, two per byte.
        let mut reading = [0u8; 16];
        let mut digits = 0usize;
        for (position, byte) in text.bytes().enumerate() {
            if matches!(position, 8 | 13 | 18 | 23) {
                if byte != b'-' {
                    return Err(ParseGuidError);
                }
                continue;
            }
            let digit = char::from(byte).to_digit(16).ok_or(ParseGuidError)?;
            let slot = reading.get_mut(digits / 2).ok_or(ParseGuidError)?;
            // A hex digit is below 16, so it fits the low half of the byte.
            *slot = (*slot << 4) | digit as u8;
            digits += 1;
        }
        let [a0, a1, a2, a3, b0, b1, c0, c1, tail @ ..] = reading;
        let [d0, d1, e0, e1, e2, e3, e4, e5] = tail;
        Ok(Self([
            a3, a2, a1, a0, b1, b0, c1, c0, d0, d1, e0, e1, e2, e3, e4, e5,
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header of the worked example, 01 §12.2, built from the printed
    /// values.
    fn example_header() -> Vec<u8> {
        let mut h = vec![0u8; HEADER_SIZE];
        h[0x000..0x004].copy_from_slice(&MAGIC);
        h[0x004..0x008].copy_from_slice(&512i32.to_le_bytes());
        h[0x008..0x00C].copy_from_slice(&[0x80, 0x75, 0xe8, 0x30]);
        h[0x00C..0x010].copy_from_slice(&1i32.to_le_bytes());
        h[0x014..0x018].copy_from_slice(&3i32.to_le_bytes());
        h[0x018..0x01C].copy_from_slice(&2i32.to_le_bytes());
        for (i, b) in h[0x01C..0x03C].iter_mut().enumerate() {
            *b = 0x10 + i as u8;
        }
        h[0x070..0x080].copy_from_slice(&[
            0x67, 0x45, 0x23, 0x01, 0xab, 0x89, 0xef, 0xcd, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ]);
        h[0x080..0x090].copy_from_slice(&[
            0x11, 0x11, 0x11, 0x11, 0x22, 0x22, 0x33, 0x33, 0x44, 0x44, 0x55, 0x55, 0x55, 0x55,
            0x55, 0x55,
        ]);
        h[0x0D4..0x0D8].copy_from_slice(&[0x90, 0x83, 0xe8, 0x30]);
        h
    }

    #[test]
    fn parses_the_worked_example_header() {
        let header = Header::parse(&example_header()).unwrap();
        assert_eq!(header.format_version(), 1);
        assert_eq!(header.cluster_size(), 512);
        assert_eq!(header.raw_cluster_size(), 512);
        assert_eq!(header.created_at().raw(), 820_540_800);
        assert_eq!(
            header.created_at().to_iso8601().as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(header.modified_at().raw(), 820_544_400);
        assert_eq!(
            header.modified_at().to_iso8601().as_deref(),
            Some("2026-01-01T01:00:00Z")
        );
        assert_eq!(header.root_dir_cluster(), 1);
        assert_eq!(header.security_flags(), 0);
        assert!(!header.is_device_bound());
        assert_eq!(header.cipher_id(), 3);
        assert_eq!(header.cipher(), Some(Cipher::Aes256));
        assert_eq!(header.kdf_rounds(), 2);
        assert_eq!(header.effective_kdf_rounds(), 2);
        assert_eq!(header.salt()[0], 0x10);
        assert_eq!(header.salt()[31], 0x2f);
        assert_eq!(
            header.file_guid().to_string(),
            "01234567-89ab-cdef-0123-456789abcdef"
        );
        assert_eq!(
            header.type_guid().to_string(),
            "11111111-2222-3333-4444-555555555555"
        );
        assert_eq!(header.failed_attempts(), 0);
        assert!(!header.has_access_token());
        assert_eq!(header.legacy_area(), &[0u8; 20]);
        assert_eq!(header.reserved(), &[0u8; 40]);
    }

    #[test]
    fn every_field_is_read_from_its_offset() {
        // Fill each field with a value derived from its offset and check that
        // it comes back from the right accessor (01 §3.1).
        let mut h = vec![0u8; HEADER_SIZE];
        for (i, b) in h.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        h[..4].copy_from_slice(&MAGIC);
        let header = Header::parse(&h).unwrap();
        let i32_at = |o: usize| i32::from_le_bytes(h[o..o + 4].try_into().unwrap());
        assert_eq!(header.raw_cluster_size(), i32_at(0x004));
        assert_eq!(header.created_at().raw(), i32_at(0x008));
        assert_eq!(header.root_dir_cluster(), i32_at(0x00C));
        assert_eq!(header.security_flags(), i32_at(0x010) as u32);
        assert_eq!(header.cipher_id(), i32_at(0x014));
        assert_eq!(header.kdf_rounds(), i32_at(0x018));
        assert_eq!(header.salt()[..], h[0x01C..0x03C]);
        assert_eq!(header.salt_check()[..], h[0x03C..0x05C]);
        assert_eq!(header.legacy_area()[..], h[0x05C..0x070]);
        assert_eq!(header.file_guid().as_bytes()[..], h[0x070..0x080]);
        assert_eq!(header.type_guid().as_bytes()[..], h[0x080..0x090]);
        assert_eq!(header.failed_attempts(), i32_at(0x090));
        assert_eq!(header.token_salt()[..], h[0x094..0x0B4]);
        assert_eq!(header.token_check()[..], h[0x0B4..0x0D4]);
        assert_eq!(header.modified_at().raw(), i32_at(0x0D4));
        assert_eq!(header.reserved()[..], h[0x0D8..0x100]);
        assert_eq!(header.wrapped_key()[..], h[0x100..0x200]);
        assert!(header.has_access_token());
    }

    #[test]
    fn rejects_short_input_and_wrong_magic() {
        let good = example_header();
        assert!(matches!(
            Header::parse(&good[..511]),
            Err(VaultError::NotAVault)
        ));
        assert!(matches!(Header::parse(&[]), Err(VaultError::NotAVault)));

        let mut first = good.clone();
        first[0] = 0x54;
        assert!(matches!(Header::parse(&first), Err(VaultError::NotAVault)));

        let mut fourth = good.clone();
        fourth[3] = 2;
        assert!(matches!(
            Header::parse(&fourth),
            Err(VaultError::UnsupportedVersion { version: 2 })
        ));

        // Longer input is fine: only the first 512 bytes are looked at.
        let mut long = good;
        long.extend_from_slice(&[0xAA; 100]);
        assert!(Header::parse(&long).is_ok());
    }

    #[test]
    fn cluster_size_is_coerced() {
        // 01 §3.3 and the cases of §13.2.
        let mut h = example_header();
        for (stored, effective) in [
            (0, 512),
            (256, 512),
            (512, 512),
            (777, 512),
            (-1, 512),
            (1024, 1024),
            (2048, 2048),
            (4096, 4096),
            (8192, 8192),
            (16384, 16384),
            (32768, 32768),
            (65536, 65536),
            (131_072, 512),
            (i32::MAX, 512),
            (i32::MIN, 512),
        ] {
            h[4..8].copy_from_slice(&i32::to_le_bytes(stored));
            let header = Header::parse(&h).unwrap();
            assert_eq!(header.cluster_size(), effective, "stored {stored}");
            assert_eq!(header.raw_cluster_size(), stored);
        }
    }

    #[test]
    fn flags_cipher_and_rounds_are_interpreted() {
        let mut h = example_header();
        for (flags, bound) in [
            (0u32, false),
            (1, true),
            (2, false),
            (3, true),
            (0x7FFF_FFFE, false),
        ] {
            h[0x10..0x14].copy_from_slice(&flags.to_le_bytes());
            let header = Header::parse(&h).unwrap();
            assert_eq!(header.security_flags(), flags);
            assert_eq!(header.is_device_bound(), bound, "flags {flags}");
        }
        for (id, cipher) in [
            (0, None),
            (1, Some(Cipher::Aes128)),
            (2, Some(Cipher::Aes192)),
            (3, Some(Cipher::Aes256)),
            (4, None),
            (-1, None),
        ] {
            h[0x14..0x18].copy_from_slice(&i32::to_le_bytes(id));
            let header = Header::parse(&h).unwrap();
            assert_eq!(header.cipher_id(), id);
            assert_eq!(header.cipher(), cipher);
            if let Some(c) = cipher {
                assert_eq!(c.id(), id);
            }
        }
        for (stored, effective) in [(0, 0), (-5, 0), (i32::MIN, 0), (100_000, 100_000)] {
            h[0x18..0x1C].copy_from_slice(&i32::to_le_bytes(stored));
            let header = Header::parse(&h).unwrap();
            assert_eq!(header.kdf_rounds(), stored);
            assert_eq!(header.effective_kdf_rounds(), effective);
        }
        assert_eq!(Cipher::Aes128.key_len(), 16);
        assert_eq!(Cipher::Aes192.key_len(), 24);
        assert_eq!(Cipher::Aes256.key_len(), 32);
        assert_eq!(Cipher::Aes256.to_string(), "AES-256");
    }

    #[test]
    fn timestamps_convert_to_utc() {
        // 01 §3.4.
        assert_eq!(Timestamp::from_raw(0).utc(), None);
        assert_eq!(Timestamp::from_raw(0).unix_seconds(), None);
        assert_eq!(Timestamp::from_raw(0).to_iso8601(), None);
        let iso = |raw: i32| Timestamp::from_raw(raw).to_iso8601().unwrap();
        assert_eq!(iso(1), "2000-01-01T00:00:01Z");
        assert_eq!(iso(820_540_800), "2026-01-01T00:00:00Z");
        // 2000 is a leap year: day 59 is 29 February.
        assert_eq!(iso(59 * 86_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso(60 * 86_400 - 1), "2000-02-29T23:59:59Z");
        assert_eq!(iso(366 * 86_400), "2001-01-01T00:00:00Z");
        // The i32 range ends on 2068-01-19.
        assert_eq!(iso(i32::MAX), "2068-01-19T03:14:07Z");
        // Negative values are not written, but must not misbehave.
        assert_eq!(iso(-1), "1999-12-31T23:59:59Z");
        assert_eq!(iso(i32::MIN), "1931-12-13T20:45:52Z");
        assert_eq!(
            Timestamp::from_raw(820_540_800).unix_seconds(),
            Some(1_767_225_600)
        );
    }

    #[test]
    fn date_conversion_matches_known_days() {
        let at = |s: i64| UtcDateTime::from_unix_seconds(s).to_string();
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(-1), "1969-12-31T23:59:59Z");
        assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(at(4_107_542_400), "2100-03-01T00:00:00Z");
        assert_eq!(at(-2_203_891_200), "1900-03-01T00:00:00Z");
        // Extremes saturate instead of overflowing.
        let _ = UtcDateTime::from_unix_seconds(i64::MAX);
        let _ = UtcDateTime::from_unix_seconds(i64::MIN);
    }

    #[test]
    fn guid_text_uses_the_windows_layout() {
        // 01 §3.5.
        let stored = [
            0x67, 0x45, 0x23, 0x01, 0xab, 0x89, 0xef, 0xcd, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        let guid = Guid::from_bytes(stored);
        assert_eq!(guid.to_string(), "01234567-89ab-cdef-0123-456789abcdef");
        assert_eq!(
            "01234567-89ab-cdef-0123-456789abcdef".parse::<Guid>(),
            Ok(guid)
        );
        assert_eq!(
            "01234567-89AB-CDEF-0123-456789ABCDEF".parse::<Guid>(),
            Ok(guid)
        );
        // The device GUID of 01 §12.3.
        let device: Guid = "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap();
        assert_eq!(
            device.as_bytes(),
            &[
                0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
        assert!(!guid.is_nil());
        assert!(Guid::from_bytes([0; 16]).is_nil());
        assert_eq!(
            format!("{guid:?}"),
            "Guid(01234567-89ab-cdef-0123-456789abcdef)"
        );
    }

    #[test]
    fn the_banking4_type_is_the_documented_guid() {
        // 01 §3.5.
        let text = "6ab4f284-82a0-46fd-83f4-438632912ba7";
        assert_eq!(BANKING4_TYPE_GUID.to_string(), text);
        assert_eq!(text.parse::<Guid>().unwrap(), BANKING4_TYPE_GUID);
    }

    #[test]
    fn guid_parsing_rejects_other_text() {
        for bad in [
            "",
            "01234567-89ab-cdef-0123-456789abcde",
            "01234567-89ab-cdef-0123-456789abcdef0",
            "0123456789abcdef0123456789abcdef0000",
            "01234567-89ab-cdef-0123_456789abcdef",
            "01234567-89ab-cdef-0123-456789abcdeg",
            "{1234567-89ab-cdef-0123-456789abcdef}",
            "01234567-89ab-cdef-0123-456789abcd\u{e9}",
        ] {
            assert_eq!(bad.parse::<Guid>(), Err(ParseGuidError), "{bad:?}");
        }
    }

    #[test]
    fn debug_output_leaves_out_the_key_fields() {
        let text = format!("{:?}", Header::parse(&example_header()).unwrap());
        assert!(text.contains("cluster_size: 512"));
        assert!(text.contains("01234567-89ab-cdef-0123-456789abcdef"));
        assert!(!text.contains("wrapped_key"));
        assert!(!text.contains("salt"));
    }
}
