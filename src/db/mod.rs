//! The DuckDB database: schema, writer and queries.
//!
//! `b4 decrypt` writes the database ([`mod@write`], [`schema`]); every other
//! command opens it read-only and sandboxed ([`read`]).

pub mod read;
pub mod schema;
pub mod write;

/// Version of the database layout. A database written with another version is
/// rejected with the advice to run `b4 decrypt` again.
pub const SCHEMA_VERSION: u32 = 1;

/// Name of the key/value table that describes the database itself:
/// `meta(key VARCHAR, value VARCHAR)`.
pub const META_TABLE: &str = "meta";

/// Name of the table of reader warnings: `warnings(message VARCHAR)`.
pub const WARNINGS_TABLE: &str = "warnings";

/// Keys of the `meta` table. Every value is text.
pub mod meta_key {
    /// [`SCHEMA_VERSION`](super::SCHEMA_VERSION) as a decimal number.
    pub const SCHEMA_VERSION: &str = "schema_version";
    /// Version of b4 that wrote the database.
    pub const B4_VERSION: &str = "b4_version";
    /// Absolute path of the vault the database was decrypted from.
    pub const VAULT_PATH: &str = "vault_path";
    /// The vault's id (file GUID) as lower-case hyphenated text.
    pub const VAULT_ID: &str = "vault_id";
    /// Creation time of the vault, UTC ISO 8601, or empty when not set.
    pub const VAULT_CREATED: &str = "vault_created";
    /// Modification time from the vault header at decrypt time, UTC ISO 8601,
    /// or empty when not set.
    pub const VAULT_MODIFIED: &str = "vault_modified";
    /// The same modification stamp as the raw header value (seconds since
    /// 2000-01-01 UTC, decimal), which is what `b4 status` compares.
    pub const VAULT_MODIFIED_STAMP: &str = "vault_modified_stamp";
    /// Size of the vault file in bytes at decrypt time.
    pub const VAULT_SIZE: &str = "vault_size";
    /// When the database was written, UTC ISO 8601.
    pub const DECRYPTED_AT: &str = "decrypted_at";
    /// [`content_checksum`](super::content_checksum) of the whole vault file at
    /// decrypt time. `b4 status` compares it when id, stamp and size all match,
    /// because the header stamp has a resolution of one second and is only
    /// rewritten when Banking4 closes the vault.
    pub const VAULT_CHECKSUM: &str = "vault_checksum";
    /// Number of vault entries holding bank access data that were skipped
    /// without being read.
    pub const ENTRIES_NEVER_READ: &str = "entries_never_read";
}

/// A 64-bit FNV-1a checksum of a vault file as 16 lower-case hex digits.
///
/// It only has to notice that the file changed, not to resist an attacker, so
/// a plain non-cryptographic checksum is enough and needs no dependency.
pub fn content_checksum(bytes: &[u8]) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = bytes.iter().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    });
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::content_checksum;

    #[test]
    fn checksum_matches_the_published_fnv1a_vectors() {
        assert_eq!(content_checksum(b""), "cbf29ce484222325");
        assert_eq!(content_checksum(b"a"), "af63dc4c8601ec8c");
        assert_eq!(content_checksum(b"foobar"), "85944171f73967e8");
    }
}
