//! Errors of the container reader (01 §10).
//!
//! Messages name files, fields, cluster numbers and counts only. They never
//! carry a value read from a vault.

use std::fmt;
use std::path::PathBuf;

/// Why a vault could not be opened, unlocked or read.
///
/// The variants follow the error table of 01 §10. None of them is raised for
/// damaged file *contents*: the container has no integrity protection, so a
/// flipped ciphertext bit decrypts to garbage without an error (01 §4).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VaultError {
    /// The vault file could not be opened or read.
    #[error("cannot read vault file {}: {source}", .path.display())]
    Io {
        /// The file that was being read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The file is bigger than any container the format can describe (01 §6.4).
    #[error("vault file is larger than {limit} bytes, the largest container the format allows")]
    TooLarge {
        /// The largest accepted size in bytes.
        limit: u64,
    },

    /// Fewer than 512 bytes, or the magic bytes are wrong (01 §3.2).
    #[error("not a Banking4 vault: the file does not start with a container header")]
    NotAVault,

    /// The file starts with `SUB` but the fourth magic byte is not 1. That byte
    /// may be a format version (01 §14, question 4); no other value is known.
    #[error("unsupported vault format: fourth magic byte is {version}, only 1 is known")]
    UnsupportedVersion {
        /// The fourth magic byte as found.
        version: u8,
    },

    /// The file length is not a whole number of clusters, or it is shorter
    /// than two clusters (01 §2). This is what a file cut off in the middle of
    /// a cluster looks like.
    #[error(
        "invalid vault layout: {file_len} bytes is not a whole number of {cluster_size}-byte \
         clusters, or fewer than two (truncated or damaged file)"
    )]
    InvalidLayout {
        /// Length of the file in bytes.
        file_len: u64,
        /// Cluster size after coercion (01 §3.3).
        cluster_size: u32,
    },

    /// An allocation table page lies beyond the end of the file (01 §6.2).
    #[error(
        "vault is truncated: the allocation table page in cluster {cluster} lies beyond the end \
         of the file"
    )]
    Truncated {
        /// Cluster number of the missing table page.
        cluster: u32,
    },

    /// The allocation table itself is damaged (01 §6.1, §6.2).
    #[error("corrupt allocation table: {fault} (cluster {cluster})")]
    CorruptAllocationTable {
        /// The cluster whose table entry is affected.
        cluster: u32,
        /// What is wrong with it.
        fault: TableFault,
    },

    /// The cluster chain of a file is damaged (01 §6.3, §8.1). On a vault that
    /// Banking4 has open this can be a transient state: try again later
    /// (01 §11).
    #[error("corrupt cluster chain: {fault} (cluster {cluster})")]
    CorruptChain {
        /// The cluster at which the chain broke; negative values are stored
        /// references that are no cluster number.
        cluster: i32,
        /// What is wrong with it.
        fault: ChainFault,
    },

    /// The cluster chain of a directory is damaged (01 §6.3, §7).
    #[error("corrupt directory: {fault} (cluster {cluster})")]
    CorruptDirectory {
        /// The cluster at which the directory's chain broke.
        cluster: i32,
        /// What is wrong with it.
        fault: ChainFault,
    },

    /// `cipher_id` is not AES-128, AES-192 or AES-256 (01 §3.7).
    #[error("unsupported cipher: cipher_id {cipher_id} (only 1, 2 and 3 are known)")]
    UnsupportedCipher {
        /// The stored cipher id.
        cipher_id: i32,
    },

    /// `kdf_rounds` is far beyond anything Banking4 writes. See
    /// [`MAX_KDF_ROUNDS`](super::MAX_KDF_ROUNDS).
    #[error(
        "kdf_rounds is {rounds}, more than the {limit} rounds this reader accepts (damaged \
         header?)"
    )]
    KdfRoundsTooHigh {
        /// The stored round count.
        rounds: u32,
        /// The accepted maximum.
        limit: u32,
    },

    /// The vault is bound to a device (`security_flags` bit 0, 01 §5.6) and no
    /// device GUID was supplied.
    #[error(
        "the vault is bound to a device: b4 {} cannot open device-bound vaults",
        env!("CARGO_PKG_VERSION")
    )]
    DeviceBound,

    /// The password check failed (01 §5.3). For a device-bound vault a wrong
    /// device GUID looks the same.
    #[error("wrong password")]
    WrongPassword,

    /// An encrypted entry was read before the vault was unlocked.
    #[error("the vault is locked: unlock it before reading an encrypted entry")]
    Locked,

    /// Two entries lay claim to the same cluster (01 §6.3: a chain belongs to
    /// one file or directory). Reported when the second of them is read, or
    /// when an entry is read whose chain leads into the storage of one that
    /// was [reserved](super::Vault::reserve); nothing of the entry that is
    /// being read was copied or decrypted then. Like
    /// the chain errors this can be a transient state of a vault that Banking4
    /// has open (01 §11).
    #[error("corrupt container: {first} and {second} share cluster {cluster}")]
    CrossLinked {
        /// The shared cluster.
        cluster: u32,
        /// What was read first and holds the cluster: `the root directory`,
        /// or `entry` and the entry's name in quotes, control characters
        /// escaped.
        first: String,
        /// What was being read when the clash was found, in the same form.
        second: String,
    },

    /// The entry was fenced off with
    /// [`Vault::reserve`](super::Vault::reserve), which is the caller saying
    /// that it will not be read. Nothing of it was read.
    #[error("the entry was reserved as one that is not read")]
    Reserved,

    /// [`Vault::read_dir`](super::Vault::read_dir) was given a file entry.
    #[error("the entry is a file, not a directory")]
    NotADirectory,

    /// [`Vault::read_file`](super::Vault::read_file) was given a directory
    /// entry.
    #[error("the entry is a directory, not a file")]
    NotAFile,
}

/// What is wrong with an allocation table entry (01 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableFault {
    /// The first entry of a table page is neither end-of-chain nor a link to
    /// the next table page (01 §6.2).
    BadPageLink,
    /// The status byte is none of `0x00`, `0x01`, `0xFF` (01 §6.1).
    BadStatus,
    /// No loaded table page covers the cluster (01 §6.2).
    NoEntry,
}

impl fmt::Display for TableFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BadPageLink => "a table page is not linked to the next table page",
            Self::BadStatus => "unknown status byte in a table entry",
            Self::NoEntry => "no table page covers a referenced cluster",
        })
    }
}

/// What is wrong with a cluster chain (01 §6.3, §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChainFault {
    /// The chain refers to cluster 0, a negative number or a cluster beyond
    /// the end of the file.
    OutOfRange,
    /// The chain refers to a cluster that holds a page of the allocation
    /// table (01 §6.2). Cluster 0 is reported as
    /// [`OutOfRange`](Self::OutOfRange), where 01 §6.3 puts it; this is for
    /// the table pages after it. Banking4 would hand out the
    /// table's bytes as contents; this reader refuses on purpose.
    TablePage,
    /// The chain runs into a cluster the table marks as free.
    Free,
    /// The chain leads back to a cluster it has already been through: it
    /// loops.
    Loop,
    /// The chain ends before the entry's size is reached.
    EndsEarly,
}

impl fmt::Display for ChainFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutOfRange => "reference to a cluster outside the file",
            Self::TablePage => "reference to a cluster of the allocation table",
            Self::Free => "the chain runs into a free cluster",
            Self::Loop => "the chain loops",
            Self::EndsEarly => "the chain ends before the entry's size is reached",
        })
    }
}
