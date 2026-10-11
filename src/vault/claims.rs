//! Which entry each cluster belongs to, as far as contents were returned or
//! an entry's storage was reserved, and which clusters a failed read went
//! through.
//!
//! A chain belongs to exactly one file or directory (01 §6.3). Nothing in the
//! allocation table says which, so a damaged or hostile container can make
//! two entries lead into the same clusters: many directories that all list
//! the same file, a file that reads a directory's pages, a directory that is
//! its own ancestor (01 §7.4). Banking4 does not notice. This
//! reader does, because the layer above would otherwise store the same
//! records under several accounts and could be made to do work that grows
//! with the square of the file size.
//!
//! The claims alone bound the work of reads that succeed: a cluster that was
//! returned for one entry costs every other entry one step. A read that
//! *fails* returns nothing and claims nothing, so the same reasoning needs a
//! second record for it: the clusters a failed walk went through are
//! [poisoned](Poison), and the next walk that reaches one of them learns
//! there, in one step, what the first one found at the end of the way.
//!
//! A claim is also how a caller keeps an entry's storage out of every read
//! without reading it: [`Vault::reserve`](super::Vault::reserve) records the
//! clusters of an entry's chain for that entry, and marks the entry as
//! reserved so that a clash with it can be told from a clash between two
//! entries that were both read.

use std::collections::HashMap;

use super::clusters::Broken;
use super::dir::Origin;
use super::error::VaultError;

/// Who can hold a cluster: the root directory, or the entry stored at a given
/// place in a directory page.
///
/// The place, not the name or the first cluster, is what tells two entries
/// apart: cross-linked entries can agree in every stored field. A copy of an
/// entry has the same place, so reading one entry twice is no clash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Owner {
    Root,
    Entry(Origin),
}

/// Why a chain supplies no more clusters than it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Halt {
    /// The chain ends, as chains do. That is a fault only for a file whose
    /// size needs more (01 §8.1). `last` is the last cluster of the chain or,
    /// for an entry without one, its stored reference.
    End { last: i32 },
    /// The allocation table or the chain is damaged there (01 §6.3).
    Broken(Broken),
    /// The next cluster is `cluster`, which the owner with the label number
    /// `holder` was given.
    Claimed { cluster: u32, holder: usize },
}

/// What a failed walk left behind at a cluster it went through: how many
/// clusters the chain supplies from this one on, itself included, and what
/// stops it after them.
///
/// The count is what keeps the legal cases legal. A file chain that loops
/// only beyond what an entry's size needs reads normally (01 §6.3, §8.1), so
/// a walk that needs no more than `yields` clusters from here goes on, and
/// one that needs more fails here with `halt`, without taking the steps in
/// between again.
///
/// `yields` is 0 for a cluster that a second failed walk went through, which
/// can only happen when a cluster further on was claimed in the meantime.
/// Such a cluster is given up: every later walk that reaches it fails at
/// once, even one that would have needed fewer clusters than are still
/// there. Without that, entries of decreasing size on one chain, each cut
/// off by a new claim a little earlier than the one before, would each walk
/// the chain again. With it a cluster is stepped on by failed walks at most
/// twice, and the work a container can cause stays bounded by its size.
/// Refusing those entries is stricter than Banking4, which
/// notices none of this, and it concerns containers only in which two
/// entries were already found to share clusters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Poison {
    pub(crate) yields: usize,
    pub(crate) halt: Halt,
}

/// The clusters handed out so far and their owners, and the clusters that
/// failed walks went through.
///
/// Grows with what is read or reserved: one map entry per cluster of
/// returned content (directory pages and file data) and per reserved
/// cluster, one per cluster of a walk that failed, one label per entry.
#[derive(Debug, Default)]
pub(crate) struct Claims {
    /// Cluster number to index into `labels`.
    holders: HashMap<u32, usize>,
    /// Index into `labels` of each owner that holds something.
    owners: HashMap<Owner, usize>,
    /// How each owner is named in an error message.
    labels: Vec<String>,
    /// For each label, whether its owner's storage was reserved (as opposed
    /// to read).
    reserved: Vec<bool>,
    /// What failed walks found, by cluster.
    poisoned: HashMap<u32, Poison>,
}

impl Claims {
    /// The label number of whoever holds `cluster`, unless that is `owner`
    /// itself or nobody.
    pub(crate) fn holder(&self, owner: Owner, cluster: u32) -> Option<usize> {
        let holder = self.holders.get(&cluster)?;
        (self.owners.get(&owner) != Some(holder)).then_some(*holder)
    }

    /// Whether `owner` is the one with the label number `holder`.
    pub(crate) fn is_holder(&self, owner: Owner, holder: usize) -> bool {
        self.owners.get(&owner) == Some(&holder)
    }

    /// The error for a walk that reached `cluster`, which belongs to the
    /// owner with the label number `holder`. `second` names who was reading.
    pub(crate) fn cross_linked(&self, cluster: u32, holder: usize, second: String) -> VaultError {
        VaultError::CrossLinked {
            cluster,
            first: self.labels.get(holder).cloned().unwrap_or_default(),
            second,
        }
    }

    /// Whether `owner` is known as one whose storage was reserved.
    pub(crate) fn is_reserved_owner(&self, owner: Owner) -> bool {
        self.owners
            .get(&owner)
            .and_then(|&index| self.reserved.get(index))
            .is_some_and(|&reserved| reserved)
    }

    /// Whether anybody holds `cluster`.
    pub(crate) fn is_held(&self, cluster: u32) -> bool {
        self.holders.contains_key(&cluster)
    }

    /// Whether `cluster` is held by an owner whose storage was reserved.
    pub(crate) fn is_reserved(&self, cluster: u32) -> bool {
        self.holders
            .get(&cluster)
            .and_then(|&holder| self.reserved.get(holder))
            .is_some_and(|&reserved| reserved)
    }

    /// Note that `clusters` belong to `owner` from now on. The caller has
    /// made sure with [`holder`](Self::holder) that nobody else has them.
    pub(crate) fn record(
        &mut self,
        owner: Owner,
        clusters: impl IntoIterator<Item = u32>,
        label: impl FnOnce() -> String,
    ) {
        let index = self.index_of(owner, label);
        for cluster in clusters {
            self.holders.insert(cluster, index);
        }
    }

    /// Like [`record`](Self::record), for an owner whose storage is fenced
    /// off without being read. The caller has made sure with
    /// [`is_held`](Self::is_held) that nobody has the clusters.
    ///
    /// An owner is what it was first known as: one that was read before
    /// stays a reader, and what it holds does not become reserved.
    pub(crate) fn reserve(
        &mut self,
        owner: Owner,
        clusters: impl IntoIterator<Item = u32>,
        label: impl FnOnce() -> String,
    ) {
        let index = self.index_as(owner, label, true);
        for cluster in clusters {
            self.holders.insert(cluster, index);
        }
    }

    /// The label number of `owner`, which gets one if it has none.
    fn index_of(&mut self, owner: Owner, label: impl FnOnce() -> String) -> usize {
        self.index_as(owner, label, false)
    }

    /// The label number of `owner`. An owner that has none gets one and is
    /// from then on a reserved one or not, as `reserved` says.
    fn index_as(&mut self, owner: Owner, label: impl FnOnce() -> String, reserved: bool) -> usize {
        match self.owners.get(&owner) {
            Some(&index) => index,
            None => {
                let index = self.labels.len();
                self.labels.push(label());
                self.reserved.push(reserved);
                self.owners.insert(owner, index);
                index
            }
        }
    }

    /// What a failed walk left at `cluster`, if one went through it.
    pub(crate) fn poison(&self, cluster: u32) -> Option<Poison> {
        self.poisoned.get(&cluster).copied()
    }

    /// Note what a failed walk found out about `cluster`, replacing what an
    /// earlier one left there.
    pub(crate) fn set_poison(&mut self, cluster: u32, poison: Poison) {
        self.poisoned.insert(cluster, poison);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::error::ChainFault;

    fn entry(page: u32, slot: usize) -> Owner {
        Owner::Entry(Origin::new(page, slot))
    }

    #[test]
    fn a_cluster_has_one_owner() {
        let mut claims = Claims::default();
        let a = entry(1, 0);
        let b = entry(1, 1);
        assert_eq!(claims.holder(a, 5), None);
        claims.record(a, [5, 6], || "entry \"a\"".to_owned());
        claims.record(Owner::Root, [1], || "the root directory".to_owned());

        // The same owner again, as often as it likes.
        assert_eq!(claims.holder(a, 5), None);
        claims.record(a, [5, 6, 7], || unreachable!());
        assert_eq!(claims.holder(a, 7), None);

        // Anyone else, on every cluster the first one holds.
        for cluster in [5, 6, 7] {
            let holder = claims.holder(b, cluster).unwrap();
            assert!(claims.is_holder(a, holder) && !claims.is_holder(b, holder));
            match claims.cross_linked(cluster, holder, "entry \"b\"".to_owned()) {
                VaultError::CrossLinked {
                    cluster: found,
                    first,
                    second,
                } => {
                    assert_eq!(found, cluster);
                    assert_eq!(first, "entry \"a\"");
                    assert_eq!(second, "entry \"b\"");
                }
                other => panic!("{other:?}"),
            }
        }
        let root = claims.holder(b, 1).unwrap();
        assert!(matches!(
            claims.cross_linked(1, root, "entry \"b\"".to_owned()),
            VaultError::CrossLinked { cluster: 1, first, .. } if first == "the root directory"
        ));
        assert_eq!(claims.holder(Owner::Root, 1), None);
        // Asking claims nothing.
        assert_eq!(claims.holder(b, 8), None);
        assert_eq!(claims.holder(a, 8), None);
    }

    #[test]
    fn a_reserved_owner_holds_its_clusters_like_any_other_and_is_marked() {
        let mut claims = Claims::default();
        let read = entry(1, 0);
        let fenced = entry(1, 1);
        claims.record(read, [5], || "entry \"read\"".to_owned());
        claims.reserve(fenced, [6, 7], || "entry \"fenced\"".to_owned());

        assert!(claims.is_held(5) && claims.is_held(6) && claims.is_held(7));
        assert!(!claims.is_held(8));
        assert!(!claims.is_reserved(5));
        assert!(claims.is_reserved(6) && claims.is_reserved(7));
        assert!(!claims.is_reserved(8));

        // A reserved cluster stands in the way of everyone but its owner.
        let holder = claims.holder(read, 6).unwrap();
        assert!(claims.is_holder(fenced, holder));
        assert_eq!(claims.holder(fenced, 6), None);
        assert!(claims.is_reserved_owner(fenced) && !claims.is_reserved_owner(read));
        assert!(!claims.is_reserved_owner(entry(9, 9)));
        // Whoever was a reader first stays one.
        claims.reserve(read, [8], || unreachable!());
        assert!(!claims.is_reserved_owner(read) && !claims.is_reserved(8));
        assert!(matches!(
            claims.cross_linked(6, holder, "entry \"read\"".to_owned()),
            VaultError::CrossLinked { cluster: 6, first, second }
                if first == "entry \"fenced\"" && second == "entry \"read\""
        ));
    }

    #[test]
    fn poison_is_kept_per_cluster_and_replaced() {
        let mut claims = Claims::default();
        assert_eq!(claims.poison(5), None);
        let looped = Poison {
            yields: 3,
            halt: Halt::Broken(Broken::Chain {
                cluster: 5,
                fault: ChainFault::Loop,
            }),
        };
        claims.set_poison(5, looped);
        assert_eq!(claims.poison(5), Some(looped));
        assert_eq!(claims.poison(6), None);
        // Poison is no claim.
        assert_eq!(claims.holder(entry(1, 0), 5), None);
        let given_up = Poison {
            yields: 0,
            halt: Halt::Claimed {
                cluster: 9,
                holder: 0,
            },
        };
        claims.set_poison(5, given_up);
        assert_eq!(claims.poison(5), Some(given_up));
    }
}
