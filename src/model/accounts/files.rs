//! The account files of a vault and what a loader does with their records
//! (03 Part 1: "Paths", "`Accts` and `AcctsNew`", "What is loaded when").

use super::account::{Account, DOCUMENTED_RECORD_VERSION};
use super::group::AccountGroup;
use crate::model::base::{load_collection, LoadReport};

/// Name of the root entry that holds the accounts.
pub const ACCOUNTS_FILE: &str = "AcctsNew";

/// Former name of that entry. It holds the accounts only in a vault that was
/// last written before the rename; in a migrated vault it holds a single
/// placeholder record.
pub const LEGACY_ACCOUNTS_FILE: &str = "Accts";

/// Name of the root entry that holds the account groups.
pub const ACCOUNT_GROUPS_FILE: &str = "AcctGroups";

/// What the name of an account's directory starts with.
const ACCOUNT_DIRECTORY_PREFIX: &str = "Acct";

/// Which entry holds the accounts of a vault, given which of the two exist
/// (03 "`Accts` and `AcctsNew`"): [`ACCOUNTS_FILE`] whenever it exists,
/// because `Accts` is then a placeholder or stale; otherwise
/// [`LEGACY_ACCOUNTS_FILE`]; `None` for a vault without accounts.
///
/// The other of the two is not read at all.
#[must_use]
pub const fn authoritative_accounts_file(
    has_accts_new: bool,
    has_accts: bool,
) -> Option<&'static str> {
    if has_accts_new {
        Some(ACCOUNTS_FILE)
    } else if has_accts {
        Some(LEGACY_ACCOUNTS_FILE)
    } else {
        None
    }
}

/// Name of the directory that holds everything belonging to the account with
/// this object id (03 "Paths"): the prefix followed by the id as a signed
/// decimal number without padding, for example `Acct800000001`.
#[must_use]
pub fn account_directory_name(oid: i32) -> String {
    format!("{ACCOUNT_DIRECTORY_PREFIX}{oid}")
}

/// The object id an account directory is named after, or `None` if `name` is
/// not the name of such a directory.
///
/// Only the exact spelling of [`account_directory_name`] is accepted, so that
/// `Acct007` or `Acct+7` is an unknown entry rather than a second directory
/// of account 7, and the file names `Accts`, `AcctsNew` and `AcctGroups` are
/// never taken for one. Names are compared byte for byte (01 §7.3).
#[must_use]
pub fn account_directory_oid(name: &str) -> Option<i32> {
    let oid: i32 = name.strip_prefix(ACCOUNT_DIRECTORY_PREFIX)?.parse().ok()?;
    (account_directory_name(oid) == name).then_some(oid)
}

/// Remove the records of kind none, which every account loader drops (03
/// "`Accts` and `AcctsNew`"): the placeholder of a migrated vault must never
/// become an account. Returns the accounts and how many records were removed.
#[must_use]
pub fn skip_placeholders(records: Vec<Account>) -> (Vec<Account>, usize) {
    let total = records.len();
    let accounts: Vec<Account> = records
        .into_iter()
        .filter(|account| !account.is_placeholder())
        .collect();
    let placeholders = total - accounts.len();
    (accounts, placeholders)
}

/// Put accounts into the order Banking4 keeps them in: by `position`, then by
/// object id (03 "What is loaded when"). File order is not reliable.
pub fn sort_accounts(accounts: &mut [Account]) {
    accounts.sort_by_key(|account| (account.position, account.base.oid));
}

/// Put account groups into the order Banking4 keeps them in: by `position`,
/// then by object id.
pub fn sort_account_groups(groups: &mut [AccountGroup]) {
    groups.sort_by_key(|group| (group.position, group.base.oid));
}

/// The accounts of one account file and what loading them ran into.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedAccounts {
    /// The accounts in file order, without the records of kind none.
    pub accounts: Vec<Account>,
    /// Records of kind none that were dropped. They are part of
    /// `report.decoded`.
    pub placeholders: usize,
    /// Decoded records, placeholders included, whose record version is above
    /// [`DOCUMENTED_RECORD_VERSION`]: written by a newer program version,
    /// which may store fields this version does not read. The bytes of such
    /// fields show up in `report.unread_bytes`.
    pub newer_versions: usize,
    /// The highest record version among the decoded records, 0 without any.
    pub highest_version: u8,
    /// What the shared loader counted for the file.
    pub report: LoadReport,
}

impl LoadedAccounts {
    /// The warnings to show for this file: those of the shared loader, and
    /// one more if records carry a version newer than the documented one.
    /// `file` is the name of the vault entry. Counts and version numbers
    /// only, never a value from a record.
    #[must_use]
    pub fn warnings(&self, file: &str) -> Vec<String> {
        let mut warnings = self.report.warnings(file);
        if self.newer_versions > 0 {
            warnings.push(format!(
                "{file}: record version above {DOCUMENTED_RECORD_VERSION} in {} of {} account \
                 record{} (highest: {}); fields added since are not read",
                self.newer_versions,
                self.report.decoded,
                if self.report.decoded == 1 { "" } else { "s" },
                self.highest_version
            ));
        }
        warnings
    }
}

/// Load an account file (`AcctsNew` or `Accts`): decode every record with
/// the tolerance of [`load_collection`], drop the records of kind none and
/// count the records of a version newer than the documented one.
///
/// Like the shared loader it never fails and never panics.
#[must_use]
pub fn load_accounts(bytes: &[u8]) -> LoadedAccounts {
    let collection = load_collection::<Account>(bytes);
    let newer_versions = collection
        .records
        .iter()
        .filter(|account| account.is_newer_version())
        .count();
    let highest_version = collection
        .records
        .iter()
        .map(|account| account.record_version)
        .max()
        .unwrap_or(0);
    let (accounts, placeholders) = skip_placeholders(collection.records);
    LoadedAccounts {
        accounts,
        placeholders,
        newer_versions,
        highest_version,
        report: collection.report,
    }
}

#[cfg(test)]
mod tests {
    use super::super::sample::{file, Sample};
    use super::super::AccountKind;
    use super::*;
    use crate::model::base::RecordBase;

    #[test]
    fn accts_new_is_authoritative_whenever_it_exists() {
        assert_eq!(authoritative_accounts_file(true, false), Some("AcctsNew"));
        // Both: `Accts` is the placeholder (or stale) and is ignored.
        assert_eq!(authoritative_accounts_file(true, true), Some("AcctsNew"));
        // Only the old name: a vault from before the rename.
        assert_eq!(authoritative_accounts_file(false, true), Some("Accts"));
        assert_eq!(authoritative_accounts_file(false, false), None);
    }

    #[test]
    fn an_account_directory_is_named_after_the_object_id() {
        assert_eq!(account_directory_name(800_000_001), "Acct800000001");
        assert_eq!(account_directory_name(0), "Acct0");
        // Signed decimal, no padding.
        assert_eq!(account_directory_name(-5), "Acct-5");
        assert_eq!(account_directory_name(i32::MIN), "Acct-2147483648");
        assert_eq!(account_directory_name(i32::MAX), "Acct2147483647");
    }

    #[test]
    fn only_the_exact_spelling_is_an_account_directory() {
        for oid in [0, 1, -5, 800_000_001, i32::MIN, i32::MAX] {
            assert_eq!(
                account_directory_oid(&account_directory_name(oid)),
                Some(oid)
            );
        }
        for name in [
            "Acct",
            "Accts",
            "AcctsNew",
            "AcctGroups",
            "AcctStmtFilters",
            "Acct007",
            "Acct+7",
            "Acct-0",
            "Acct 7",
            "Acct7 ",
            "Acct7.txt",
            "acct7",
            "ACCT7",
            "XAcct7",
            "Acct2147483648",
            "Acct\u{0667}",
            "Stock7",
            "",
        ] {
            assert_eq!(account_directory_oid(name), None, "{name:?}");
        }
    }

    #[test]
    fn records_of_kind_none_are_dropped_and_newer_versions_counted() {
        let bytes = file(&[
            Sample {
                oid: 1,
                version: 27,
                trailing: vec![0; 10],
                ..Sample::default()
            }
            .record(),
            // The placeholder of a migrated vault.
            Sample {
                oid: 2,
                kind: 0,
                ..Sample::default()
            }
            .record(),
            // The test is on the low eight bits.
            Sample {
                oid: 3,
                kind: 0x100,
                version: 28,
                ..Sample::default()
            }
            .record(),
            Sample {
                oid: 4,
                kind: 0x02,
                ..Sample::default()
            }
            .record(),
        ]);
        let loaded = load_accounts(&bytes);
        let oids: Vec<i32> = loaded
            .accounts
            .iter()
            .map(|account| account.base.oid)
            .collect();
        assert_eq!(oids, [1, 4]);
        assert_eq!(loaded.accounts[1].account_kind, AccountKind::Cash);
        assert_eq!(loaded.placeholders, 2);
        assert_eq!(loaded.report.decoded, 4);
        assert_eq!(loaded.newer_versions, 2);
        assert_eq!(loaded.highest_version, 28);

        let warnings = loaded.warnings("AcctsNew");
        assert_eq!(
            warnings,
            [
                "AcctsNew: bytes this version does not read in 1 of 4 account records \
                 (chunk 1: 10 bytes in 1 record)",
                "AcctsNew: record version above 26 in 2 of 4 account records (highest: 28); \
                 fields added since are not read",
            ]
        );
    }

    #[test]
    fn a_file_of_the_documented_version_loads_without_warnings() {
        let loaded = load_accounts(&file(&[Sample::default().record()]));
        assert_eq!(loaded.accounts.len(), 1);
        assert_eq!(loaded.placeholders, 0);
        assert_eq!(loaded.newer_versions, 0);
        assert_eq!(loaded.highest_version, 26);
        assert!(loaded.warnings("AcctsNew").is_empty());

        let empty = load_accounts(&[]);
        assert!(empty.accounts.is_empty());
        assert_eq!(empty.highest_version, 0);
        assert!(empty.warnings("AcctsNew").is_empty());
    }

    #[test]
    fn accounts_and_groups_sort_by_position_then_id() {
        let mut accounts = load_accounts(&file(&[
            Sample {
                oid: 30,
                position: 2,
                ..Sample::default()
            }
            .record(),
            Sample {
                oid: 20,
                position: 65535,
                ..Sample::default()
            }
            .record(),
            Sample {
                oid: 10,
                position: 2,
                ..Sample::default()
            }
            .record(),
            Sample {
                oid: 40,
                position: 0,
                ..Sample::default()
            }
            .record(),
        ]))
        .accounts;
        sort_accounts(&mut accounts);
        let oids: Vec<i32> = accounts.iter().map(|account| account.base.oid).collect();
        assert_eq!(oids, [40, 10, 30, 20]);

        let group = |oid, position| AccountGroup {
            base: RecordBase {
                oid,
                update_stamp: 0,
            },
            position,
            name: None,
            description: None,
            flags: super::super::GroupFlags::default(),
        };
        let mut groups = [group(3, 1), group(2, 0), group(1, 1)];
        sort_account_groups(&mut groups);
        let oids: Vec<i32> = groups.iter().map(|group| group.base.oid).collect();
        assert_eq!(oids, [2, 1, 3]);
    }
}
