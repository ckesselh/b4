//! Accounts and account groups (docs/format/03-model-and-accounts.md).
//!
//! [`Account`] and [`AccountGroup`] are the records of `AcctsNew` and
//! `AcctGroups`. Both implement [`Record`](crate::model::base::Record), so
//! [`load_collection`](crate::model::base::load_collection) reads either
//! file; [`load_accounts`] adds what only the account file needs: dropping
//! the placeholder records and counting records of a newer layout.
//!
//! The module also answers the questions of the document's file map that
//! concern accounts: which of the two account files to read
//! ([`authoritative_accounts_file`]) and how an account's directory is named
//! ([`account_directory_name`], [`account_directory_oid`]).
//!
//! The settings file and the saved booking filters of the same document are
//! deliberately not decoded: `ModelSettings` holds secrets and `b4` never
//! reads it.

mod account;
mod files;
mod group;
mod kinds;
mod retention;
#[cfg(test)]
mod sample;

pub use account::{Account, DOCUMENTED_RECORD_VERSION};
pub use files::{
    account_directory_name, account_directory_oid, authoritative_accounts_file, load_accounts,
    skip_placeholders, sort_account_groups, sort_accounts, LoadedAccounts, ACCOUNTS_FILE,
    ACCOUNT_GROUPS_FILE, LEGACY_ACCOUNTS_FILE,
};
pub use group::AccountGroup;
pub use kinds::{
    AccountFlags, AccountKind, BalanceFlags, BookkeepingFlags, GroupFlags, NotifyFlags,
    StatementSchedule, SyncFlags,
};
pub use retention::{RetentionPeriod, RetentionUnit};
