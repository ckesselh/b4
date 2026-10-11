//! The vault container: header, key derivation, allocation table, directories
//! and file contents (docs/format/01-container.md).
//!
//! A vault is one file holding a small file system. Its header, allocation
//! table and directories are stored in the clear; file contents are encrypted
//! per file with a key that is unwrapped from the header with the password.
//! This module gives read-only access to all of it and knows nothing about
//! what the files mean.
//!
//! # Reading a vault
//!
//! ```no_run
//! use b4::vault::{find_entry, Vault, VaultError};
//!
//! fn categories(password: &str) -> Result<Option<Vec<u8>>, VaultError> {
//!     let mut vault = Vault::open("Haushaltsbuch.sub")?;
//!     vault.unlock(password)?;
//!     match find_entry(vault.root(), "Categories") {
//!         Some(entry) => vault.read_file(entry).map(Some),
//!         None => Ok(None),
//!     }
//! }
//! ```
//!
//! [`Header::read`] reads the 512-byte header alone, for commands that only
//! describe a vault.
//!
//! # What is read when
//!
//! Reading is by [`Entry`], and an entry is obtained from a directory listing,
//! which touches directory pages only. A caller therefore sees the *name* of
//! every entry before a single byte of any file's contents has been looked at
//! or decrypted, and decides by name what to read. b4 relies on this to keep
//! its promise that bank access data is never read: do not add an interface
//! that reads contents by path, by pattern or in bulk.
//!
//! # Fencing off what is not read
//!
//! A name is not enough. Nothing in a container says which entry a cluster
//! belongs to, so a damaged or manipulated directory slot can carry a name
//! the caller reads and the first cluster of an entry it must not read; all
//! files of a vault are encrypted with the same key, so the bytes would
//! decrypt. A caller that skips entries therefore tells the vault so,
//! *before it reads anything*, with [`Vault::reserve`].
//!
//! The guarantee, exactly: once an entry is reserved, no call on this vault
//! returns, copies or decrypts a byte of a cluster that the allocation table
//! chains from the entry's first cluster, whatever size the entry states,
//! or, for a directory, of a cluster in the chain of any entry below it, at
//! any depth. Reading the entry itself fails with [`VaultError::Reserved`].
//! Every other entry whose chain needs such a cluster fails with
//! [`VaultError::CrossLinked`] before anything of *its* chain is copied or
//! decrypted, and [`Vault::is_reserved`] tells the caller that the cluster in
//! the error is a reserved one. Reserving reads the allocation table and, of
//! a reserved directory, the directory's own pages, which are stored in the
//! clear and hold names, sizes and first clusters; that is how the clusters
//! below it are found. It reads no file data, decrypts nothing and needs no
//! password.
//!
//! Reserving never fails. A chain is followed until it ends, until it breaks
//! (01 §6.3) or until it reaches a cluster that already has a holder, and
//! what was walked up to there is reserved. Clusters behind a break belong
//! to no chain, so the entry has no claim to them either.
//!
//! The limit of this is the limit of the format. The fence follows the
//! directories as they are found, and they are neither encrypted nor
//! authenticated: whoever can rewrite a container can also remove an entry's
//! slot, or point a slot at clusters that no entry names any more, and
//! nothing then says whose bytes those once were. The guarantee is about
//! the entries a container has, not about where its ciphertext came from.
//!
//! The work is linear in the size of the container, however often and in
//! whatever order entries are reserved: every chain step either gives a
//! cluster its first holder or ends the walk for one entry, and a directory
//! slot is decoded once, when its page gets its holder. That is at most one
//! step per cluster and one per 64 bytes of directory page, plus one per
//! call.
//!
//! # Read-only
//!
//! [`Vault::open`] opens the file with read access only, reads it once from
//! start to end into memory and closes it. Nothing is ever written, no lock
//! is taken and no lock file is created (01 §11). All further work is done on
//! that snapshot, so a vault that Banking4 changes afterwards cannot confuse
//! a read that is under way. A snapshot taken at the very moment Banking4
//! writes can still be inconsistent in itself; the chain and table errors
//! that result mean "try again", not "damaged" (01 §11).
//!
//! # Damaged input
//!
//! Nothing in a container is integrity protected. Structural damage (a bad
//! table entry, a chain that leaves the file or loops, a truncated file) is
//! reported as a [`VaultError`]; it never panics and never makes the reader
//! allocate more than the size of the file. Damaged *contents* cannot be
//! detected here: a flipped ciphertext bit silently garbles one 16-byte block
//! (01 §4).
//!
//! In two places the reader is stricter than Banking4, which
//! would return data twice: a chain that leads back into itself is refused
//! at the cluster it reaches again, and a cluster belongs to the first entry
//! it was read or reserved for. A second entry whose chain leads into it is refused with
//! [`VaultError::CrossLinked`], whether it is a file, a directory or a
//! directory that is its own ancestor (01 §7.4). Reading the same entry
//! again is always fine. A caller that goes through [`Vault::root`] and
//! [`Vault::read_dir`] therefore needs no loop protection of its own, and
//! the work a vault can cause is bounded by its size.
//!
//! That bound holds for reads that fail, too. The clusters a failed read
//! went through are remembered together with what it ran into, and the next
//! read whose chain leads to one of them and needs to get as far fails
//! there, in one step and with the same kind of error, whichever entry it is
//! for. An entry that needs less of the chain than is sound still reads
//! (01 §8.1). A failed read is never turned into contents by this.
//!
//! The third place: a cluster that holds a page of the allocation table is
//! no part of any chain. Banking4 would return the table's
//! bytes as contents; this reader reports
//! [`ChainFault::TablePage`] (01 §6.2).

mod claims;
mod clusters;
mod crypto;
mod dir;
mod error;
mod fields;
mod header;
mod ripemd160;

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use claims::{Claims, Halt, Owner, Poison};
use clusters::{Broken, Clusters};
use crypto::ContentKey;

pub use clusters::MAX_VAULT_SIZE;
pub use crypto::MAX_KDF_ROUNDS;
pub use dir::{find_entry, Entry};
pub use error::{ChainFault, TableFault, VaultError};
pub use header::{
    Cipher, Guid, Header, ParseGuidError, Timestamp, UtcDateTime, BANKING4_TYPE_GUID, HEADER_SIZE,
};

/// AES block size in bytes: the unit of file encryption (01 §4).
const BLOCK: usize = 16;

/// How an error message names the root directory as the holder of a cluster.
const ROOT_LABEL: &str = "the root directory";

/// A vault held in memory, read-only.
///
/// [`open`](Self::open) and [`from_bytes`](Self::from_bytes) check the
/// structure (01 §9, phase 1) and need no password. Directories can be listed
/// and unencrypted files read right away; [`unlock`](Self::unlock) checks the
/// password and makes encrypted files readable.
pub struct Vault {
    header: Header,
    clusters: Clusters,
    root: Vec<Entry>,
    /// The content key once the vault is unlocked. Wiped on drop.
    key: Option<ContentKey>,
    /// Which entry each cluster was read or reserved for, and where reads
    /// failed. Reading is `&self`, hence the lock; it is held for the length
    /// of one read.
    claims: Mutex<Claims>,
    /// Which clusters were used as what, for the tests that pin what is
    /// never read.
    #[cfg(test)]
    touched: Mutex<Touched>,
}

/// What a vault did with its clusters so far, by cluster number. Only tests
/// have it: it is written at the two places where the bytes of a cluster are
/// used, the copy of file data in [`Vault::read_file`] and the decoding of
/// directory pages.
#[cfg(test)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Touched {
    /// Clusters whose bytes were copied out as file data, with the number of
    /// bytes.
    pub(crate) data: std::collections::BTreeMap<u32, usize>,
    /// Those of them whose bytes went through the cipher.
    pub(crate) decrypted: std::collections::BTreeSet<u32>,
    /// Clusters that were decoded as directory pages.
    pub(crate) pages: std::collections::BTreeSet<u32>,
}

/// One walk along a chain: who reads, from where and how far.
struct Walk<'a> {
    owner: Owner,
    /// The stored reference to the first cluster.
    first_cluster: i32,
    /// How many clusters are needed: `Some` for a file, whose size says so
    /// (01 §8.1), `None` for a directory, which is its whole chain (01 §7.2).
    needed: Option<usize>,
    /// How an error message names the owner.
    label: &'a dyn Fn() -> String,
}

impl Walk<'_> {
    /// Whether this walk, with `remaining` clusters still to get from a
    /// poisoned cluster on (itself included), ends at that cluster.
    fn ends_at(&self, poison: Poison, remaining: Option<usize>, claims: &Claims) -> bool {
        match (poison.halt, remaining) {
            // A claim does not stand in its holder's way.
            (Halt::Claimed { holder, .. }, _) if claims.is_holder(self.owner, holder) => false,
            (_, Some(remaining)) => remaining > poison.yields,
            // A directory needs its chain to the end, and a proper end is
            // what it is looking for.
            (Halt::End { .. }, None) => false,
            (_, None) => true,
        }
    }

    /// The error for a walk that `halt` ended.
    fn error(&self, halt: Halt, claims: &Claims) -> VaultError {
        match halt {
            Halt::End { last } => VaultError::CorruptChain {
                cluster: last,
                fault: ChainFault::EndsEarly,
            },
            Halt::Broken(broken) if self.needed.is_some() => broken.in_file(),
            Halt::Broken(broken) => broken.in_directory(),
            Halt::Claimed { cluster, holder } => {
                claims.cross_linked(cluster, holder, (self.label)())
            }
        }
    }
}

/// Follow a chain for one entry and return its clusters in order: all of
/// them, or as many as the entry needs (01 §6.3, §8.1).
///
/// Every cluster is checked against the claims before anything of it is
/// used. Nothing is claimed here; the caller does that once it has what it
/// came for.
///
/// A walk that fails leaves [`Poison`] on the clusters it went through, and
/// a walk that comes to a poisoned cluster and needs more of the chain than
/// is sound from there fails at that cluster with what the earlier walk
/// found. Many entries that lead into one long damaged chain therefore cost
/// one walk and one step each, not one walk each.
///
/// The cluster named by a replayed error is the one the first walk named.
/// For a loop that is where the first walk closed it, which a walk that
/// enters the loop elsewhere would have put elsewhere; the kind of error is
/// the same.
fn follow<'a>(
    clusters: &'a Clusters,
    claims: &mut Claims,
    walk: &Walk<'_>,
) -> Result<Vec<(u32, &'a [u8])>, VaultError> {
    // An entry that was reserved is not read, by whoever asks: that is what
    // makes the fence hold for the entry itself.
    if claims.is_reserved_owner(walk.owner) {
        return Err(VaultError::Reserved);
    }
    // Grows with the chain that is really there, never by a size taken from
    // an entry (01 §8.1).
    let mut walked: Vec<(u32, &'a [u8])> = Vec::new();
    // Positions in `walked` of clusters that were poisoned already.
    let mut seen_before = Vec::new();
    let mut chain = clusters.chain(walk.first_cluster);
    // What ended the walk, and how many clusters the chain still supplies
    // from the cluster the walk ended at.
    let (halt, beyond, replayed) = loop {
        let remaining = match walk.needed {
            Some(needed) if needed <= walked.len() => return Ok(walked),
            Some(needed) => Some(needed.saturating_sub(walked.len())),
            None => None,
        };
        let (cluster, data) = match chain.next() {
            Some(Ok(step)) => step,
            Some(Err(broken)) => break (Halt::Broken(broken), 0, false),
            None if remaining.is_none() => return Ok(walked),
            None => {
                // A cluster number has at most 24 bits.
                let last = walked.last().map_or(walk.first_cluster, |&(cluster, _)| {
                    i32::try_from(cluster).unwrap_or(i32::MAX)
                });
                break (Halt::End { last }, 0, false);
            }
        };
        if let Some(holder) = claims.holder(walk.owner, cluster) {
            break (Halt::Claimed { cluster, holder }, 0, false);
        }
        if let Some(poison) = claims.poison(cluster) {
            if walk.ends_at(poison, remaining, claims) {
                break (poison.halt, poison.yields, true);
            }
            seen_before.push(walked.len());
        }
        walked.push((cluster, data));
    };

    // A chain that the walk itself found to loop: from the cluster it came
    // back to, every cluster is on the loop and supplies one round of it.
    // The clusters before lead into the loop and supply their way there too.
    let loop_start = match halt {
        Halt::Broken(Broken::Chain {
            cluster,
            fault: ChainFault::Loop,
        }) if !replayed => walked
            .iter()
            .position(|&(walked, _)| i32::try_from(walked) == Ok(cluster)),
        _ => None,
    };
    for (position, &(cluster, _)) in walked.iter().enumerate() {
        let ahead = walked.len().saturating_sub(position);
        let yields = match loop_start {
            Some(start) if position >= start => walked.len().saturating_sub(start),
            _ => ahead.saturating_add(beyond),
        };
        claims.set_poison(cluster, Poison { yields, halt });
    }
    // A cluster that failed walks went through twice is given up, which is
    // what bounds the work (see `Poison`).
    for position in seen_before {
        if let Some(&(cluster, _)) = walked.get(position) {
            claims.set_poison(cluster, Poison { yields: 0, halt });
        }
    }
    Err(walk.error(halt, claims))
}

impl Vault {
    /// Read the vault at `path` into memory and check its structure.
    ///
    /// The file is opened for reading only, read in one pass and closed
    /// again. Files larger than [`MAX_VAULT_SIZE`] are refused before
    /// anything is read.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, VaultError> {
        let path = path.as_ref();
        let io = |source| VaultError::Io {
            path: path.to_path_buf(),
            source,
        };
        let file = File::open(path).map_err(io)?;
        let len = file.metadata().map_err(io)?.len();
        if len > MAX_VAULT_SIZE {
            return Err(VaultError::TooLarge {
                limit: MAX_VAULT_SIZE,
            });
        }
        // The length is a hint only: a file that grows while it is read is
        // still cut off one byte past the limit and then refused.
        let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        file.take(MAX_VAULT_SIZE + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        Self::from_bytes(bytes)
    }

    /// Take a complete vault image that is already in memory and check its
    /// structure.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, VaultError> {
        // 01 §9, steps 2 to 6. The size limit is checked with the layout.
        let header = Header::parse(&bytes)?;
        let clusters = Clusters::new(bytes, header.cluster_size())?;
        let mut claims = Claims::default();
        let label = || ROOT_LABEL.to_owned();
        let walk = Walk {
            owner: Owner::Root,
            first_cluster: header.root_dir_cluster(),
            needed: None,
            label: &label,
        };
        let pages = follow(&clusters, &mut claims, &walk)?;
        let root = dir::entries(&pages);
        claims.record(Owner::Root, pages.iter().map(|&(page, _)| page), label);
        #[cfg(test)]
        let touched = Touched {
            pages: pages.iter().map(|&(page, _)| page).collect(),
            ..Touched::default()
        };
        Ok(Self {
            header,
            clusters,
            root,
            key: None,
            claims: Mutex::new(claims),
            #[cfg(test)]
            touched: Mutex::new(touched),
        })
    }

    /// The record of which entry holds which cluster.
    fn claims(&self) -> MutexGuard<'_, Claims> {
        // Nothing panics while the lock is held; if a thread did, the record
        // would still be whole, because it is only added to after a walk
        // has come to its end, good or bad.
        self.claims.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Number of chain steps taken on this vault so far.
    #[cfg(test)]
    pub(crate) fn chain_steps(&self) -> u64 {
        self.clusters.steps()
    }

    /// What was done with which cluster so far.
    #[cfg(test)]
    pub(crate) fn touched(&self) -> Touched {
        self.touched_mut().clone()
    }

    #[cfg(test)]
    fn touched_mut(&self) -> MutexGuard<'_, Touched> {
        self.touched.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Note that `bytes` bytes of `cluster` were copied out as file data.
    #[cfg(test)]
    fn note_data(&self, cluster: u32, bytes: usize, decrypted: bool) {
        let mut touched = self.touched_mut();
        *touched.data.entry(cluster).or_default() += bytes;
        if decrypted {
            touched.decrypted.insert(cluster);
        }
    }

    /// Only tests keep track of what was copied.
    #[cfg(not(test))]
    #[inline]
    fn note_data(&self, _cluster: u32, _bytes: usize, _decrypted: bool) {}

    /// The vault's header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Size of the vault file in bytes.
    pub fn file_len(&self) -> u64 {
        self.clusters.file_len() as u64
    }

    /// Number of clusters in the container, not counting the header slot
    /// (01 §2).
    pub fn cluster_count(&self) -> u32 {
        self.clusters.cluster_count()
    }

    /// Check `password` and unlock the vault's encrypted files (01 §9,
    /// phase 2).
    ///
    /// The password is used exactly as given: no trimming, no normalisation.
    /// An empty password is legal (01 §5.5). The key derived from it and the
    /// recovered content key are wiped from memory when they are dropped.
    ///
    /// Fails with [`VaultError::UnsupportedCipher`],
    /// [`VaultError::DeviceBound`] for a vault that is bound to a device,
    /// [`VaultError::KdfRoundsTooHigh`] or [`VaultError::WrongPassword`]. A
    /// failed attempt leaves the vault as it was and changes nothing on disk.
    pub fn unlock(&mut self, password: &str) -> Result<(), VaultError> {
        self.key = Some(crypto::derive_content_key(&self.header, password, None)?);
        Ok(())
    }

    /// Like [`unlock`](Self::unlock) for a vault that is bound to a device:
    /// `device_guid` is the GUID of the Banking4 installation that set the
    /// password (01 §5.6). For a vault that is not device-bound the GUID is
    /// ignored.
    ///
    /// A wrong GUID cannot be told from a wrong password. The b4 command
    /// line has no way to supply a device GUID; this exists for library
    /// users who know theirs.
    pub fn unlock_with_device_guid(
        &mut self,
        password: &str,
        device_guid: &Guid,
    ) -> Result<(), VaultError> {
        self.key = Some(crypto::derive_content_key(
            &self.header,
            password,
            Some(device_guid),
        )?);
        Ok(())
    }

    /// Whether the vault has been unlocked.
    pub fn is_unlocked(&self) -> bool {
        self.key.is_some()
    }

    /// The entries of the root directory, in listing order.
    ///
    /// Available without the password. No file contents are read.
    pub fn root(&self) -> &[Entry] {
        &self.root
    }

    /// The entries of the directory `dir`, in listing order (01 §7.2).
    ///
    /// Available without the password. Only the directory's own pages are
    /// read, never the contents of the files it lists. A directory entry
    /// without pages lists as empty (01 §7.4).
    ///
    /// Fails with [`VaultError::CrossLinked`] if one of the pages belongs to
    /// the root directory or to another entry that was read or reserved
    /// before. That covers a directory that leads back to one of its
    /// ancestors, so going from [`root`](Self::root) into its directories
    /// needs no bookkeeping. Fails with [`VaultError::Reserved`] for a
    /// directory that was itself [reserved](Self::reserve).
    pub fn read_dir(&self, dir: &Entry) -> Result<Vec<Entry>, VaultError> {
        if !dir.is_dir() {
            return Err(VaultError::NotADirectory);
        }
        let owner = Owner::Entry(dir.origin());
        let label = || dir.label();
        let walk = Walk {
            owner,
            first_cluster: dir.first_cluster(),
            needed: None,
            label: &label,
        };
        let mut claims = self.claims();
        // The chain is walked to its end before anything is decoded, so that
        // one that loops fails without a list of the entries it repeats.
        let pages = follow(&self.clusters, &mut claims, &walk)?;
        claims.record(owner, pages.iter().map(|&(page, _)| page), label);
        drop(claims);
        #[cfg(test)]
        self.touched_mut()
            .pages
            .extend(pages.iter().map(|&(page, _)| page));
        Ok(dir::entries(&pages))
    }

    /// Fence off the storage of `entry` without reading it: afterwards no
    /// other entry can be read from its clusters. Returns the number of
    /// clusters that were reserved by this call.
    ///
    /// Call this for every entry that is not going to be read, before the
    /// first [`read_file`](Self::read_file): see "Fencing off what is not
    /// read" in the [module documentation](self) for what it guarantees and
    /// why a name alone does not.
    ///
    /// For a file the clusters of its chain are recorded as belonging to it,
    /// all of them, whatever its size says: a size is as easily forged as a
    /// first cluster, and a chain belongs to one entry as a whole (01 §6.3).
    /// For a directory the same is done for its pages and then for every
    /// entry listed in them, and so on down. Only the allocation table and
    /// the pages of reserved directories are looked at. No file data is read,
    /// nothing is decrypted and no password is needed.
    ///
    /// This never fails. A chain that is damaged, or that leads into a
    /// cluster which already has a holder, is reserved as far as it could be
    /// walked, and a directory lists what its walked pages hold. Reserving an
    /// entry a second time reserves nothing new, and neither does reserving
    /// one that was read before: that one stays readable. An entry that was
    /// reserved first cannot be read afterwards ([`VaultError::Reserved`]).
    ///
    /// The work is bounded by the size of the container, not by the depth or
    /// the shape of the directories: a cluster is given to its first holder
    /// once, and a directory that leads back into itself or into another
    /// one stops at the first cluster that is taken.
    pub fn reserve(&self, entry: &Entry) -> usize {
        let mut claims = self.claims();
        let mut reserved = 0usize;
        // A list instead of recursion: directories can be nested as deep as
        // the container has clusters, and the stack must not follow them.
        // The list holds entries of pages that were reserved a moment ago,
        // each page once, so it stays within the number of directory slots.
        let mut pending = vec![entry.clone()];
        while let Some(entry) = pending.pop() {
            let mut own: Vec<(u32, &[u8])> = Vec::new();
            for step in self.clusters.chain(entry.first_cluster()) {
                // Best effort: what lies behind a break is no part of the
                // chain, and what has a holder is fenced by that holder (or
                // is this entry's already).
                let Ok((cluster, data)) = step else { break };
                if claims.is_held(cluster) {
                    break;
                }
                own.push((cluster, data));
            }
            // Recorded before the children are looked at, so that one of
            // them that points back at these pages finds them taken.
            claims.reserve(
                Owner::Entry(entry.origin()),
                own.iter().map(|&(cluster, _)| cluster),
                || entry.label(),
            );
            reserved = reserved.saturating_add(own.len());
            if entry.is_dir() {
                #[cfg(test)]
                self.touched_mut()
                    .pages
                    .extend(own.iter().map(|&(page, _)| page));
                pending.extend(dir::entries(&own));
            }
        }
        reserved
    }

    /// Whether `cluster` belongs to an entry whose storage was fenced off
    /// with [`reserve`](Self::reserve).
    ///
    /// After a read failed with [`VaultError::CrossLinked`], this tells for
    /// the cluster named there whether the entry ran into storage that was
    /// reserved, which no retry will change, or into another entry that was
    /// read.
    pub fn is_reserved(&self, cluster: u32) -> bool {
        self.claims().is_reserved(cluster)
    }

    /// The contents of the file `file`, decrypted if the entry says they are
    /// encrypted (01 §8).
    ///
    /// This is the only way contents are read: one entry at a time, chosen by
    /// the caller. An unencrypted file can be read while the vault is locked;
    /// an encrypted one gives [`VaultError::Locked`].
    ///
    /// Fails with [`VaultError::CrossLinked`] if one of the clusters the size
    /// needs belongs to a directory, to another file that was read before or
    /// to an entry that was [reserved](Self::reserve). In that case nothing
    /// was copied or decrypted, not even the clusters before the shared one.
    /// Fails with [`VaultError::Reserved`] for a file that was itself
    /// reserved, unless it is empty.
    ///
    /// The contents are built in the one buffer that is returned: the stored
    /// bytes are copied into it and decrypted in place, and it never grows
    /// or moves. No other copy of the plaintext is made here, so a caller
    /// that wipes the buffer (its whole capacity, which can hold up to 15
    /// bytes of decrypted padding behind the contents) has wiped all of it.
    pub fn read_file(&self, file: &Entry) -> Result<Vec<u8>, VaultError> {
        if file.is_dir() {
            return Err(VaultError::NotAFile);
        }
        let key = match (&self.key, file.is_encrypted()) {
            (Some(key), true) => Some(key),
            (None, true) => return Err(VaultError::Locked),
            (_, false) => None,
        };
        // 01 §8.1: an empty file produces no bytes and its chain is not
        // consulted, whatever `first_cluster` says.
        let size = file.size() as usize;
        if size == 0 {
            return Ok(Vec::new());
        }

        // Make sure the chain is long enough, and that all of it is this
        // entry's to read, before allocating anything for the contents: a
        // size made up by a damaged entry costs nothing, because the list
        // below grows with the chain that is really there. A chain may be
        // longer than the size needs; the rest is ignored (01 §8.1).
        let cluster_size = self.clusters.cluster_size();
        let needed = size.div_ceil(cluster_size);
        let owner = Owner::Entry(file.origin());
        let label = || file.label();
        let walk = Walk {
            owner,
            first_cluster: file.first_cluster(),
            needed: Some(needed),
            label: &label,
        };
        // The order is what the fence rests on (see the module
        // documentation): `follow` checks every cluster the size needs
        // against the claims and only names where the clusters are. Not a
        // byte is copied, let alone decrypted, until the walk has come back
        // whole. A chain that reaches reserved storage in its last cluster
        // gives up its first one too.
        let mut claims = self.claims();
        let chain = follow(&self.clusters, &mut claims, &walk)?;
        claims.record(owner, chain.iter().map(|&(cluster, _)| cluster), label);
        drop(claims);

        // Of an encrypted file, whole blocks are stored: the last block is
        // zero-filled before encryption (01 §8.2). `needed` clusters exist,
        // so both lengths are within the size of the snapshot.
        let stored = if key.is_some() {
            size.div_ceil(BLOCK).saturating_mul(BLOCK)
        } else {
            size
        };
        let mut contents = Vec::with_capacity(stored);
        for (cluster, data) in chain {
            let wanted = stored.saturating_sub(contents.len()).min(cluster_size);
            contents.extend_from_slice(data.get(..wanted).unwrap_or(data));
            self.note_data(cluster, wanted, key.is_some());
        }
        if let Some(key) = key {
            // ECB: every block on its own, so the whole file in one go.
            key.decrypt(&mut contents);
            contents.truncate(size);
        }
        Ok(contents)
    }
}

/// Shows the header and the state, never the key.
impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault")
            .field("header", &self.header)
            .field("file_len", &self.file_len())
            .field("cluster_count", &self.cluster_count())
            .field("root_entries", &self.root.len())
            .field("unlocked", &self.is_unlocked())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CS: usize = 512;

    /// The worked example of 01 §12 without its file contents: enough
    /// structure for the tests below. `hello.txt` (cluster 2) and
    /// `Dir/two.bin` (clusters 5 and 6) are marked encrypted.
    fn example() -> Vec<u8> {
        let mut image = vec![0u8; 8 * CS];
        image[..4].copy_from_slice(&[0x53, 0x55, 0x42, 0x01]);
        image[0x04..0x08].copy_from_slice(&512i32.to_le_bytes());
        image[0x0C..0x10].copy_from_slice(&1i32.to_le_bytes());
        image[0x14..0x18].copy_from_slice(&3i32.to_le_bytes());
        for (cluster, entry) in [
            [0xFF, 0, 0, 0],
            [0xFF, 0, 0, 0],
            [0xFF, 0, 0, 0],
            [0xFF, 0, 0, 0],
            [0xFF, 0, 0, 0],
            [0x01, 6, 0, 0],
            [0xFF, 0, 0, 0],
        ]
        .iter()
        .enumerate()
        {
            image[CS + cluster * 4..CS + cluster * 4 + 4].copy_from_slice(entry);
        }
        put_slot(&mut image, 2 * CS, 2, 20, 1, b"hello.txt");
        put_slot(&mut image, 2 * CS + 64, 3, 5, 0, b"plain.txt");
        put_slot(&mut image, 2 * CS + 128, 4, 0, 2, b"Dir");
        put_slot(&mut image, 5 * CS, 5, 520, 1, b"two.bin");
        image[4 * CS..4 * CS + 5].copy_from_slice(b"plain");
        image
    }

    fn example_with(change: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut image = example();
        change(&mut image);
        image
    }

    fn put_slot(image: &mut [u8], at: usize, first: i32, size: i32, flags: u8, name: &[u8]) {
        image[at..at + 4].copy_from_slice(&first.to_le_bytes());
        image[at + 4..at + 8].copy_from_slice(&size.to_le_bytes());
        image[at + 8] = flags;
        image[at + 9] = name.len() as u8;
        image[at + 10..at + 10 + name.len()].copy_from_slice(name);
    }

    #[test]
    fn a_locked_vault_lists_directories_and_reads_plain_files() {
        // 01 §9: steps 1 to 6 and unencrypted files need no password.
        let vault = Vault::from_bytes(example()).unwrap();
        assert!(!vault.is_unlocked());
        assert_eq!(vault.cluster_count(), 7);
        assert_eq!(vault.file_len(), 4096);
        let names: Vec<&str> = vault.root().iter().map(Entry::name).collect();
        assert_eq!(names, ["hello.txt", "plain.txt", "Dir"]);

        let plain = find_entry(vault.root(), "plain.txt").unwrap();
        assert_eq!(vault.read_file(plain).unwrap(), b"plain");

        let dir = find_entry(vault.root(), "Dir").unwrap();
        let inner = vault.read_dir(dir).unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].name(), "two.bin");
        assert_eq!(inner[0].size(), 520);

        let hello = find_entry(vault.root(), "hello.txt").unwrap();
        assert!(matches!(vault.read_file(hello), Err(VaultError::Locked)));
        assert!(matches!(
            vault.read_file(&inner[0]),
            Err(VaultError::Locked)
        ));
    }

    #[test]
    fn entries_are_read_as_what_they_are() {
        let vault = Vault::from_bytes(example()).unwrap();
        let plain = find_entry(vault.root(), "plain.txt").unwrap();
        let dir = find_entry(vault.root(), "Dir").unwrap();
        assert!(matches!(
            vault.read_dir(plain),
            Err(VaultError::NotADirectory)
        ));
        assert!(matches!(vault.read_file(dir), Err(VaultError::NotAFile)));
    }

    #[test]
    fn size_decides_how_much_of_a_chain_is_read() {
        // 01 §8.1, on the plain file in cluster 3 and with the chain 5 -> 6.
        let read = |first: i32, size: i32| {
            let mut image = example();
            put_slot(&mut image, 2 * CS + 64, first, size, 0, b"plain.txt");
            let vault = Vault::from_bytes(image).unwrap();
            let entry = find_entry(vault.root(), "plain.txt").unwrap().clone();
            vault.read_file(&entry)
        };
        assert_eq!(read(3, 5).unwrap(), b"plain");
        assert_eq!(read(3, 3).unwrap(), b"pla");
        assert_eq!(read(3, 512).unwrap().len(), 512);
        // One byte more than the single cluster holds.
        assert!(matches!(
            read(3, 513),
            Err(VaultError::CorruptChain {
                cluster: 3,
                fault: ChainFault::EndsEarly
            })
        ));
        // Two clusters: every size up to 1024 reads, 1025 does not.
        assert_eq!(read(5, 513).unwrap().len(), 513);
        assert_eq!(read(5, 1024).unwrap().len(), 1024);
        assert!(matches!(
            read(5, 1025),
            Err(VaultError::CorruptChain {
                cluster: 6,
                fault: ChainFault::EndsEarly
            })
        ));
        // A size that needs fewer clusters than the chain has is fine.
        assert_eq!(read(5, 10).unwrap().len(), 10);
        // No chain at all.
        assert!(matches!(
            read(-1, 1),
            Err(VaultError::CorruptChain {
                cluster: -1,
                fault: ChainFault::EndsEarly
            })
        ));
        // An empty file never consults its chain.
        for first in [-1, 3, 100_000, i32::MAX, -2] {
            assert_eq!(read(first, 0).unwrap(), b"");
        }
        // References outside the file.
        for first in [-2, 7, 100_000, i32::MAX] {
            assert!(matches!(
                read(first, 1),
                Err(VaultError::CorruptChain {
                    fault: ChainFault::OutOfRange,
                    ..
                })
            ));
        }
        // An absurd size in a small file fails without a matching allocation.
        assert!(matches!(
            read(3, i32::MAX),
            Err(VaultError::CorruptChain {
                fault: ChainFault::EndsEarly,
                ..
            })
        ));
    }

    #[test]
    fn a_looping_file_chain_is_refused_where_it_closes() {
        // 01 §6.3: cluster 6 links back to 5. As long as the size is met
        // before the walk comes round, the link is one of those beyond what
        // the size needs (01 §8.1). A size that needs the loop would read
        // the same clusters again; Banking4 does that (not confirmed,
        // 01 §13.4), this reader refuses.
        let read = |size: i32| {
            let mut image = example();
            image[CS + 6 * 4..CS + 6 * 4 + 4].copy_from_slice(&[0x01, 5, 0, 0]);
            put_slot(&mut image, 2 * CS + 64, 5, size, 0, b"plain.txt");
            let vault = Vault::from_bytes(image).unwrap();
            let entry = find_entry(vault.root(), "plain.txt").unwrap();
            vault.read_file(entry)
        };
        assert_eq!(read(1024).unwrap().len(), 1024);
        for size in [1025, 7 * 512, i32::MAX] {
            assert!(
                matches!(
                    read(size),
                    Err(VaultError::CorruptChain {
                        cluster: 5,
                        fault: ChainFault::Loop
                    })
                ),
                "size {size}"
            );
        }
    }

    #[test]
    fn a_cluster_belongs_to_the_entry_it_was_first_read_for() {
        // plain.txt is given the first cluster of Dir/two.bin (5 -> 6).
        let mut image = example();
        put_slot(&mut image, 2 * CS + 64, 6, 5, 0, b"plain.txt");
        put_slot(&mut image, 5 * CS, 5, 520, 0, b"two.bin");
        let open = || Vault::from_bytes(image.clone()).unwrap();
        let shared = |result: Result<Vec<u8>, VaultError>, first: &str, second: &str| match result {
            Err(VaultError::CrossLinked {
                cluster: 6,
                first: a,
                second: b,
            }) => assert_eq!((a.as_str(), b.as_str()), (first, second)),
            other => panic!("{other:?}"),
        };

        // Whoever is read first keeps the cluster, and can be read again.
        let vault = open();
        let plain = find_entry(vault.root(), "plain.txt").unwrap();
        let dir = find_entry(vault.root(), "Dir").unwrap();
        let two = &vault.read_dir(dir).unwrap()[0];
        assert_eq!(vault.read_file(two).unwrap().len(), 520);
        shared(
            vault.read_file(plain),
            "entry \"two.bin\"",
            "entry \"plain.txt\"",
        );
        assert_eq!(vault.read_file(two).unwrap().len(), 520);
        assert_eq!(vault.read_file(&two.clone()).unwrap().len(), 520);
        assert_eq!(vault.read_dir(dir).unwrap().len(), 1);

        let vault = open();
        let plain = find_entry(vault.root(), "plain.txt").unwrap();
        let dir = find_entry(vault.root(), "Dir").unwrap();
        let two = &vault.read_dir(dir).unwrap()[0];
        assert_eq!(vault.read_file(plain).unwrap().len(), 5);
        shared(
            vault.read_file(two),
            "entry \"plain.txt\"",
            "entry \"two.bin\"",
        );
        // A refused read claims nothing: cluster 5 is still free to take.
        assert_eq!(vault.read_file(plain).unwrap().len(), 5);

        // Only what a read returned is claimed: 512 bytes of the chain
        // 5 -> 6 leave cluster 6 to someone else.
        let mut image = example();
        put_slot(&mut image, 2 * CS + 64, 6, 5, 0, b"plain.txt");
        put_slot(&mut image, 5 * CS, 5, 512, 0, b"two.bin");
        let vault = Vault::from_bytes(image).unwrap();
        let dir = find_entry(vault.root(), "Dir").unwrap();
        let two = &vault.read_dir(dir).unwrap()[0];
        let plain = find_entry(vault.root(), "plain.txt").unwrap();
        assert_eq!(vault.read_file(two).unwrap().len(), 512);
        assert_eq!(vault.read_file(plain).unwrap().len(), 5);
    }

    #[test]
    fn directories_cannot_share_pages_or_lead_back() {
        // A directory that is the root directory again (01 §7.4).
        let mut image = example();
        put_slot(&mut image, 2 * CS + 128, 1, 0, 2, b"Dir");
        let vault = Vault::from_bytes(image).unwrap();
        let dir = find_entry(vault.root(), "Dir").unwrap();
        match vault.read_dir(dir) {
            Err(VaultError::CrossLinked {
                cluster: 1,
                first,
                second,
            }) => assert_eq!(
                (first.as_str(), second.as_str()),
                ("the root directory", "entry \"Dir\"")
            ),
            other => panic!("{other:?}"),
        }

        // Two directory entries with the same first cluster: the first one
        // lists, as often as it is asked, the second is refused.
        let mut image = example();
        put_slot(&mut image, 2 * CS + 192, 4, 0, 2, b"Twin");
        let vault = Vault::from_bytes(image).unwrap();
        let dir = find_entry(vault.root(), "Dir").unwrap();
        let twin = find_entry(vault.root(), "Twin").unwrap();
        assert_ne!(dir, twin);
        assert_eq!(vault.read_dir(twin).unwrap().len(), 1);
        assert_eq!(vault.read_dir(twin).unwrap().len(), 1);
        assert!(matches!(
            vault.read_dir(dir),
            Err(VaultError::CrossLinked { cluster: 4, .. })
        ));

        // A file that reads a directory's page, and the other way round.
        let mut image = example();
        put_slot(&mut image, 2 * CS + 64, 4, 5, 0, b"plain.txt");
        let vault = Vault::from_bytes(image).unwrap();
        let dir = find_entry(vault.root(), "Dir").unwrap();
        let plain = find_entry(vault.root(), "plain.txt").unwrap();
        vault.read_dir(dir).unwrap();
        assert!(matches!(
            vault.read_file(plain),
            Err(VaultError::CrossLinked { cluster: 4, .. })
        ));
        let vault = Vault::from_bytes(example_with(|image| {
            put_slot(image, 2 * CS + 64, 4, 5, 0, b"plain.txt");
        }))
        .unwrap();
        let dir = find_entry(vault.root(), "Dir").unwrap();
        let plain = find_entry(vault.root(), "plain.txt").unwrap();
        vault.read_file(plain).unwrap();
        assert!(matches!(
            vault.read_dir(dir),
            Err(VaultError::CrossLinked { cluster: 4, .. })
        ));
    }

    #[test]
    fn names_in_messages_cannot_carry_control_characters() {
        let mut image = example();
        put_slot(&mut image, 2 * CS + 192, 4, 0, 2, b"\x1b[31mTwin\n");
        let vault = Vault::from_bytes(image).unwrap();
        let dir = find_entry(vault.root(), "Dir").unwrap();
        let twin = find_entry(vault.root(), "\x1b[31mTwin\n").unwrap();
        vault.read_dir(dir).unwrap();
        let text = vault.read_dir(twin).unwrap_err().to_string();
        assert_eq!(
            text,
            "corrupt container: entry \"Dir\" and entry \"\\u{1b}[31mTwin\\n\" share cluster 4"
        );
    }

    /// A container whose root directory holds many entries that lead into
    /// one chain of `shared.len()` clusters.
    struct Fan {
        image: Vec<u8>,
        /// The clusters of the shared chain, in chain order.
        shared: Vec<usize>,
    }

    impl Fan {
        /// Offset of the allocation table entry of `cluster` (01 §6.2).
        fn table_entry(cluster: usize) -> usize {
            let per_page = CS / 4;
            (cluster / per_page * per_page + 1) * CS + cluster % per_page * 4
        }

        fn link(next: usize) -> [u8; 4] {
            let [a, b, c, _] = (next as u32).to_le_bytes();
            [1, a, b, c]
        }

        /// Build a container of `clusters` clusters. Each of `entries` is
        /// the position in the shared chain where the entry starts, its size
        /// and its flags; they are named `E0`, `E1` and so on. The shared
        /// chain ends as a chain should, or links back to its first cluster
        /// if `looping`.
        fn new(
            clusters: usize,
            shared_len: usize,
            looping: bool,
            entries: &[(usize, i32, u8)],
        ) -> Self {
            let per_page = CS / 4;
            let root_len = entries.len().div_ceil(8).max(1);
            // Clusters that are not table pages, in order: first the root
            // directory, then the shared chain.
            let free: Vec<usize> = (1..clusters)
                .filter(|n| n % per_page != 0)
                .take(root_len + shared_len)
                .collect();
            assert_eq!(free.len(), root_len + shared_len, "too few clusters");
            let (root, shared) = free.split_at(root_len);

            let mut image = vec![0u8; (clusters + 1) * CS];
            image[..4].copy_from_slice(&[0x53, 0x55, 0x42, 0x01]);
            image[0x04..0x08].copy_from_slice(&512i32.to_le_bytes());
            image[0x0C..0x10].copy_from_slice(&1i32.to_le_bytes());
            let mut put = |cluster: usize, entry: [u8; 4]| {
                let at = Self::table_entry(cluster);
                image[at..at + 4].copy_from_slice(&entry);
            };
            const END: [u8; 4] = [0xFF, 0, 0, 0];
            for chain in [root, shared] {
                for pair in chain.windows(2) {
                    put(pair[0], Self::link(pair[1]));
                }
                put(chain[chain.len() - 1], END);
            }
            if looping {
                put(shared[shared.len() - 1], Self::link(shared[0]));
            }
            for page in (0..clusters).step_by(per_page) {
                let next = page + per_page;
                put(
                    page,
                    if next < clusters {
                        Self::link(next)
                    } else {
                        END
                    },
                );
            }
            for (i, &(start, size, flags)) in entries.iter().enumerate() {
                put_slot(
                    &mut image,
                    (root[i / 8] + 1) * CS + i % 8 * 64,
                    shared[start] as i32,
                    size,
                    flags,
                    format!("E{i}").as_bytes(),
                );
            }
            Self {
                image,
                shared: shared.to_vec(),
            }
        }
    }

    /// Run `read` on every root entry and return the chain steps that took.
    fn steps_for_all(vault: &Vault, mut read: impl FnMut(usize, &Entry)) -> u64 {
        let before = vault.chain_steps();
        for (i, entry) in vault.root().iter().enumerate() {
            read(i, entry);
        }
        vault.chain_steps() - before
    }

    #[test]
    fn many_entries_on_one_chain_cost_one_step_each() {
        // 01 §6.3: a container of 4000 clusters whose root directory holds
        // 3000 directory entries that all start at the same chain of 900
        // pages. Listing them all is 3000 reads; without the claims each
        // would walk and decode the 900 pages again.
        let fan = Fan::new(4000, 900, false, &vec![(0, 0, 2); 3000]);
        assert_eq!(fan.image.len(), 2_048_512);
        let vault = Vault::from_bytes(fan.image).unwrap();
        assert_eq!(vault.root().len(), 3000);
        let mut listed = 0;
        let mut refused = 0;
        let steps = steps_for_all(&vault, |_, entry| match vault.read_dir(entry) {
            Ok(_) => listed += 1,
            Err(VaultError::CrossLinked { .. }) => refused += 1,
            Err(other) => panic!("{other:?}"),
        });
        assert_eq!((listed, refused), (1, 2999));
        // One walk of the 900 pages, and one step for each of the others.
        assert_eq!(steps, 900 + 2999);
    }

    #[test]
    fn many_entries_on_one_failing_chain_cost_one_step_each() {
        // The same 3000 entries, but the chain of 900 pages they share links
        // back to its first page. No read succeeds, so nothing is claimed;
        // without the record of failed walks every entry would go round the
        // 900 pages again, 2.7 million steps in a file of 2 MB and growing
        // with the square of its size.
        let fan = Fan::new(4000, 900, true, &vec![(0, 0, 2); 3000]);
        // The image of the review: 2,048,512 bytes, in which the last link
        // of the shared chain, at offset 655,892, leads to its first cluster.
        assert_eq!(fan.image.len(), 2_048_512);
        assert_eq!((fan.shared[0], fan.shared[899]), (378, 1285));
        assert_eq!(Fan::table_entry(1285), 655_892);
        assert_eq!(fan.image[655_892..655_896], [0x01, 0x7a, 0x01, 0x00]);
        let vault = Vault::from_bytes(fan.image).unwrap();
        let read_all = || {
            steps_for_all(&vault, |i, entry| match vault.read_dir(entry) {
                Err(VaultError::CorruptDirectory {
                    cluster: 378,
                    fault: ChainFault::Loop,
                }) => {}
                other => panic!("entry {i}: {other:?}"),
            })
        };
        // The first walk goes round once and takes the step that closes the
        // loop; each of the others takes one step.
        assert_eq!(read_all(), 901 + 2999);
        // Asking again changes nothing and costs a step per entry.
        assert_eq!(read_all(), 3000);

        // The same with files that need one cluster more than the loop has.
        let size = 901 * CS as i32;
        let fan = Fan::new(4000, 900, true, &vec![(0, size, 0); 3000]);
        let vault = Vault::from_bytes(fan.image).unwrap();
        let steps = steps_for_all(&vault, |i, entry| match vault.read_file(entry) {
            Err(VaultError::CorruptChain {
                cluster: 378,
                fault: ChainFault::Loop,
            }) => {}
            other => panic!("entry {i}: {other:?}"),
        });
        assert_eq!(steps, 901 + 2999);
    }

    #[test]
    fn many_entries_on_one_short_or_damaged_chain_cost_one_step_each() {
        // A chain that ends before the size of the files is reached
        // (01 §8.1): the first read finds that out, the others are told.
        let size = 901 * CS as i32;
        let fan = Fan::new(4000, 900, false, &vec![(0, size, 0); 3000]);
        let vault = Vault::from_bytes(fan.image).unwrap();
        let steps = steps_for_all(&vault, |i, entry| match vault.read_file(entry) {
            Err(VaultError::CorruptChain {
                cluster: 1285,
                fault: ChainFault::EndsEarly,
            }) => {}
            other => panic!("entry {i}: {other:?}"),
        });
        assert_eq!(steps, 900 + 2999);

        // A chain that runs into a free cluster (01 §6.3), for directories
        // and files alike, entered at different places.
        let entries: Vec<(usize, i32, u8)> = (0..3000)
            .map(|i| match i % 3 {
                0 => (0, 0, 2),
                1 => (i % 800, size, 0),
                _ => (i % 800, 0, 2),
            })
            .collect();
        let mut fan = Fan::new(4000, 900, false, &entries);
        let free = Fan::table_entry(fan.shared[899]);
        fan.image[free..free + 4].copy_from_slice(&[0, 0, 0, 0]);
        let vault = Vault::from_bytes(fan.image).unwrap();
        let steps = steps_for_all(&vault, |i, entry| {
            let result = if entry.is_dir() {
                vault.read_dir(entry).map(|_| ())
            } else {
                vault.read_file(entry).map(|_| ())
            };
            match result {
                Err(VaultError::CorruptDirectory {
                    cluster: 1285,
                    fault: ChainFault::Free,
                }) if entry.is_dir() => {}
                Err(VaultError::CorruptChain {
                    cluster: 1285,
                    fault: ChainFault::Free,
                }) if !entry.is_dir() => {}
                other => panic!("entry {i}: {other:?}"),
            }
        });
        assert_eq!(steps, 900 + 2999);
    }

    #[test]
    fn a_failed_read_leaves_the_sound_part_of_a_chain_readable() {
        // 01 §6.3, §8.1: cluster 6 links back to 5. An entry that needs the
        // loop fails; one whose size is met before the walk comes round
        // reads, also after the other one failed, and is then the owner.
        let mut image = example();
        image[CS + 6 * 4..CS + 6 * 4 + 4].copy_from_slice(&[0x01, 5, 0, 0]);
        put_slot(&mut image, 2 * CS + 64, 5, 1025, 0, b"long");
        put_slot(&mut image, 2 * CS + 192, 5, 2000, 0, b"longer");
        put_slot(&mut image, 2 * CS + 256, 5, 1024, 0, b"fits");
        put_slot(&mut image, 2 * CS + 320, 6, 512, 0, b"tail");
        let vault = Vault::from_bytes(image).unwrap();
        let entry = |name| find_entry(vault.root(), name).unwrap();
        let looped = |result: Result<Vec<u8>, VaultError>| {
            assert!(matches!(
                result,
                Err(VaultError::CorruptChain {
                    cluster: 5,
                    fault: ChainFault::Loop
                })
            ));
        };

        let before = vault.chain_steps();
        looped(vault.read_file(entry("long")));
        assert_eq!(vault.chain_steps() - before, 3);
        // The same entry again, and another one that needs the loop: the
        // same error after one step.
        looped(vault.read_file(entry("long")));
        looped(vault.read_file(entry("longer")));
        assert_eq!(vault.chain_steps() - before, 5);
        // A failed read has not become contents, nor a claim.
        assert_eq!(vault.read_file(entry("fits")).unwrap().len(), 1024);
        assert_eq!(vault.read_file(entry("fits")).unwrap().len(), 1024);
        // From here on the clusters are taken, which is the error that says
        // more.
        for name in ["long", "longer", "tail"] {
            assert!(matches!(
                vault.read_file(entry(name)),
                Err(VaultError::CrossLinked { .. })
            ));
        }

        // A chain that is too short for one entry is long enough for
        // another, and is a whole directory (01 §7.2).
        for (reader, flags) in [("fits", 0), ("Dir", 2)] {
            let mut image = example();
            put_slot(&mut image, 2 * CS + 64, 5, 1025, 0, b"long");
            put_slot(&mut image, 2 * CS + 128, 5, 1024, flags, reader.as_bytes());
            put_slot(&mut image, 2 * CS + 192, 5, 2000, 0, b"longer");
            let vault = Vault::from_bytes(image).unwrap();
            let entry = |name| find_entry(vault.root(), name).unwrap();
            let before = vault.chain_steps();
            for (name, steps) in [("long", 2), ("longer", 3), ("long", 4)] {
                assert!(matches!(
                    vault.read_file(entry(name)),
                    Err(VaultError::CorruptChain {
                        cluster: 6,
                        fault: ChainFault::EndsEarly
                    })
                ));
                assert_eq!(vault.chain_steps() - before, steps, "{name}");
            }
            if flags == 2 {
                assert_eq!(vault.read_dir(entry(reader)).unwrap(), []);
            } else {
                assert_eq!(vault.read_file(entry(reader)).unwrap().len(), 1024);
            }
        }
    }

    #[test]
    fn a_read_refused_for_a_claim_is_remembered_too() {
        // plain.txt holds cluster 6. Two entries start at cluster 5, which
        // leads there: the first walks to the clash, the second is told at
        // cluster 5, and both name the holder.
        let mut image = example();
        put_slot(&mut image, 2 * CS + 64, 6, 5, 0, b"plain.txt");
        put_slot(&mut image, 2 * CS + 128, 5, 520, 0, b"first");
        put_slot(&mut image, 2 * CS + 192, 5, 0, 2, b"second");
        put_slot(&mut image, 2 * CS + 256, 5, 512, 0, b"short");
        let vault = Vault::from_bytes(image).unwrap();
        let entry = |name| find_entry(vault.root(), name).unwrap();
        vault.read_file(entry("plain.txt")).unwrap();
        let before = vault.chain_steps();
        let shared = |result: Result<(), VaultError>, reader: &str| match result {
            Err(VaultError::CrossLinked {
                cluster: 6,
                first,
                second,
            }) => assert_eq!(
                (first.as_str(), second),
                ("entry \"plain.txt\"", format!("entry {reader:?}"))
            ),
            other => panic!("{other:?}"),
        };
        shared(vault.read_file(entry("first")).map(|_| ()), "first");
        assert_eq!(vault.chain_steps() - before, 2);
        shared(vault.read_dir(entry("second")).map(|_| ()), "second");
        shared(vault.read_file(entry("first")).map(|_| ()), "first");
        assert_eq!(vault.chain_steps() - before, 4);
        // The holder is not in its own way, and an entry that stops short
        // of the clash reads.
        assert_eq!(vault.read_file(entry("plain.txt")).unwrap().len(), 5);
        assert_eq!(vault.read_file(entry("short")).unwrap().len(), 512);
    }

    #[test]
    fn claims_that_cut_a_chain_shorter_and_shorter_do_not_add_up() {
        // A chain of 300 clusters and 300 files, read in this order: a file
        // of one cluster at the end of the chain, then a file from its start
        // that runs into that cluster; a file of one cluster before the last
        // one, then a file from the start that is one cluster shorter than
        // the one before and runs into that; and so on. Each long file fails
        // somewhere new. If each of them could walk as far as the last one
        // got, that would be 150 walks of nearly 300 steps.
        let len = 300usize;
        let entries: Vec<(usize, i32, u8)> = (0..len / 2)
            .flat_map(|m| [(len - 1 - m, 512, 0), (0, ((len - m) * CS) as i32, 0)])
            .collect();
        let fan = Fan::new(400, len, false, &entries);
        let vault = Vault::from_bytes(fan.image).unwrap();
        let mut refused = 0;
        let steps = steps_for_all(&vault, |i, entry| match vault.read_file(entry) {
            Ok(contents) if i % 2 == 0 => assert_eq!(contents.len(), 512),
            Err(VaultError::CrossLinked { .. }) => refused += 1,
            other => panic!("entry {i}: {other:?}"),
        });
        // Every long file is refused, and after two failed walks the chain
        // is given up, the short files on it included.
        assert!(refused >= len / 2, "{refused}");
        // Two walks of the chain and a step for each entry, with room to
        // spare; the walks one after the other would be more than 30,000.
        assert!(steps <= 3 * len as u64 + entries.len() as u64, "{steps}");
    }

    #[test]
    fn allocation_table_pages_are_never_contents() {
        // 01 §6.2: 129 clusters of 512 bytes, so the table has a second page
        // in cluster 128. Its own entry reads "end of chain", like that of a
        // file of one cluster, and the page starts with those four bytes.
        // An entry that points there gets an error, not the table.
        let mut image = vec![0u8; 130 * CS];
        assert_eq!(image.len(), 66_560);
        image[..4].copy_from_slice(&[0x53, 0x55, 0x42, 0x01]);
        image[0x04..0x08].copy_from_slice(&512i32.to_le_bytes());
        image[0x0C..0x10].copy_from_slice(&1i32.to_le_bytes());
        image[CS..CS + 4].copy_from_slice(&[0x01, 128, 0, 0]);
        image[CS + 4..CS + 8].copy_from_slice(&[0xFF, 0, 0, 0]);
        image[CS + 8..CS + 12].copy_from_slice(&[0x01, 128, 0, 0]);
        image[129 * CS..129 * CS + 4].copy_from_slice(&[0xFF, 0, 0, 0]);
        put_slot(&mut image, 2 * CS, 128, 4, 0, b"file");
        put_slot(&mut image, 2 * CS + 64, 128, 0, 2, b"dir");
        put_slot(&mut image, 2 * CS + 128, 2, 600, 0, b"linked");
        put_slot(&mut image, 2 * CS + 192, 2, 512, 0, b"before");

        let vault = Vault::from_bytes(image.clone()).unwrap();
        let entry = |name| find_entry(vault.root(), name).unwrap();
        let table_page = VaultError::CorruptChain {
            cluster: 128,
            fault: ChainFault::TablePage,
        }
        .to_string();
        assert_eq!(
            table_page,
            "corrupt cluster chain: reference to a cluster of the allocation table (cluster 128)"
        );
        assert_eq!(
            vault.read_file(entry("file")).unwrap_err().to_string(),
            table_page
        );
        assert!(matches!(
            vault.read_dir(entry("dir")),
            Err(VaultError::CorruptDirectory {
                cluster: 128,
                fault: ChainFault::TablePage
            })
        ));
        // A link into the page is refused where it is found, which is at
        // the cluster before, even if the size does not need the page.
        for name in ["linked", "before"] {
            assert_eq!(
                vault.read_file(entry(name)).unwrap_err().to_string(),
                table_page
            );
        }

        // A root directory in a table page.
        image[0x0C..0x10].copy_from_slice(&128i32.to_le_bytes());
        assert!(matches!(
            Vault::from_bytes(image),
            Err(VaultError::CorruptDirectory {
                cluster: 128,
                fault: ChainFault::TablePage
            })
        ));
    }

    #[test]
    fn directory_faults_are_reported_as_such() {
        // A root directory chain that loops (01 §10).
        let mut image = example();
        image[CS + 4..CS + 8].copy_from_slice(&[0x01, 1, 0, 0]);
        assert!(matches!(
            Vault::from_bytes(image),
            Err(VaultError::CorruptDirectory {
                fault: ChainFault::Loop,
                ..
            })
        ));

        // A root directory outside the file.
        let mut image = example();
        image[0x0C..0x10].copy_from_slice(&99i32.to_le_bytes());
        assert!(matches!(
            Vault::from_bytes(image),
            Err(VaultError::CorruptDirectory {
                cluster: 99,
                fault: ChainFault::OutOfRange
            })
        ));

        // A subdirectory whose page is marked free; the root still lists.
        let mut image = example();
        image[CS + 4 * 4..CS + 4 * 4 + 4].copy_from_slice(&[0, 0, 0, 0]);
        let vault = Vault::from_bytes(image).unwrap();
        let dir = find_entry(vault.root(), "Dir").unwrap();
        assert!(matches!(
            vault.read_dir(dir),
            Err(VaultError::CorruptDirectory {
                cluster: 4,
                fault: ChainFault::Free
            })
        ));

        // A directory without pages lists as empty (01 §7.4).
        let mut image = example();
        put_slot(&mut image, 2 * CS + 128, -1, 0, 2, b"Dir");
        let vault = Vault::from_bytes(image).unwrap();
        let dir = find_entry(vault.root(), "Dir").unwrap();
        assert_eq!(vault.read_dir(dir).unwrap(), []);
    }

    #[test]
    fn structure_errors_surface_when_opening() {
        assert!(matches!(
            Vault::from_bytes(Vec::new()),
            Err(VaultError::NotAVault)
        ));
        assert!(matches!(
            Vault::from_bytes(example()[..511].to_vec()),
            Err(VaultError::NotAVault)
        ));
        // Only a header.
        assert!(matches!(
            Vault::from_bytes(example()[..512].to_vec()),
            Err(VaultError::InvalidLayout { .. })
        ));
        let mut long = example();
        long.push(0);
        assert!(matches!(
            Vault::from_bytes(long),
            Err(VaultError::InvalidLayout { file_len: 4097, .. })
        ));
        let mut table = example();
        table[CS..CS + 4].copy_from_slice(&[0, 0, 0, 0]);
        assert!(matches!(
            Vault::from_bytes(table),
            Err(VaultError::CorruptAllocationTable {
                cluster: 0,
                fault: TableFault::BadPageLink
            })
        ));
    }

    #[test]
    fn debug_output_shows_state_only() {
        let vault = Vault::from_bytes(example()).unwrap();
        let text = format!("{vault:?}");
        assert!(text.contains("unlocked: false"));
        assert!(text.contains("root_entries: 3"));
    }

    // --- Reserving ------------------------------------------------------------

    /// Whether `result` is the clash of `second` with `first` at `cluster`.
    fn is_clash<T: fmt::Debug>(
        result: &Result<T, VaultError>,
        cluster: u32,
        first: &str,
        second: &str,
    ) -> bool {
        matches!(
            result,
            Err(VaultError::CrossLinked { cluster: at, first: a, second: b })
                if *at == cluster && a == &format!("entry {first:?}") && b == &format!("entry {second:?}")
        )
    }

    #[test]
    fn a_reserved_file_cannot_be_read_through_another_entry() {
        // plain.txt is given the chain of Dir/two.bin (5 -> 6), which the
        // caller has said it will not read.
        let mut image = example();
        put_slot(&mut image, 2 * CS + 64, 5, 600, 0, b"plain.txt");
        put_slot(&mut image, 5 * CS, 5, 520, 0, b"two.bin");
        let vault = Vault::from_bytes(image).unwrap();
        let entry = |name| find_entry(vault.root(), name).unwrap();
        let two = vault.read_dir(entry("Dir")).unwrap().remove(0);
        let steps = vault.chain_steps();

        assert_eq!(vault.reserve(&two), 2);
        assert_eq!(vault.chain_steps() - steps, 2);
        assert!(vault.is_reserved(5) && vault.is_reserved(6));
        // Directory pages and clusters of nobody are not reserved.
        for cluster in [0, 1, 2, 3, 4, 7, 100_000] {
            assert!(!vault.is_reserved(cluster), "{cluster}");
        }

        let read = vault.read_file(entry("plain.txt"));
        assert!(is_clash(&read, 5, "two.bin", "plain.txt"), "{read:?}");
        // Nothing was copied, of no cluster, and reserving copied nothing.
        let touched = vault.touched();
        assert!(touched.data.is_empty(), "{touched:?}");
        assert!(touched.decrypted.is_empty(), "{touched:?}");

        // Again costs one step and reserves nothing new.
        let steps = vault.chain_steps();
        assert_eq!(vault.reserve(&two), 0);
        assert_eq!(vault.chain_steps() - steps, 1);

        // The reserved entry itself is not read either, not even by the
        // caller that holds it, and asking costs no step.
        assert!(matches!(vault.read_file(&two), Err(VaultError::Reserved)));
        assert!(matches!(
            vault.read_file(&two.clone()),
            Err(VaultError::Reserved)
        ));
        assert_eq!(vault.chain_steps() - steps, 1);
        assert!(vault.touched().data.is_empty());
    }

    #[test]
    fn a_read_that_reaches_reserved_storage_late_copies_nothing_at_all() {
        // plain.txt starts in its own cluster 3, which is linked on to the
        // reserved cluster 6. Its first cluster must not be copied either:
        // the whole chain is checked before the first byte is used.
        let mut image = example();
        image[CS + 3 * 4..CS + 3 * 4 + 4].copy_from_slice(&[0x01, 6, 0, 0]);
        put_slot(&mut image, 2 * CS + 64, 3, 600, 0, b"plain.txt");
        put_slot(&mut image, 5 * CS, 6, 100, 0, b"two.bin");
        let vault = Vault::from_bytes(image.clone()).unwrap();
        let entry = |name| find_entry(vault.root(), name).unwrap();
        let two = vault.read_dir(entry("Dir")).unwrap().remove(0);
        assert_eq!(vault.reserve(&two), 1);
        let read = vault.read_file(entry("plain.txt"));
        assert!(is_clash(&read, 6, "two.bin", "plain.txt"), "{read:?}");
        assert!(vault.touched().data.is_empty());

        // The link alone does no harm: with a size that its own cluster
        // holds, the file reads, and reads that cluster only (01 §8.1).
        put_slot(&mut image, 2 * CS + 64, 3, 5, 0, b"plain.txt");
        let vault = Vault::from_bytes(image).unwrap();
        let entry = |name| find_entry(vault.root(), name).unwrap();
        let two = vault.read_dir(entry("Dir")).unwrap().remove(0);
        vault.reserve(&two);
        assert_eq!(vault.read_file(entry("plain.txt")).unwrap(), b"plain");
        assert_eq!(
            vault.touched().data.keys().copied().collect::<Vec<_>>(),
            [3]
        );
    }

    #[test]
    fn a_file_is_reserved_with_its_whole_chain_whatever_its_size_says() {
        // The size of a reserved entry is as little to be trusted as its
        // first cluster: two.bin says it is empty, or one cluster long, and
        // its chain 5 -> 6 is reserved all the same.
        for size in [0, 1, 512, i32::MAX] {
            let mut image = example();
            put_slot(&mut image, 2 * CS + 64, 6, 5, 0, b"plain.txt");
            put_slot(&mut image, 5 * CS, 5, size, 0, b"two.bin");
            let vault = Vault::from_bytes(image).unwrap();
            let entry = |name| find_entry(vault.root(), name).unwrap();
            let two = vault.read_dir(entry("Dir")).unwrap().remove(0);
            assert_eq!(vault.reserve(&two), 2, "size {size}");
            let read = vault.read_file(entry("plain.txt"));
            assert!(is_clash(&read, 6, "two.bin", "plain.txt"), "{read:?}");
        }
    }

    #[test]
    fn a_reserved_directory_fences_everything_below_it_and_reads_no_data() {
        // plain.txt is given the last cluster of Dir/two.bin, and
        // "Other" is a second directory entry with the pages of Dir.
        let mut image = example();
        put_slot(&mut image, 2 * CS + 64, 6, 5, 0, b"plain.txt");
        put_slot(&mut image, 2 * CS + 192, 4, 0, 2, b"Other");
        let vault = Vault::from_bytes(image).unwrap();
        let entry = |name| find_entry(vault.root(), name).unwrap();

        // The page of Dir and the two clusters of the file in it.
        assert_eq!(vault.reserve(entry("Dir")), 3);
        for cluster in [4, 5, 6] {
            assert!(vault.is_reserved(cluster), "{cluster}");
        }
        let touched = vault.touched();
        assert_eq!(touched.pages.iter().copied().collect::<Vec<_>>(), [1, 4]);
        assert!(touched.data.is_empty() && touched.decrypted.is_empty());

        let read = vault.read_file(entry("plain.txt"));
        assert!(is_clash(&read, 6, "two.bin", "plain.txt"), "{read:?}");
        // The directory cannot be listed under another name either.
        let listed = vault.read_dir(entry("Other"));
        assert!(is_clash(&listed, 4, "Dir", "Other"), "{listed:?}");
        assert!(vault.touched().data.is_empty());
        // Nor under its own: the entries of a reserved directory are not
        // handed out.
        assert!(matches!(
            vault.read_dir(entry("Dir")),
            Err(VaultError::Reserved)
        ));
        // What was not reserved reads as before.
        assert!(matches!(
            vault.read_file(entry("hello.txt")),
            Err(VaultError::Locked)
        ));
    }

    #[test]
    fn reserving_is_best_effort_and_never_fails() {
        // Chains that lead nowhere, out of the file, into the allocation
        // table, into the root directory or round in a circle.
        for (first, flags) in [(-1, 0), (-2, 0), (7, 2), (100_000, 0), (1, 2), (1, 0)] {
            let mut image = example();
            put_slot(&mut image, 2 * CS + 192, first, 9, flags, b"odd");
            let vault = Vault::from_bytes(image).unwrap();
            let odd = find_entry(vault.root(), "odd").unwrap();
            assert_eq!(vault.reserve(odd), 0, "first cluster {first}");
            assert!(!vault.is_reserved(1));
            // Everything else is as it was.
            let plain = find_entry(vault.root(), "plain.txt").unwrap();
            assert_eq!(vault.read_file(plain).unwrap(), b"plain");
        }

        // A chain that loops is reserved once round: 5 -> 6 -> 5.
        let mut image = example();
        image[CS + 6 * 4..CS + 6 * 4 + 4].copy_from_slice(&[0x01, 5, 0, 0]);
        put_slot(&mut image, 2 * CS + 192, 5, 9, 0, b"odd");
        let vault = Vault::from_bytes(image).unwrap();
        assert_eq!(vault.reserve(find_entry(vault.root(), "odd").unwrap()), 2);

        // A chain that breaks is reserved up to the break: 5 -> 6 -> free.
        let mut image = example();
        image[CS + 6 * 4..CS + 6 * 4 + 4].copy_from_slice(&[0x01, 7, 0, 0]);
        put_slot(&mut image, 2 * CS + 192, 5, 9, 0, b"odd");
        let vault = Vault::from_bytes(image).unwrap();
        assert_eq!(vault.reserve(find_entry(vault.root(), "odd").unwrap()), 1);
        assert!(vault.is_reserved(5) && !vault.is_reserved(6));

        // A directory that lists itself and its parent's other entries: its
        // page is reserved once, the entries in it find it taken.
        let mut image = example();
        put_slot(&mut image, 5 * CS + 64, 4, 0, 2, b"Self");
        put_slot(&mut image, 5 * CS + 128, 1, 0, 2, b"Up");
        put_slot(&mut image, 5 * CS + 192, 3, 5, 0, b"Plain again");
        let vault = Vault::from_bytes(image).unwrap();
        let entry = |name| find_entry(vault.root(), name).unwrap();
        // Dir's page, two.bin's two clusters and the cluster of plain.txt,
        // which the directory lists as "Plain again".
        assert_eq!(vault.reserve(entry("Dir")), 4);
        let read = vault.read_file(entry("plain.txt"));
        assert!(is_clash(&read, 3, "Plain again", "plain.txt"), "{read:?}");

        // An entry that was read keeps what it was read for.
        let vault = Vault::from_bytes(example()).unwrap();
        let plain = find_entry(vault.root(), "plain.txt").unwrap();
        assert_eq!(vault.read_file(plain).unwrap(), b"plain");
        assert_eq!(vault.reserve(plain), 0);
        assert!(!vault.is_reserved(3));
        assert_eq!(vault.read_file(plain).unwrap(), b"plain");
    }

    /// A container of `depth` directories, each the only entry of the one
    /// before, with one file of one cluster in the innermost. Returns the
    /// image and the cluster of that file.
    fn nested(depth: usize) -> (Vec<u8>, usize) {
        let per_page = CS / 4;
        let clusters = depth + depth / (per_page - 1) + 8;
        // Clusters that are not table pages: the root, the directories from
        // the outermost in, the file.
        let free: Vec<usize> = (1..clusters)
            .filter(|n| n % per_page != 0)
            .take(depth + 2)
            .collect();
        assert_eq!(free.len(), depth + 2, "too few clusters");
        let mut image = vec![0u8; (clusters + 1) * CS];
        image[..4].copy_from_slice(&[0x53, 0x55, 0x42, 0x01]);
        image[0x04..0x08].copy_from_slice(&512i32.to_le_bytes());
        image[0x0C..0x10].copy_from_slice(&1i32.to_le_bytes());
        const END: [u8; 4] = [0xFF, 0, 0, 0];
        for &cluster in &free {
            let at = Fan::table_entry(cluster);
            image[at..at + 4].copy_from_slice(&END);
        }
        for page in (0..clusters).step_by(per_page) {
            let next = page + per_page;
            let at = Fan::table_entry(page);
            image[at..at + 4].copy_from_slice(&if next < clusters {
                Fan::link(next)
            } else {
                END
            });
        }
        for pair in free.windows(2).take(depth) {
            put_slot(
                &mut image,
                (pair[0] + 1) * CS,
                pair[1] as i32,
                0,
                2,
                b"Down",
            );
        }
        let file = free[depth + 1];
        put_slot(
            &mut image,
            (free[depth] + 1) * CS,
            file as i32,
            100,
            0,
            b"Deep",
        );
        (image, file)
    }

    #[test]
    fn reserving_goes_to_any_depth_in_work_bounded_by_the_container() {
        // 20000 directories inside one another: deeper than a call stack
        // would follow, and the file at the bottom is fenced all the same.
        let depth = 20_000;
        let (mut image, file) = nested(depth);
        // A file in the root with the cluster of the one at the bottom.
        put_slot(&mut image, 2 * CS + 64, file as i32, 100, 0, b"Top");
        let clusters = image.len() / CS - 1;
        let vault = Vault::from_bytes(image).unwrap();
        let entry = |name| find_entry(vault.root(), name).unwrap();
        let before = vault.chain_steps();
        assert_eq!(vault.reserve(entry("Down")), depth + 1);
        // One step per cluster reserved; nothing is walked twice.
        let steps = vault.chain_steps() - before;
        assert_eq!(steps, depth as u64 + 1);
        assert!(steps <= clusters as u64);
        assert!(vault.is_reserved(file as u32));
        let read = vault.read_file(entry("Top"));
        assert!(is_clash(&read, file as u32, "Deep", "Top"), "{read:?}");
        let touched = vault.touched();
        assert!(touched.data.is_empty());
        assert_eq!(touched.pages.len(), depth + 1);
    }

    #[test]
    fn reserving_many_entries_on_one_chain_costs_one_step_each() {
        // 3000 entries, files and directories in turn, that all start at the
        // same chain of 900 clusters: the first is given the chain, each of
        // the others stops at its first cluster.
        let entries: Vec<(usize, i32, u8)> = (0..3000).map(|i| (0, 0, (i % 2) as u8 * 2)).collect();
        let fan = Fan::new(4000, 900, false, &entries);
        let vault = Vault::from_bytes(fan.image).unwrap();
        let mut reserved = 0;
        let steps = steps_for_all(&vault, |_, entry| reserved += vault.reserve(entry));
        assert_eq!(reserved, 900);
        assert_eq!(steps, 900 + 2999);
        for &cluster in &fan.shared {
            assert!(vault.is_reserved(cluster as u32));
        }
    }

    #[test]
    fn error_messages_are_lower_case_and_name_no_values() {
        let errors = [
            VaultError::NotAVault,
            VaultError::UnsupportedVersion { version: 2 },
            VaultError::TooLarge {
                limit: MAX_VAULT_SIZE,
            },
            VaultError::InvalidLayout {
                file_len: 513,
                cluster_size: 512,
            },
            VaultError::Truncated { cluster: 128 },
            VaultError::CorruptAllocationTable {
                cluster: 4,
                fault: TableFault::BadStatus,
            },
            VaultError::CorruptChain {
                cluster: -2,
                fault: ChainFault::OutOfRange,
            },
            VaultError::CorruptDirectory {
                cluster: 9,
                fault: ChainFault::Loop,
            },
            VaultError::CrossLinked {
                cluster: 7,
                first: ROOT_LABEL.to_owned(),
                second: "entry \"Stmt\"".to_owned(),
            },
            VaultError::UnsupportedCipher { cipher_id: 0 },
            VaultError::KdfRoundsTooHigh {
                rounds: u32::MAX,
                limit: MAX_KDF_ROUNDS,
            },
            VaultError::DeviceBound,
            VaultError::WrongPassword,
            VaultError::Locked,
            VaultError::NotADirectory,
            VaultError::NotAFile,
            VaultError::Reserved,
        ];
        for error in errors {
            let text = error.to_string();
            let first = text.chars().next().unwrap();
            assert!(first.is_lowercase(), "{text}");
            assert!(!text.ends_with('.'), "{text}");
        }
        assert_eq!(
            VaultError::DeviceBound.to_string(),
            format!(
                "the vault is bound to a device: b4 {} cannot open device-bound vaults",
                env!("CARGO_PKG_VERSION")
            )
        );
    }
}
