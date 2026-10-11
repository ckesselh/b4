//! The cluster layer: where clusters are in the file (01 §2), the allocation
//! table (01 §6) and cluster chains (01 §6.3).
//!
//! Works on an in-memory snapshot of the whole container. Every offset is
//! checked against the snapshot, so a damaged table can make a read fail but
//! never reach outside the buffer.

use std::collections::HashSet;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use super::error::{ChainFault, TableFault, VaultError};

/// The largest container this reader accepts: 2 GiB minus one byte.
///
/// Banking4 handles file offsets as `i32`, so a container of
/// 2 GiB or more is outside the format (01 §6.4). The limit also keeps every
/// cluster number well inside 24 bits.
pub const MAX_VAULT_SIZE: u64 = i32::MAX as u64;

/// Status byte of a free table entry (01 §6.1).
const STATUS_FREE: u8 = 0x00;
/// Status byte of an entry that links to the next cluster of a chain.
const STATUS_CHAINED: u8 = 0x01;
/// Status byte of the last entry of a chain.
const STATUS_END: u8 = 0xFF;

/// Stored reference that means "no chain" (01 §7.1): an entry whose file is
/// still being written, or a directory without pages.
pub(crate) const NO_CHAIN: i32 = -1;

/// A snapshot of a container, cut into clusters, with its allocation table
/// located and checked.
pub(crate) struct Clusters {
    bytes: Vec<u8>,
    cluster_size: usize,
    /// Number of clusters after the header slot; valid numbers are
    /// `0..cluster_count`.
    cluster_count: u32,
    /// Number of allocation table pages that are linked from page 0. Page `k`
    /// is cluster `k * entries_per_page`.
    table_pages: u32,
    /// How many chain steps were taken on this snapshot, for the tests that
    /// bound the work a container can cause.
    #[cfg(test)]
    steps: AtomicU64,
}

impl Clusters {
    /// Check the layout of `bytes` (01 §9 step 4) and load the allocation
    /// table (step 5). `cluster_size` is the coerced value of the header.
    pub(crate) fn new(bytes: Vec<u8>, cluster_size: u32) -> Result<Self, VaultError> {
        let file_len = bytes.len() as u64;
        if file_len > MAX_VAULT_SIZE {
            return Err(VaultError::TooLarge {
                limit: MAX_VAULT_SIZE,
            });
        }
        let invalid = VaultError::InvalidLayout {
            file_len,
            cluster_size,
        };
        // The header yields one of eight sizes, never zero; `checked_rem`
        // keeps this function safe for any caller.
        let size = u64::from(cluster_size);
        if size < 512 || file_len.checked_rem(size) != Some(0) || file_len < 2 * size {
            return Err(invalid);
        }
        let Ok(cluster_count) = u32::try_from(file_len / size - 1) else {
            return Err(invalid);
        };
        let mut clusters = Self {
            bytes,
            cluster_size: cluster_size as usize,
            cluster_count,
            table_pages: 0,
            #[cfg(test)]
            steps: AtomicU64::new(0),
        };
        clusters.table_pages = clusters.count_table_pages()?;
        Ok(clusters)
    }

    /// Length of the snapshot in bytes.
    pub(crate) fn file_len(&self) -> usize {
        self.bytes.len()
    }

    /// The allocation unit in bytes.
    pub(crate) fn cluster_size(&self) -> usize {
        self.cluster_size
    }

    /// Number of clusters in the container (01 §2).
    pub(crate) fn cluster_count(&self) -> u32 {
        self.cluster_count
    }

    /// Number of chain steps taken so far: one per cluster a walk asked the
    /// allocation table about, the one a walk failed on included.
    #[cfg(test)]
    pub(crate) fn steps(&self) -> u64 {
        self.steps.load(Ordering::Relaxed)
    }

    /// Table entries per table page (01 §6.2). At most 16384, so the cast is
    /// lossless.
    fn entries_per_page(&self) -> u32 {
        (self.cluster_size / 4) as u32
    }

    /// The bytes of cluster `n`, which start at `(n + 1) * cluster_size`
    /// (01 §2). `None` if the cluster lies beyond the end of the file.
    fn cluster(&self, n: u32) -> Option<&[u8]> {
        if n >= self.cluster_count {
            return None;
        }
        let start = (n as usize)
            .checked_add(1)?
            .checked_mul(self.cluster_size)?;
        let end = start.checked_add(self.cluster_size)?;
        self.bytes.get(start..end)
    }

    /// Follow the links between the table pages (01 §6.2) and return how many
    /// pages there are.
    fn count_table_pages(&self) -> Result<u32, VaultError> {
        let entries_per_page = self.entries_per_page();
        let mut page_cluster = 0u32;
        let mut pages = 0u32;
        // Each step moves forward by a whole page, so this ends at the latest
        // when it runs past the last cluster.
        loop {
            let page = self.cluster(page_cluster).ok_or(VaultError::Truncated {
                cluster: page_cluster,
            })?;
            pages += 1;
            // The first entry of a page describes the page itself.
            let next_page = page_cluster.checked_add(entries_per_page);
            match page.first_chunk::<4>() {
                Some([STATUS_END, ..]) => return Ok(pages),
                Some([STATUS_CHAINED, a, b, c])
                    if Some(u32::from_le_bytes([*a, *b, *c, 0])) == next_page =>
                {
                    page_cluster = u32::from_le_bytes([*a, *b, *c, 0]);
                }
                _ => {
                    return Err(VaultError::CorruptAllocationTable {
                        cluster: page_cluster,
                        fault: TableFault::BadPageLink,
                    })
                }
            }
        }
    }

    /// The raw table entry of cluster `n`, or `None` if no loaded table page
    /// covers it (01 §6.2).
    fn table_entry(&self, n: u32) -> Option<[u8; 4]> {
        let entries_per_page = self.entries_per_page();
        let page = n.checked_div(entries_per_page)?;
        if page >= self.table_pages {
            return None;
        }
        let page_bytes = self.cluster(page.checked_mul(entries_per_page)?)?;
        let offset = (n % entries_per_page) as usize * 4;
        page_bytes
            .get(offset..)
            .and_then(|rest| rest.first_chunk::<4>())
            .copied()
    }

    /// Validate `reference` as a member of a chain and look up what follows
    /// it (01 §6.3). Returns the cluster number and its successor, `None`
    /// meaning the chain ends here.
    fn successor(&self, reference: i32) -> Result<(u32, Option<i32>), Broken> {
        let cluster = self
            .chain_member(reference)
            .map_err(|fault| Broken::chain(reference, fault))?;
        let entry = self
            .table_entry(cluster)
            .ok_or(Broken::table(cluster, TableFault::NoEntry))?;
        match entry {
            [STATUS_END, ..] => Ok((cluster, None)),
            [STATUS_CHAINED, a, b, c] => {
                // 24 bits: never negative as an i32.
                let next = i32::from_le_bytes([a, b, c, 0]);
                match self.chain_member(next) {
                    Ok(_) => Ok((cluster, Some(next))),
                    Err(fault) => Err(Broken::chain(next, fault)),
                }
            }
            [STATUS_FREE, ..] => Err(Broken::chain(reference, ChainFault::Free)),
            _ => Err(Broken::table(cluster, TableFault::BadStatus)),
        }
    }

    /// A stored reference as a cluster number, if it can be part of a chain:
    /// `0 < n < cluster_count` (01 §6.3), and not one of the clusters the
    /// allocation table itself is stored in.
    ///
    /// The second condition is stricter than Banking4 on purpose.
    /// 01 §6.3 names cluster 0 only, but every loaded table page is as much
    /// a part of the structure as page 0 is (01 §6.2), and its own table
    /// entry looks like the end of a one-cluster chain. Without the check an
    /// entry that points at such a page would get the table's bytes back as
    /// file contents or as directory slots. No writer puts data there.
    fn chain_member(&self, reference: i32) -> Result<u32, ChainFault> {
        let cluster = u32::try_from(reference)
            .ok()
            .filter(|&n| n > 0 && n < self.cluster_count)
            .ok_or(ChainFault::OutOfRange)?;
        if self.is_table_page(cluster) {
            return Err(ChainFault::TablePage);
        }
        Ok(cluster)
    }

    /// Whether cluster `n` holds a page of the allocation table as it was
    /// loaded: page `k` is cluster `k * entries_per_page` (01 §6.2). A
    /// cluster in such a position beyond the last linked page is not one; it
    /// has no table entry at all and fails for that reason.
    fn is_table_page(&self, n: u32) -> bool {
        let entries_per_page = self.entries_per_page();
        n.checked_rem(entries_per_page) == Some(0)
            && n.checked_div(entries_per_page)
                .is_some_and(|page| page < self.table_pages)
    }

    /// Walk the chain that starts at the stored reference `first_cluster`.
    /// [`NO_CHAIN`] gives an empty walk.
    pub(crate) fn chain(&self, first_cluster: i32) -> Chain<'_> {
        Chain {
            clusters: self,
            next: (first_cluster != NO_CHAIN).then_some(first_cluster),
            visited: HashSet::new(),
        }
    }
}

/// A walk along a cluster chain. Yields each cluster's number and bytes after
/// checking the cluster and its table entry, and stops after the first error.
///
/// The successor is looked up for every cluster that is yielded, the last one
/// included, as Banking4 does (01 §8.1).
///
/// A cluster that the walk reaches a second time is reported as a loop, which
/// bounds every walk by the number of clusters. This is stricter than
/// Banking4 on purpose: that follows a file chain round a loop for as long as
/// the entry's size asks (01 §6.3; not confirmed, see §13.4) and so returns
/// the same clusters again as if they were more data. Nothing a writer
/// produces links a chain back into itself.
pub(crate) struct Chain<'a> {
    clusters: &'a Clusters,
    next: Option<i32>,
    /// The clusters yielded so far. Grows with the walk, never with the size
    /// of the container.
    visited: HashSet<u32>,
}

impl<'a> Iterator for Chain<'a> {
    type Item = Result<(u32, &'a [u8]), Broken>;

    fn next(&mut self) -> Option<Self::Item> {
        let reference = self.next.take()?;
        #[cfg(test)]
        self.clusters.steps.fetch_add(1, Ordering::Relaxed);
        let step = self.clusters.successor(reference).and_then(|(n, next)| {
            if !self.visited.insert(n) {
                return Err(Broken::chain(reference, ChainFault::Loop));
            }
            // `successor` has checked the range, so the cluster is there.
            let data = self
                .clusters
                .cluster(n)
                .ok_or(Broken::chain(reference, ChainFault::OutOfRange))?;
            Ok((n, data, next))
        });
        Some(step.map(|(n, data, next)| {
            self.next = next;
            (n, data)
        }))
    }
}

/// Why a chain walk stopped. The caller knows whether it was reading a file
/// or a directory and picks the error accordingly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Broken {
    Table { cluster: u32, fault: TableFault },
    Chain { cluster: i32, fault: ChainFault },
}

impl Broken {
    fn table(cluster: u32, fault: TableFault) -> Self {
        Self::Table { cluster, fault }
    }

    fn chain(cluster: i32, fault: ChainFault) -> Self {
        Self::Chain { cluster, fault }
    }

    /// The error for a broken file chain.
    pub(crate) fn in_file(self) -> VaultError {
        match self {
            Self::Table { cluster, fault } => VaultError::CorruptAllocationTable { cluster, fault },
            Self::Chain { cluster, fault } => VaultError::CorruptChain { cluster, fault },
        }
    }

    /// The error for a broken directory chain.
    pub(crate) fn in_directory(self) -> VaultError {
        match self {
            Self::Table { cluster, fault } => VaultError::CorruptAllocationTable { cluster, fault },
            Self::Chain { cluster, fault } => VaultError::CorruptDirectory { cluster, fault },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CS: usize = 512;

    /// A container image of `clusters` clusters (plus the header slot) whose
    /// table page 0 holds `table`, four bytes per entry.
    fn image(clusters: usize, table: &[[u8; 4]]) -> Vec<u8> {
        let mut bytes = vec![0u8; (clusters + 1) * CS];
        for (i, entry) in table.iter().enumerate() {
            bytes[CS + i * 4..CS + i * 4 + 4].copy_from_slice(entry);
        }
        // Mark every cluster with its own number so reads can be told apart.
        for n in 1..clusters {
            bytes[(n + 1) * CS] = n as u8;
        }
        bytes
    }

    const END: [u8; 4] = [0xFF, 0, 0, 0];
    const FREE: [u8; 4] = [0, 0, 0, 0];

    fn link(next: u32) -> [u8; 4] {
        let [a, b, c, _] = next.to_le_bytes();
        [1, a, b, c]
    }

    fn walk(clusters: &Clusters, first: i32) -> Result<Vec<u32>, Broken> {
        clusters.chain(first).map(|r| r.map(|(n, _)| n)).collect()
    }

    #[test]
    fn layout_is_checked() {
        // 01 §2: a multiple of the cluster size, at least two clusters.
        let table = [END];
        assert!(Clusters::new(image(1, &table), 512).is_ok());
        for len in [0, 511, 512, 513, 1023, 1025, 1536 + 1] {
            let mut bytes = image(3, &table);
            bytes.resize(len, 0);
            assert!(
                matches!(
                    Clusters::new(bytes, 512),
                    Err(VaultError::InvalidLayout { cluster_size: 512, file_len }) if file_len == len as u64
                ),
                "length {len}"
            );
        }
        // The same bytes under a larger cluster size.
        assert!(matches!(
            Clusters::new(image(2, &table), 4096),
            Err(VaultError::InvalidLayout { .. })
        ));
        let clusters = Clusters::new(image(7, &table), 512).unwrap();
        assert_eq!(clusters.cluster_count(), 7);
        assert_eq!(clusters.cluster_size(), 512);
        assert_eq!(clusters.file_len(), 4096);
        // Not a cluster size the header can yield.
        assert!(Clusters::new(image(1, &table), 0).is_err());
    }

    #[test]
    fn cluster_n_starts_at_n_plus_one_clusters() {
        let clusters = Clusters::new(image(4, &[END, END, END, END]), 512).unwrap();
        assert_eq!(clusters.cluster(2).unwrap()[0], 2);
        assert_eq!(clusters.cluster(3).unwrap().len(), 512);
        assert!(clusters.cluster(4).is_none());
        assert!(clusters.cluster(u32::MAX).is_none());
    }

    #[test]
    fn chains_are_followed() {
        // 2 -> 5 -> 3, not contiguous; 4 is a chain of one.
        let table = [END, END, link(5), END, END, link(3), FREE];
        let clusters = Clusters::new(image(7, &table), 512).unwrap();
        assert_eq!(walk(&clusters, 2), Ok(vec![2, 5, 3]));
        assert_eq!(walk(&clusters, 4), Ok(vec![4]));
        assert_eq!(walk(&clusters, NO_CHAIN), Ok(vec![]));
        let (n, data) = clusters.chain(5).next().unwrap().unwrap();
        assert_eq!((n, data[0], data.len()), (5, 5, 512));
    }

    #[test]
    fn chain_faults_are_told_apart() {
        // 01 §6.3 and the error table of §10.
        let table = [
            END,
            link(6),                  // 1: runs into a free cluster
            link(0),                  // 2: links to cluster 0
            link(7),                  // 3: links beyond the file
            [2, 0, 0, 0],             // 4: unknown status
            [0xFF, 0x12, 0x34, 0x56], // 5: end of chain, link bytes are ignored
            FREE,
        ];
        let clusters = Clusters::new(image(7, &table), 512).unwrap();
        let chain = |cluster, fault| Err(Broken::Chain { cluster, fault });
        assert_eq!(walk(&clusters, 1), chain(6, ChainFault::Free));
        assert_eq!(walk(&clusters, 6), chain(6, ChainFault::Free));
        assert_eq!(walk(&clusters, 2), chain(0, ChainFault::OutOfRange));
        assert_eq!(walk(&clusters, 3), chain(7, ChainFault::OutOfRange));
        assert_eq!(
            walk(&clusters, 4),
            Err(Broken::Table {
                cluster: 4,
                fault: TableFault::BadStatus
            })
        );
        assert_eq!(walk(&clusters, 5), Ok(vec![5]));
        // References that are no cluster number at all.
        for bad in [0, -2, 7, 8, i32::MAX, i32::MIN] {
            assert_eq!(walk(&clusters, bad), chain(bad, ChainFault::OutOfRange));
        }
    }

    #[test]
    fn a_failed_walk_yields_its_error_once() {
        let clusters = Clusters::new(image(4, &[END, link(2), FREE, END]), 512).unwrap();
        let mut chain = clusters.chain(1);
        assert!(matches!(chain.next(), Some(Ok((1, _)))));
        assert!(matches!(chain.next(), Some(Err(_))));
        assert!(chain.next().is_none());
    }

    #[test]
    fn looping_chains_end_with_an_error() {
        // 1 -> 2 -> 3 -> 1, and a cluster that links to itself.
        let table = [END, link(2), link(3), link(1), link(4)];
        let clusters = Clusters::new(image(5, &table), 512).unwrap();
        // The walk is cut off at the first cluster it reaches again, not
        // after as many steps as the container has clusters.
        assert_eq!(clusters.chain(1).count(), 3 + 1);
        assert_eq!(
            walk(&clusters, 1),
            Err(Broken::Chain {
                cluster: 1,
                fault: ChainFault::Loop
            })
        );
        // Entered from outside the loop: 4 -> 2 -> 3 -> 1 -> 2.
        let table = [END, link(2), link(3), link(1), link(2)];
        let clusters = Clusters::new(image(5, &table), 512).unwrap();
        assert_eq!(clusters.chain(4).count(), 4 + 1);
        assert_eq!(
            walk(&clusters, 4),
            Err(Broken::Chain {
                cluster: 2,
                fault: ChainFault::Loop
            })
        );
        let table = [END, link(2), link(3), link(1), link(4)];
        let clusters = Clusters::new(image(5, &table), 512).unwrap();
        assert_eq!(
            walk(&clusters, 4),
            Err(Broken::Chain {
                cluster: 4,
                fault: ChainFault::Loop
            })
        );
    }

    /// An image with two table pages: 129 clusters of 512 bytes, page 1 in
    /// cluster 128.
    fn two_page_image(first_entry_of_page_0: [u8; 4]) -> Vec<u8> {
        let mut bytes = vec![0u8; 130 * CS];
        bytes[CS..CS + 4].copy_from_slice(&first_entry_of_page_0);
        // Page 1 describes itself as the last table page.
        bytes[129 * CS..129 * CS + 4].copy_from_slice(&END);
        // Cluster 127 (page 0) -> cluster 129? There is none: the last
        // cluster is 128. Chain 126 -> 127 stays within page 0.
        bytes[CS + 126 * 4..CS + 126 * 4 + 4].copy_from_slice(&link(127));
        bytes[CS + 127 * 4..CS + 127 * 4 + 4].copy_from_slice(&END);
        bytes
    }

    #[test]
    fn table_pages_are_linked_through_their_first_entry() {
        // 01 §6.2.
        let clusters = Clusters::new(two_page_image(link(128)), 512).unwrap();
        assert_eq!(clusters.cluster_count(), 129);
        assert_eq!(clusters.table_pages, 2);
        assert_eq!(walk(&clusters, 126), Ok(vec![126, 127]));
        // The entry of cluster 128 is the first entry of page 1.
        assert_eq!(clusters.table_entry(128), Some(END));
        assert!(clusters.is_table_page(0) && clusters.is_table_page(128));
        assert!(!clusters.is_table_page(1) && !clusters.is_table_page(127));

        // A link to anything but own + entries_per_page is corruption.
        for bad in [link(127), link(129), link(1), FREE, [2, 128, 0, 0]] {
            assert!(
                matches!(
                    Clusters::new(two_page_image(bad), 512),
                    Err(VaultError::CorruptAllocationTable {
                        cluster: 0,
                        fault: TableFault::BadPageLink
                    })
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn table_pages_are_no_chain_members() {
        // 01 §6.2, §6.3: page 1 is cluster 128 and its own entry says "end
        // of chain", which is what a one-cluster file looks like. It is the
        // table all the same, and so is no part of any chain: neither as the
        // first cluster nor as the target of a link.
        let table_page = Broken::Chain {
            cluster: 128,
            fault: ChainFault::TablePage,
        };
        let mut bytes = two_page_image(link(128));
        let clusters = Clusters::new(bytes.clone(), 512).unwrap();
        assert_eq!(walk(&clusters, 128), Err(table_page));
        // 126 -> 127 -> 128: refused at the link, before 127 is handed out.
        bytes[CS + 127 * 4..CS + 127 * 4 + 4].copy_from_slice(&link(128));
        let clusters = Clusters::new(bytes, 512).unwrap();
        let steps: Vec<_> = clusters.chain(126).collect();
        assert_eq!(steps.len(), 2);
        assert!(matches!(steps[0], Ok((126, _))));
        assert_eq!(steps[1], Err(table_page));
        // Cluster 0 keeps the error 01 §6.3 gives it.
        assert_eq!(
            walk(&clusters, 0),
            Err(Broken::Chain {
                cluster: 0,
                fault: ChainFault::OutOfRange
            })
        );
    }

    #[test]
    fn a_table_page_beyond_the_file_is_a_truncation() {
        // 01 §10: page 0 announces page 1 in cluster 128, the file ends
        // before it.
        let mut bytes = two_page_image(link(128));
        bytes.truncate(129 * CS);
        assert!(matches!(
            Clusters::new(bytes, 512),
            Err(VaultError::Truncated { cluster: 128 })
        ));
    }

    #[test]
    fn clusters_without_a_table_page_have_no_entry() {
        // 01 §6.2: page 0 says it is the last page, but the file has a
        // cluster 128 that only page 1 could describe.
        let mut bytes = two_page_image(END);
        bytes[CS + 127 * 4..CS + 127 * 4 + 4].copy_from_slice(&link(128));
        let clusters = Clusters::new(bytes, 512).unwrap();
        assert_eq!(clusters.table_pages, 1);
        assert_eq!(
            walk(&clusters, 128),
            Err(Broken::Table {
                cluster: 128,
                fault: TableFault::NoEntry
            })
        );
        // The walk reads cluster 126 and 127, then fails on 128.
        let results: Vec<_> = clusters.chain(126).collect();
        assert_eq!(results.len(), 3);
        assert!(results[2].is_err());
    }

    #[test]
    fn errors_are_mapped_by_what_was_being_read() {
        let chain = Broken::chain(9, ChainFault::Free);
        assert!(matches!(
            chain.in_file(),
            VaultError::CorruptChain {
                cluster: 9,
                fault: ChainFault::Free
            }
        ));
        assert!(matches!(
            chain.in_directory(),
            VaultError::CorruptDirectory {
                cluster: 9,
                fault: ChainFault::Free
            }
        ));
        let table = Broken::table(9, TableFault::BadStatus);
        assert!(matches!(
            table.in_file(),
            VaultError::CorruptAllocationTable { cluster: 9, .. }
        ));
        assert!(matches!(
            table.in_directory(),
            VaultError::CorruptAllocationTable { cluster: 9, .. }
        ));
    }
}
