//! Reads a vault into the model. The list of entries b4 reads lives here.
//!
//! [`load`] walks the root directory of an unlocked vault and exactly one
//! level below it. What happens to an entry is decided by its *name*, before
//! a byte of its content is read ([`classify_root`], [`classify_in_account`],
//! [`classify_in_stock`]). There are four classes:
//!
//! 1. **decoded**: the collections of [`Snapshot`];
//! 2. **bank access data** ([`NEVER_READ`]): never read, only counted;
//! 3. **known, not exported** ([`NOT_EXPORTED`] and a few names inside account
//!    directories): skipped without a message;
//! 4. **unknown**: skipped unread, with one warning that names the entry.
//!
//! This is an allow-list: content is read only for the names of class 1, so
//! a name nobody has heard of is as safe as one of class 2.
//!
//! # The fence
//!
//! A name says nothing about where an entry's bytes are. A directory slot
//! with a name of class 1 can carry the first cluster of an entry of class
//! 2, and since all files of a vault share one key, reading it would decrypt
//! that entry's content. The loader therefore works in two steps and never
//! mixes them:
//!
//! 1. **Fence.** It classifies the root, reserves the storage of every root
//!    entry of class 2, 3 and 4 ([`Vault::reserve`]), then lists the
//!    `Acct<oid>` and `Stock<oid>` directories, classifies what is in them
//!    and reserves every entry of class 3 and 4 there. Up to here no file
//!    content has been read and nothing has been decrypted.
//! 2. **Read.** Only now the first entry of class 1 is read.
//!
//! What this guarantees: no byte of the file data of an entry of class 2, 3
//! or 4 is read or decrypted, under any name, not the entry's own and not
//! that of an entry of class 1 that leads into its clusters. Of a directory
//! of those classes only the unencrypted structure is walked (its pages with
//! the names, sizes and first clusters of what it lists, to any depth), to
//! know which clusters belong to it; nothing below it is read as data. An
//! entry of class 1 whose chain reaches a reserved cluster makes the load
//! fail with [`LoadError::Fenced`] before anything of that chain is copied
//! or decrypted: a vault whose readable entries share storage with entries
//! b4 does not read is damaged or was manipulated, and b4 writes no database
//! from it. The same holds for an `Acct<oid>` or `Stock<oid>` directory
//! whose pages are those of a reserved directory: it is not listed.
//!
//! Reserving is best effort and never fails the load: an entry of class 2 to
//! 4 with a broken chain is reserved as far as its chain can be walked. It
//! costs one pass over the allocation table chains and the directory pages
//! of what is not read, linear in the size of the vault ([`Vault::reserve`]).
//!
//! The fence follows the directories as the vault has them. A vault is not
//! authenticated, so one that was rewritten so that no entry of class 2
//! names the clusters any more (the slot removed or renamed) gives the
//! loader nothing to fence; see the module documentation of
//! [`crate::vault`].
//!
//! The loader also applies the rules that need more than one record: which
//! account file counts, the account-dependent adjustments of bookings, the
//! regrouping of splits and batches, the payment rules, and the marks for
//! records Banking4 itself hides. Nothing is dropped on the way except the
//! placeholder accounts of kind none.
//!
//! Warnings are plain sentences that name entries, record types and counts.
//! They never contain a value from a record.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use zeroize::Zeroizing;

use crate::model::accounts::{
    account_directory_oid, authoritative_accounts_file, load_accounts, Account, AccountGroup,
    ACCOUNTS_FILE, ACCOUNT_GROUPS_FILE, DOCUMENTED_RECORD_VERSION, LEGACY_ACCOUNTS_FILE,
};
use crate::model::base::{load_collection, LoadReport, Record};
use crate::model::masterdata::{
    category_loader_view, duplicate_isins, exchange_rate_view, stock_directory_oid, Asset,
    Category, ExchangeRate, Mandate, Payee, Payer, PortfolioItem, PortfolioTransaction,
    SpecialCategory, StockInstrument, StockPrice, TaxExemption,
};
use crate::model::payments::{
    apply_payment_rules, regroup_bookings, Booking, Payment, StandingOrder,
};
use crate::vault::{Entry, Vault, VaultError};
use crate::wire::{Date, UnreadBytes};

/// Root entries that hold bank access data or secrets. Their content is never
/// read, and nothing in a directory among them is, whatever it contains: the
/// loader reserves their storage before it reads anything (see "The fence"
/// in the [module documentation](self)).
pub const NEVER_READ: [&str; 9] = [
    "Contacts",
    "EbicsContacts",
    "ContactsHTML",
    "SpoolerContacts",
    "OnlineContactKeys",
    "Outbox",
    "OutboxData",
    "ModelSettings",
    "OnlineTraces",
];

/// Root entries b4 knows and does not export: user interface state, logs,
/// pictures and names of old program versions. Skipped without a message.
/// `Accts` joins them when `AcctsNew` exists.
pub const NOT_EXPORTED: [&str; 9] = [
    "AcctStmtFilters",
    "Invoices",
    "ImportExportPaymtsLog",
    "FinPingBanks",
    "Avatars",
    "ExchangeRates",
    "Traces",
    "Layout",
    "TanLists",
];

/// Files of an account directory b4 knows and does not export. The contents
/// of account documents (`<number>.txt`, `.pdf`, `.bin`) belong here too.
pub const NOT_EXPORTED_IN_ACCOUNT: [&str; 3] = ["AcctFiles", "AcctPayChecks", "AcctPayCheckSplits"];

/// A collection file of the root directory that b4 decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootFile {
    /// `AcctsNew`, or `Accts` in a vault without it.
    Accounts,
    /// `AcctGroups`.
    AccountGroups,
    /// `Payees`.
    Payees,
    /// `Payers`.
    Payers,
    /// `Categories`.
    Categories,
    /// `Mandates`.
    Mandates,
    /// `ExchangeRatesBIN`.
    ExchangeRates,
    /// `StockInstruments`.
    StockInstruments,
}

/// What the loader does with an entry of the root directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootClass {
    /// Class 1: a collection file that is decoded.
    Collection(RootFile),
    /// Class 1: the directory `Acct<oid>` of an account.
    AccountDirectory(i32),
    /// Class 1: the directory `Stock<oid>` of an instrument.
    StockDirectory(i32),
    /// Class 2: bank access data. Never read, only counted.
    NeverRead,
    /// Class 3: known and not exported. Skipped silently.
    NotExported,
    /// Class 4: unknown. Skipped unread with a warning.
    Unknown,
}

/// A collection file of an account directory that b4 decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccountFile {
    /// `StmtMasterEntries`: the legacy second booking file.
    LegacyBookings,
    /// `Stmt`.
    Bookings,
    /// `Paymts`.
    Payments,
    /// `Stos`.
    StandingOrders,
    /// `Portf`.
    PortfolioItems,
    /// `PortfTxns`.
    PortfolioTransactions,
    /// `AcctTaxExemptions`.
    TaxExemptions,
    /// `Assets`.
    Assets,
}

/// What the loader does with an entry one level below the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InnerClass<File> {
    /// Class 1: a collection file that is decoded.
    Collection(File),
    /// Class 3: known and not exported. Skipped silently.
    NotExported,
    /// Class 4: unknown. Skipped unread with a warning.
    Unknown,
}

/// Decide what to do with the root entry `name`. `has_accts_new` says whether
/// the root has a file `AcctsNew`, which makes `Accts` a leftover.
///
/// The names of class 2 are tested first and for files and directories alike,
/// so nothing else about an entry can make the loader read one of them.
pub fn classify_root(name: &str, is_dir: bool, has_accts_new: bool) -> RootClass {
    if NEVER_READ.contains(&name) {
        return RootClass::NeverRead;
    }
    if NOT_EXPORTED.contains(&name) {
        return RootClass::NotExported;
    }
    if is_dir {
        // The legacy account file has a name that starts like a directory of
        // an account; `account_directory_oid` accepts digits only.
        return match (account_directory_oid(name), stock_directory_oid(name)) {
            (Some(oid), _) => RootClass::AccountDirectory(oid),
            (None, Some(oid)) => RootClass::StockDirectory(oid),
            (None, None) => RootClass::Unknown,
        };
    }
    let file = match name {
        ACCOUNTS_FILE => RootFile::Accounts,
        LEGACY_ACCOUNTS_FILE => {
            return match authoritative_accounts_file(has_accts_new, true) {
                Some(LEGACY_ACCOUNTS_FILE) => RootClass::Collection(RootFile::Accounts),
                _ => RootClass::NotExported,
            }
        }
        ACCOUNT_GROUPS_FILE => RootFile::AccountGroups,
        "Payees" => RootFile::Payees,
        "Payers" => RootFile::Payers,
        "Categories" => RootFile::Categories,
        "Mandates" => RootFile::Mandates,
        "ExchangeRatesBIN" => RootFile::ExchangeRates,
        "StockInstruments" => RootFile::StockInstruments,
        _ => return RootClass::Unknown,
    };
    RootClass::Collection(file)
}

/// Decide what to do with the entry `name` of an `Acct<oid>` directory.
pub fn classify_in_account(name: &str, is_dir: bool) -> InnerClass<AccountFile> {
    if is_dir {
        // Banking4 never creates a third level.
        return InnerClass::Unknown;
    }
    let file = match name {
        "StmtMasterEntries" => AccountFile::LegacyBookings,
        "Stmt" => AccountFile::Bookings,
        "Paymts" => AccountFile::Payments,
        "Stos" => AccountFile::StandingOrders,
        "Portf" => AccountFile::PortfolioItems,
        "PortfTxns" => AccountFile::PortfolioTransactions,
        "AcctTaxExemptions" => AccountFile::TaxExemptions,
        "Assets" => AccountFile::Assets,
        _ if NOT_EXPORTED_IN_ACCOUNT.contains(&name) || is_document_content(name) => {
            return InnerClass::NotExported
        }
        _ => return InnerClass::Unknown,
    };
    InnerClass::Collection(file)
}

/// Decide what to do with the entry `name` of a `Stock<oid>` directory. The
/// one file there is `StockPrices`.
pub fn classify_in_stock(name: &str, is_dir: bool) -> InnerClass<()> {
    if !is_dir && name == "StockPrices" {
        InnerClass::Collection(())
    } else {
        InnerClass::Unknown
    }
}

/// Whether `name` is the content file of an account document: the document's
/// object id as a decimal number and one of the three extensions (03, "Account
/// kinds and per-account files").
fn is_document_content(name: &str) -> bool {
    let Some((number, extension)) = name.rsplit_once('.') else {
        return false;
    };
    let digits = number.strip_prefix('-').unwrap_or(number);
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(extension, "txt" | "pdf" | "bin")
}

/// A record together with the object id of the account or instrument whose
/// directory it was stored in. The record itself does not carry it.
#[derive(Debug, Clone, PartialEq)]
pub struct Owned<T> {
    /// The `<oid>` of the directory `Acct<oid>` or `Stock<oid>`.
    pub owner: i32,
    /// The record.
    pub record: T,
}

/// A booking, payment or standing order with its account and the currency
/// its amount is in.
#[derive(Debug, Clone, PartialEq)]
pub struct InAccount<T> {
    /// The `<oid>` of the directory `Acct<oid>` the record was stored in.
    pub account_id: i32,
    /// The currency Banking4 shows for the record (04 §5.11, §6.2): for a
    /// booking the account's, also in a PayPal account, for the others the
    /// stored one or what the scheme implies. Where the rule would fall back
    /// to the application's default, which a vault does not record, this is
    /// the stored code.
    pub currency: Option<String>,
    /// The record, after the collection rules.
    pub record: T,
}

/// A record with the mark for what Banking4 does with it when it loads the
/// file.
#[derive(Debug, Clone, PartialEq)]
pub struct Marked<T> {
    /// Banking4 drops the record when it loads the file. b4 keeps it.
    pub hidden_by_banking4: bool,
    /// The record.
    pub record: T,
}

/// A category with what Banking4 makes of it when it loads the file (05
/// §3.3 "Loader").
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedCategory {
    /// Banking4 drops the record when it loads the file. b4 keeps it.
    pub hidden_by_banking4: bool,
    /// The special role the category has in Banking4: the stored one
    /// (`record.special_category`) for the first record of each role, none
    /// for every later record that repeats it and for a record Banking4
    /// drops.
    pub special: SpecialCategory,
    /// The record, with the role as stored.
    pub record: Category,
}

/// Put the marks of Banking4's loading rules on the records of one
/// `Categories` file, in file order.
fn loaded_categories(records: Vec<Category>) -> Vec<LoadedCategory> {
    let marks = category_loader_view(&records);
    records
        .into_iter()
        .zip(marks)
        .map(|(record, mark)| LoadedCategory {
            hidden_by_banking4: mark.dropped,
            special: mark.special_category,
            record,
        })
        .collect()
}

/// Everything b4 decodes from one vault.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// The accounts in file order, without placeholders of kind none.
    pub accounts: Vec<Account>,
    /// The account groups in file order.
    pub account_groups: Vec<AccountGroup>,
    /// The bookings of every account directory, regrouped (04 §6.4).
    pub bookings: Vec<InAccount<Booking>>,
    /// The payments of every account directory, after the rules of 04 §8.3.
    pub payments: Vec<InAccount<Payment>>,
    /// The standing orders of every account directory.
    pub standing_orders: Vec<InAccount<StandingOrder>>,
    /// The address book.
    pub payees: Vec<Payee>,
    /// The counterparties for direct debits.
    pub payers: Vec<Payer>,
    /// The category catalogue.
    pub categories: Vec<LoadedCategory>,
    /// The mandates.
    pub mandates: Vec<Marked<Mandate>>,
    /// The exchange rates.
    pub exchange_rates: Vec<Marked<ExchangeRate>>,
    /// The securities.
    pub stock_instruments: Vec<StockInstrument>,
    /// The prices of every `Stock<oid>` directory.
    pub stock_prices: Vec<Owned<StockPrice>>,
    /// The positions of every account directory.
    pub portfolio_items: Vec<Owned<PortfolioItem>>,
    /// The securities transactions of every account directory.
    pub portfolio_transactions: Vec<Owned<PortfolioTransaction>>,
    /// The tax exemption orders of every account directory.
    pub tax_exemptions: Vec<Owned<TaxExemption>>,
    /// The items of every account directory.
    pub assets: Vec<Owned<Asset>>,
    /// What needs the user's attention, one sentence each.
    pub warnings: Vec<String>,
    /// What is worth knowing and needs no attention, one sentence each.
    pub notes: Vec<String>,
    /// How many root entries of class 2 the vault has. None of them was read.
    pub entries_never_read: usize,
}

/// Why a vault could not be loaded: an entry b4 decodes could not be read
/// from the container.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// Reading the content of an entry, or listing a directory, failed.
    #[error("cannot read the vault entry {entry}")]
    Entry {
        /// The path of the entry inside the vault, control characters
        /// escaped.
        entry: String,
        /// What the container reader reported.
        #[source]
        source: VaultError,
    },
    /// An entry b4 decodes leads into the storage of an entry it does not
    /// read (see "The fence" in the [module documentation](self)). Nothing
    /// of the entry was read.
    #[error("cannot read the vault entry {entry}")]
    Fenced {
        /// The path of the entry that was to be read, control characters
        /// escaped.
        entry: String,
        /// The container reader's [`VaultError::CrossLinked`], which names
        /// both entries and the cluster.
        #[source]
        source: VaultError,
    },
}

impl LoadError {
    /// The error for the entry at `path` that the container did not deliver.
    fn new(vault: &Vault, path: &str, source: VaultError) -> Self {
        let entry = shown(path);
        match source {
            VaultError::CrossLinked { cluster, .. } if vault.is_reserved(cluster) => {
                LoadError::Fenced { entry, source }
            }
            _ => LoadError::Entry { entry, source },
        }
    }
}

/// Read every collection b4 decodes from the unlocked `vault`.
///
/// Before anything is read, the storage of every entry that is not going to
/// be read is reserved in `vault` (see "The fence" in the
/// [module documentation](self)); those entries cannot be read from `vault`
/// afterwards.
///
/// Fails only when the container itself cannot deliver an entry (a damaged
/// cluster chain, an entry that shares storage with one b4 does not read, a
/// locked vault). Damage *inside* a collection file never fails: the records
/// that can be decoded are kept and a warning says what was lost.
pub fn load(vault: &Vault) -> Result<Snapshot, LoadError> {
    let mut loader = Loader::new(vault, today());
    loader.walk_root()?;
    Ok(loader.finish())
}

/// The current day, UTC. It only decides where the collection rules sort
/// records without a date.
fn today() -> Date {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    i32::try_from(seconds / 86_400)
        .ok()
        .and_then(Date::from_days_since_unix_epoch)
        .unwrap_or(Date::MAX)
}

/// An entry name for a message: the name is chosen by whoever wrote the
/// vault, so control characters are escaped and an absurd length is cut.
fn shown(name: &str) -> String {
    const LIMIT: usize = 80;
    let mut text: String = name
        .chars()
        .take(LIMIT)
        .flat_map(char::escape_debug)
        .collect();
    if name.chars().count() > LIMIT {
        text.push_str("...");
    }
    text
}

/// `1 booking record`, `3 booking records`.
fn records(count: usize, record_type: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {record_type} record{plural}")
}

/// `1 file`, `3 files`.
fn files(count: usize) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} file{plural}")
}

/// What all files of one record type have in common, for the aggregated
/// lines.
#[derive(Debug, Default)]
struct TypeTally {
    files: usize,
    decoded: usize,
    files_with_unread: usize,
    unread_records: usize,
    unread_histogram: BTreeMap<UnreadBytes, usize>,
    files_with_legacy: usize,
    legacy_records: usize,
    /// Highest account record version above the documented one, 0 if none.
    newer_version: u8,
}

/// The kind of directory one level below the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
    Account,
    Stock,
}

impl Place {
    const fn noun(self) -> &'static str {
        match self {
            Place::Account => "account",
            Place::Stock => "stock",
        }
    }
}

/// `1 account directory`, `2 stock directories`.
fn directories(count: usize, place: Place) -> String {
    let plural = if count == 1 { "y" } else { "ies" };
    format!("{count} {} director{plural}", place.noun())
}

/// Several directories that share a finding: how many, and the first.
#[derive(Debug, Default)]
struct Seen {
    count: usize,
    first: String,
}

impl Seen {
    fn add(&mut self, directory: &str) {
        if self.count == 0 {
            self.first = shown(directory);
        }
        self.count += 1;
    }
}

/// What the account-dependent rules need to know about an account.
#[derive(Debug, Clone, Default)]
struct AccountFacts {
    kind: u8,
    currency: Option<String>,
}

struct Loader<'a> {
    vault: &'a Vault,
    today: Date,
    snapshot: Snapshot,
    accounts: BTreeMap<i32, AccountFacts>,
    /// The object ids of the stock instruments, for the directories that
    /// refer to them.
    instruments: BTreeSet<i32>,
    tallies: BTreeMap<&'static str, TypeTally>,
    /// Unknown entries below the root, by place, name and kind of entry.
    unknown_inner: BTreeMap<(Place, String, bool), Seen>,
    orphans: BTreeMap<Place, Seen>,
    legacy_booking_files: Seen,
    legacy_bookings: usize,
}

impl<'a> Loader<'a> {
    fn new(vault: &'a Vault, today: Date) -> Self {
        Self {
            vault,
            today,
            snapshot: Snapshot::default(),
            accounts: BTreeMap::new(),
            instruments: BTreeSet::new(),
            tallies: BTreeMap::new(),
            unknown_inner: BTreeMap::new(),
            orphans: BTreeMap::new(),
            legacy_booking_files: Seen::default(),
            legacy_bookings: 0,
        }
    }

    fn warn(&mut self, message: String) {
        self.snapshot.warnings.push(message);
    }

    /// The content of the file `entry`. Only ever called for class 1, and
    /// only after [`fence`](Self::fence).
    ///
    /// The buffer is wiped when it is dropped, which is as soon as the file
    /// has been decoded. It is the plaintext of a whole collection file, and
    /// for accounts and payments that includes the bank access fields the
    /// decoder passes over: they must not stay behind in freed memory.
    /// [`Vault::read_file`] decrypts in this one buffer and makes no other
    /// copy, and the image of the vault that stays in memory is ciphertext
    /// for every encrypted entry. The decoded records are not wiped: they
    /// hold what b4 exports.
    fn read(&self, entry: &Entry, path: &str) -> Result<Zeroizing<Vec<u8>>, LoadError> {
        self.vault
            .read_file(entry)
            .map(Zeroizing::new)
            .map_err(|source| LoadError::new(self.vault, path, source))
    }

    /// Read and decode the collection file `entry` and account for what the
    /// decoder found.
    fn collection<T: Record>(&mut self, entry: &Entry, path: &str) -> Result<Vec<T>, LoadError> {
        let bytes = self.read(entry, path)?;
        let collection = load_collection::<T>(&bytes);
        self.account_for(path, &collection.report);
        Ok(collection.records)
    }

    /// Turn the report of one file into per-file warnings for what is wrong
    /// with that file, and into the per-type tallies for what is merely a
    /// property of the program version that wrote it.
    fn account_for(&mut self, path: &str, report: &LoadReport) {
        let path = shown(path);
        let kind = report.record_type;
        if let Some(stop) = &report.stopped {
            self.warn(format!(
                "{path}: reading stopped at {stop}; kept the {} before it",
                records(report.decoded, kind)
            ));
        }
        if report.undecodable > 0 {
            let first = report
                .undecodable_samples
                .first()
                .map(|sample| format!(" (first: {sample})"))
                .unwrap_or_default();
            self.warn(format!(
                "{path}: skipped {} that cannot be decoded{first}",
                records(report.undecodable, kind)
            ));
        }
        if !report.extra_chunks.is_zero() {
            self.warn(format!(
                "{path}: {} of {} have chunks from a newer Banking4 version that b4 does not \
                 read ({} skipped)",
                report.extra_chunks.records,
                records(report.decoded, kind),
                report.extra_chunk_count
            ));
        }
        if !report.overlong_strings.is_zero() {
            self.warn(format!(
                "{path}: a text runs past its chunk in {}; the fields after it were read as empty",
                records(report.overlong_strings.records, kind)
            ));
        }
        if !report.duplicate_oids.is_zero() {
            self.warn(format!(
                "{path}: repeated object id in {}; all were kept",
                records(report.duplicate_oids.records, kind)
            ));
        }

        let tally = self.tallies.entry(kind).or_default();
        tally.files += 1;
        tally.decoded += report.decoded;
        if !report.unread_bytes.is_zero() {
            tally.files_with_unread += 1;
            tally.unread_records += report.unread_bytes.records;
            for (unread, count) in &report.unread_histogram {
                *tally.unread_histogram.entry(*unread).or_default() += count;
            }
        }
        if !report.legacy.is_zero() {
            tally.files_with_legacy += 1;
            tally.legacy_records += report.legacy.records;
        }
    }

    fn walk_root(&mut self) -> Result<(), LoadError> {
        let vault = self.vault;
        let root = vault.root();
        let has_accts_new = root
            .iter()
            .any(|entry| !entry.is_dir() && entry.name() == ACCOUNTS_FILE);

        // The class of every entry is settled from the listing alone, before
        // anything is read.
        let classes: Vec<RootClass> = root
            .iter()
            .map(|entry| classify_root(entry.name(), entry.is_dir(), has_accts_new))
            .collect();
        let of_class = |wanted: fn(&RootClass) -> bool| {
            root.iter()
                .zip(&classes)
                .filter(move |(_, class)| wanted(class))
        };

        // Nothing is read before this has come back.
        let listings = self.fence(root, &classes)?;

        self.snapshot.entries_never_read = classes
            .iter()
            .filter(|class| **class == RootClass::NeverRead)
            .count();
        for (entry, _) in of_class(|class| *class == RootClass::Unknown) {
            let name = shown(entry.name());
            self.warn(if entry.is_dir() {
                format!("unknown directory \"{name}\" in the vault: skipped without reading it")
            } else {
                format!(
                    "unknown entry \"{name}\" in the vault ({} bytes): skipped without reading it",
                    entry.size()
                )
            });
        }

        // Accounts and instruments first: the directories refer to them.
        let mut found_accounts = false;
        for (entry, class) in of_class(|class| matches!(class, RootClass::Collection(_))) {
            let RootClass::Collection(file) = class else {
                continue;
            };
            let path = entry.name();
            match file {
                RootFile::Accounts => {
                    found_accounts = true;
                    self.accounts_file(entry, path)?;
                }
                RootFile::AccountGroups => {
                    let records = self.collection(entry, path)?;
                    self.snapshot.account_groups.extend(records);
                }
                RootFile::Payees => {
                    let records = self.collection(entry, path)?;
                    self.snapshot.payees.extend(records);
                }
                RootFile::Payers => {
                    let records = self.collection(entry, path)?;
                    self.snapshot.payers.extend(records);
                }
                RootFile::Categories => {
                    let records: Vec<Category> = self.collection(entry, path)?;
                    self.snapshot.categories.extend(loaded_categories(records));
                }
                RootFile::Mandates => {
                    let records: Vec<Mandate> = self.collection(entry, path)?;
                    let marked = records.into_iter().map(|record| Marked {
                        hidden_by_banking4: record.loader_drop_reason().is_some(),
                        record,
                    });
                    self.snapshot.mandates.extend(marked);
                }
                RootFile::ExchangeRates => {
                    let records: Vec<ExchangeRate> = self.collection(entry, path)?;
                    let views = exchange_rate_view(&records);
                    let marked = records.into_iter().zip(views).map(|(record, view)| Marked {
                        hidden_by_banking4: view.dropped.is_some(),
                        record,
                    });
                    self.snapshot.exchange_rates.extend(marked);
                }
                RootFile::StockInstruments => {
                    let records: Vec<StockInstrument> = self.collection(entry, path)?;
                    if let Some(warning) = split_warning(path, &records) {
                        self.warn(warning);
                    }
                    self.snapshot.stock_instruments.extend(records);
                }
            }
        }
        if !found_accounts {
            self.warn(format!(
                "the vault has no account file ({ACCOUNTS_FILE} or {LEGACY_ACCOUNTS_FILE}): \
                 there are no accounts"
            ));
        }
        // Over all instruments, like the index Banking4 builds (05 §3.6).
        let repeated = duplicate_isins(&self.snapshot.stock_instruments).len();
        if repeated > 0 {
            self.warn(format!(
                "StockInstruments: repeated ISIN in {}; all were kept, so an ISIN can name more \
                 than one instrument",
                records(repeated, StockInstrument::NAME)
            ));
        }
        // One index for all directories, not one search per directory.
        self.instruments = self
            .snapshot
            .stock_instruments
            .iter()
            .map(|instrument| instrument.base.oid)
            .collect();

        for ((entry, class), listing) in root.iter().zip(&classes).zip(listings) {
            match (class, listing) {
                (RootClass::AccountDirectory(oid), Some(listing)) => {
                    self.account_directory(entry, *oid, listing)?;
                }
                (RootClass::StockDirectory(oid), Some(listing)) => {
                    self.stock_directory(entry, *oid, listing)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Keep everything that is not going to be read out of reach, before the
    /// first read: reserve the storage of every root entry of class 2, 3 and
    /// 4, then list the directories of class 1 and reserve what is of class
    /// 3 and 4 in them. Returns the listing of each directory of class 1,
    /// `None` for every other root entry, in the order of `root`.
    ///
    /// The order matters. The root comes first, so that a directory of class
    /// 1 whose pages are those of a reserved directory is not even listed.
    /// And all of it comes before the first file is read, so that a file of
    /// class 1 that leads into reserved clusters, wherever it is, finds them
    /// taken. This reads the allocation table and directory pages only.
    fn fence(
        &self,
        root: &[Entry],
        classes: &[RootClass],
    ) -> Result<Vec<Option<Vec<Entry>>>, LoadError> {
        let vault = self.vault;
        for (entry, class) in root.iter().zip(classes) {
            if matches!(
                class,
                RootClass::NeverRead | RootClass::NotExported | RootClass::Unknown
            ) {
                vault.reserve(entry);
            }
        }
        let mut listings = Vec::with_capacity(root.len());
        for (entry, class) in root.iter().zip(classes) {
            let is_read: fn(&Entry) -> bool = match class {
                RootClass::AccountDirectory(_) => |entry| {
                    matches!(
                        classify_in_account(entry.name(), entry.is_dir()),
                        InnerClass::Collection(_)
                    )
                },
                RootClass::StockDirectory(_) => |entry| {
                    matches!(
                        classify_in_stock(entry.name(), entry.is_dir()),
                        InnerClass::Collection(())
                    )
                },
                _ => {
                    listings.push(None);
                    continue;
                }
            };
            let listing = self.list(entry)?;
            for inner in listing.iter().filter(|inner| !is_read(inner)) {
                vault.reserve(inner);
            }
            listings.push(Some(listing));
        }
        Ok(listings)
    }

    fn accounts_file(&mut self, entry: &Entry, path: &str) -> Result<(), LoadError> {
        let bytes = self.read(entry, path)?;
        let loaded = load_accounts(&bytes);
        self.account_for(path, &loaded.report);
        if loaded.newer_versions > 0 {
            let tally = self.tallies.entry(loaded.report.record_type).or_default();
            tally.newer_version = tally.newer_version.max(loaded.highest_version);
        }
        for account in &loaded.accounts {
            // The first record of an id decides, as the first row does in the
            // views of the database.
            self.accounts
                .entry(account.base.oid)
                .or_insert_with(|| AccountFacts {
                    kind: account.account_kind.raw(),
                    currency: account.currency.clone(),
                });
        }
        self.snapshot.accounts.extend(loaded.accounts);
        Ok(())
    }

    fn list(&self, directory: &Entry) -> Result<Vec<Entry>, LoadError> {
        self.vault
            .read_dir(directory)
            .map_err(|source| LoadError::new(self.vault, directory.name(), source))
    }

    /// Read the files of class 1 of the account directory `directory`, whose
    /// entries are `listing`.
    fn account_directory(
        &mut self,
        directory: &Entry,
        oid: i32,
        listing: Vec<Entry>,
    ) -> Result<(), LoadError> {
        let name = directory.name();
        let facts = match self.accounts.get(&oid) {
            Some(facts) => facts.clone(),
            None => {
                self.orphans.entry(Place::Account).or_default().add(name);
                // 03, "Orphan directories": as for an account without a
                // currency whose kind is neither cash nor PayPal.
                AccountFacts::default()
            }
        };

        // Classify the whole listing, then read in a fixed order: the legacy
        // booking file before `Stmt`, whatever the directory's order is.
        let mut wanted: Vec<(AccountFile, Entry)> = Vec::new();
        for entry in listing {
            match classify_in_account(entry.name(), entry.is_dir()) {
                InnerClass::Collection(file) => wanted.push((file, entry)),
                InnerClass::NotExported => {}
                InnerClass::Unknown => {
                    let key = (Place::Account, entry.name().to_owned(), entry.is_dir());
                    self.unknown_inner.entry(key).or_default().add(name);
                }
            }
        }
        wanted.sort_by_key(|(file, _)| *file);

        let mut bookings: Vec<Booking> = Vec::new();
        for (file, entry) in &wanted {
            let path = format!("{name}/{}", entry.name());
            match file {
                AccountFile::LegacyBookings => {
                    let records: Vec<Booking> = self.collection(entry, &path)?;
                    self.legacy_booking_files.add(name);
                    self.legacy_bookings += records.len();
                    bookings.extend(records);
                }
                AccountFile::Bookings => bookings.extend(self.collection(entry, &path)?),
                AccountFile::Payments => self.payments(entry, &path, oid, &facts)?,
                AccountFile::StandingOrders => {
                    let records: Vec<StandingOrder> = self.collection(entry, &path)?;
                    let rows = records.into_iter().map(|record| InAccount {
                        account_id: oid,
                        currency: record
                            .displayed_currency(facts.currency.as_deref())
                            .map(str::to_owned),
                        record,
                    });
                    self.snapshot.standing_orders.extend(rows);
                }
                AccountFile::PortfolioItems => {
                    let records = self.collection(entry, &path)?;
                    self.snapshot.portfolio_items.extend(owned(oid, records));
                }
                AccountFile::PortfolioTransactions => {
                    let records = self.collection(entry, &path)?;
                    self.snapshot
                        .portfolio_transactions
                        .extend(owned(oid, records));
                }
                AccountFile::TaxExemptions => {
                    let records = self.collection(entry, &path)?;
                    self.snapshot.tax_exemptions.extend(owned(oid, records));
                }
                AccountFile::Assets => {
                    let records = self.collection(entry, &path)?;
                    self.snapshot.assets.extend(owned(oid, records));
                }
            }
        }

        // 04 §6.2 step 4, then the regrouping of 04 §6.4 over the bookings of
        // both files together.
        for booking in &mut bookings {
            booking.apply_account_kind(facts.kind);
        }
        regroup_bookings(&mut bookings, self.today);
        // The currency Banking4 shows, which is the account's in a PayPal
        // account too. What it keeps in the record there (the stored code,
        // `Booking::adjusted_currency`) stays in `record.base.currency`.
        let rows = bookings.into_iter().map(|record| InAccount {
            account_id: oid,
            currency: record
                .displayed_currency(facts.currency.as_deref())
                .map(str::to_owned),
            record,
        });
        self.snapshot.bookings.extend(rows);
        Ok(())
    }

    fn payments(
        &mut self,
        entry: &Entry,
        path: &str,
        oid: i32,
        facts: &AccountFacts,
    ) -> Result<(), LoadError> {
        let mut records: Vec<Payment> = self.collection(entry, path)?;
        let rules = apply_payment_rules(&mut records, self.today);
        if rules.legacy_master_conflicts > 0 {
            self.warn(format!(
                "{}: {} of an old layout repeat the batch number of an earlier one; each was \
                 kept as a batch of its own",
                shown(path),
                records_named(rules.legacy_master_conflicts, "batch master")
            ));
        }
        let rows = records.into_iter().map(|record| InAccount {
            account_id: oid,
            currency: record
                .displayed_currency(facts.currency.as_deref())
                .map(str::to_owned),
            record,
        });
        self.snapshot.payments.extend(rows);
        Ok(())
    }

    /// Read the price file of the stock directory `directory`, whose entries
    /// are `listing`.
    fn stock_directory(
        &mut self,
        directory: &Entry,
        oid: i32,
        listing: Vec<Entry>,
    ) -> Result<(), LoadError> {
        let name = directory.name();
        if !self.instruments.contains(&oid) {
            self.orphans.entry(Place::Stock).or_default().add(name);
        }
        for entry in listing {
            match classify_in_stock(entry.name(), entry.is_dir()) {
                InnerClass::Collection(()) => {
                    let path = format!("{name}/{}", entry.name());
                    let records = self.collection(&entry, &path)?;
                    self.snapshot.stock_prices.extend(owned(oid, records));
                }
                InnerClass::NotExported => {}
                InnerClass::Unknown => {
                    let key = (Place::Stock, entry.name().to_owned(), entry.is_dir());
                    self.unknown_inner.entry(key).or_default().add(name);
                }
            }
        }
        Ok(())
    }

    /// Add the aggregated lines and hand out the snapshot.
    fn finish(mut self) -> Snapshot {
        let mut warnings = Vec::new();
        let mut notes = Vec::new();

        for ((place, name, is_dir), seen) in &self.unknown_inner {
            warnings.push(unknown_inner_warning(*place, name, *is_dir, seen));
        }
        for (place, seen) in &self.orphans {
            warnings.push(orphan_warning(*place, seen));
        }
        if self.legacy_booking_files.count > 0 {
            warnings.push(format!(
                "{} (first: {}) hold the legacy booking file StmtMasterEntries: its {} were \
                 added to those of Stmt",
                directories(self.legacy_booking_files.count, Place::Account),
                self.legacy_booking_files.first,
                records(self.legacy_bookings, Booking::NAME)
            ));
        }
        for (record_type, tally) in &self.tallies {
            if let Some(warning) = unread_warning(record_type, tally) {
                warnings.push(warning);
            }
            if let Some(note) = legacy_note(record_type, tally) {
                notes.push(note);
            }
        }

        self.snapshot.warnings.extend(warnings);
        self.snapshot.notes.extend(notes);
        self.snapshot
    }
}

fn owned<T>(owner: i32, records: Vec<T>) -> impl Iterator<Item = Owned<T>> {
    records
        .into_iter()
        .map(move |record| Owned { owner, record })
}

/// The warning for a `StockInstruments` file with split lines that do not
/// parse (05 §3.6.1): how many lines in how many records, never a line.
fn split_warning(path: &str, instruments: &[StockInstrument]) -> Option<String> {
    let affected = instruments
        .iter()
        .filter(|instrument| instrument.malformed_split_lines > 0)
        .count();
    let lines = instruments.iter().fold(0u64, |lines, instrument| {
        lines.saturating_add(u64::from(instrument.malformed_split_lines))
    });
    (affected > 0).then(|| {
        let plural = if lines == 1 { "" } else { "s" };
        format!(
            "{}: {lines} split line{plural} in {} cannot be read; the other splits and all other \
             fields were kept",
            shown(path),
            records(affected, StockInstrument::NAME)
        )
    })
}

/// `1 batch master`, `2 batch masters`.
fn records_named(count: usize, noun: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {noun}{plural}")
}

fn unknown_inner_warning(place: Place, name: &str, is_dir: bool, seen: &Seen) -> String {
    let what = if is_dir { "directory" } else { "entry" };
    format!(
        "unknown {what} \"{}\" in {} (first: {}): skipped without reading it",
        shown(name),
        directories(seen.count, place),
        seen.first
    )
}

fn orphan_warning(place: Place, seen: &Seen) -> String {
    let (owner, column) = match place {
        Place::Account => ("an account", "account_id"),
        Place::Stock => ("a stock instrument", "instrument_id"),
    };
    format!(
        "{} (first: {}) without {owner}: the records there were decoded and keep the \
         directory's id as {column}",
        directories(seen.count, place),
        seen.first
    )
}

/// The one line per record type about bytes a newer program version appended.
fn unread_warning(record_type: &str, tally: &TypeTally) -> Option<String> {
    if tally.unread_records == 0 {
        return None;
    }
    const SHOWN: usize = 3;
    let mut shapes: Vec<String> = tally
        .unread_histogram
        .iter()
        .take(SHOWN)
        .map(|(unread, count)| {
            let plural = if unread.bytes == 1 { "" } else { "s" };
            format!(
                "{} byte{plural} at the end of chunk {} in {count}",
                unread.bytes, unread.chunk
            )
        })
        .collect();
    if tally.unread_histogram.len() > SHOWN {
        shapes.push("...".to_owned());
    }
    let version = if tally.newer_version > 0 {
        format!(
            "; their record version is {}, b4 knows up to {DOCUMENTED_RECORD_VERSION}",
            tally.newer_version
        )
    } else {
        String::new()
    };
    Some(format!(
        "{} of {} in {} carry bytes from a newer Banking4 version that b4 does not read ({}){version}",
        tally.unread_records,
        records(tally.decoded, record_type),
        files(tally.files_with_unread),
        shapes.join(", ")
    ))
}

/// The one line per record type about records in the old single-chunk layout.
/// They are complete and fully decoded, so this is a note, not a warning.
fn legacy_note(record_type: &str, tally: &TypeTally) -> Option<String> {
    (tally.legacy_records > 0).then(|| {
        format!(
            "{} of {} in {} use the layout of an old Banking4 version; they were decoded in full",
            tally.legacy_records,
            records(tally.decoded, record_type),
            files(tally.files_with_legacy)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::base::{Tally, Undecodable};
    use crate::wire::WireError;

    const PASSWORD: &str = "demo";

    fn demo_bytes() -> Vec<u8> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault/demo.sub");
        std::fs::read(path).unwrap()
    }

    fn unlocked(bytes: Vec<u8>) -> Vault {
        let mut vault = Vault::from_bytes(bytes).unwrap();
        vault.unlock(PASSWORD).unwrap();
        vault
    }

    /// The offset of the 64-byte directory slot of the entry called `name`:
    /// `first_cluster` (4 bytes), size (4), flags (1), name length (1), name
    /// (54, zero-filled), on a 64-byte boundary (01 §7.1).
    fn slot_of(image: &[u8], name: &str) -> usize {
        let found: Vec<usize> = (0..image.len() / 64)
            .map(|slot| slot * 64)
            .filter(|&at| {
                let slot = &image[at..at + 64];
                slot[..4] != [0; 4]
                    && usize::from(slot[9]) == name.len()
                    && &slot[10..10 + name.len()] == name.as_bytes()
                    && slot[10 + name.len()..].iter().all(|byte| *byte == 0)
            })
            .collect();
        assert_eq!(found.len(), 1, "one slot named {name}");
        found[0]
    }

    /// Make every attempt to read or list the entry fail: its chain now
    /// starts at a cluster far outside the file.
    fn break_first_cluster(image: &mut [u8], name: &str) {
        let at = slot_of(image, name);
        image[at..at + 4].copy_from_slice(&0x00F0_0000_i32.to_le_bytes());
    }

    #[test]
    fn the_nine_names_of_bank_access_data_are_never_read() {
        for name in NEVER_READ {
            for is_dir in [false, true] {
                for has_accts_new in [false, true] {
                    assert_eq!(
                        classify_root(name, is_dir, has_accts_new),
                        RootClass::NeverRead,
                        "{name}"
                    );
                }
                // Not below the root either: nothing there is read by a name
                // of this list.
                assert_eq!(
                    classify_in_account(name, is_dir),
                    InnerClass::Unknown,
                    "{name}"
                );
                assert_eq!(
                    classify_in_stock(name, is_dir),
                    InnerClass::Unknown,
                    "{name}"
                );
            }
        }
        assert_eq!(NEVER_READ.len(), 9);
    }

    #[test]
    fn every_decoded_root_name_has_its_class() {
        let files = [
            ("AcctsNew", RootFile::Accounts),
            ("AcctGroups", RootFile::AccountGroups),
            ("Payees", RootFile::Payees),
            ("Payers", RootFile::Payers),
            ("Categories", RootFile::Categories),
            ("Mandates", RootFile::Mandates),
            ("ExchangeRatesBIN", RootFile::ExchangeRates),
            ("StockInstruments", RootFile::StockInstruments),
        ];
        for (name, file) in files {
            for has_accts_new in [false, true] {
                assert_eq!(
                    classify_root(name, false, has_accts_new),
                    RootClass::Collection(file),
                    "{name}"
                );
                // A directory of that name is not the collection.
                assert_eq!(
                    classify_root(name, true, has_accts_new),
                    RootClass::Unknown,
                    "{name}"
                );
            }
        }
        assert_eq!(
            classify_root("Acct757073100", true, true),
            RootClass::AccountDirectory(757_073_100)
        );
        assert_eq!(
            classify_root("Acct-5", true, true),
            RootClass::AccountDirectory(-5)
        );
        assert_eq!(
            classify_root("Stock779824800", true, true),
            RootClass::StockDirectory(779_824_800)
        );
        // Files with the names of directories are not read.
        assert_eq!(
            classify_root("Acct757073100", false, true),
            RootClass::Unknown
        );
        assert_eq!(
            classify_root("Stock779824800", false, true),
            RootClass::Unknown
        );
    }

    #[test]
    fn the_legacy_account_file_counts_only_without_the_new_one() {
        assert_eq!(classify_root("Accts", false, true), RootClass::NotExported);
        assert_eq!(
            classify_root("Accts", false, false),
            RootClass::Collection(RootFile::Accounts)
        );
        assert_eq!(classify_root("Accts", true, false), RootClass::Unknown);
    }

    #[test]
    fn known_names_that_are_not_exported_are_skipped_silently() {
        for name in NOT_EXPORTED {
            for is_dir in [false, true] {
                assert_eq!(
                    classify_root(name, is_dir, true),
                    RootClass::NotExported,
                    "{name}"
                );
            }
        }
        for name in NOT_EXPORTED_IN_ACCOUNT {
            assert_eq!(
                classify_in_account(name, false),
                InnerClass::NotExported,
                "{name}"
            );
        }
        for name in ["800391600.txt", "800391600.pdf", "800391600.bin", "-12.pdf"] {
            assert_eq!(
                classify_in_account(name, false),
                InnerClass::NotExported,
                "{name}"
            );
        }
    }

    #[test]
    fn every_decoded_account_file_has_its_class() {
        let files = [
            ("StmtMasterEntries", AccountFile::LegacyBookings),
            ("Stmt", AccountFile::Bookings),
            ("Paymts", AccountFile::Payments),
            ("Stos", AccountFile::StandingOrders),
            ("Portf", AccountFile::PortfolioItems),
            ("PortfTxns", AccountFile::PortfolioTransactions),
            ("AcctTaxExemptions", AccountFile::TaxExemptions),
            ("Assets", AccountFile::Assets),
        ];
        for (name, file) in files {
            assert_eq!(
                classify_in_account(name, false),
                InnerClass::Collection(file),
                "{name}"
            );
            assert_eq!(
                classify_in_account(name, true),
                InnerClass::Unknown,
                "{name}"
            );
            // The names mean nothing in the root or in a stock directory.
            assert_eq!(
                classify_root(name, false, true),
                RootClass::Unknown,
                "{name}"
            );
            assert_eq!(
                classify_in_stock(name, false),
                InnerClass::Unknown,
                "{name}"
            );
        }
        assert_eq!(
            classify_in_stock("StockPrices", false),
            InnerClass::Collection(())
        );
        assert_eq!(classify_in_stock("StockPrices", true), InnerClass::Unknown);
        assert_eq!(
            classify_in_account("StockPrices", false),
            InnerClass::Unknown
        );
        // The legacy booking file is read before `Stmt`.
        assert!(AccountFile::LegacyBookings < AccountFile::Bookings);
    }

    #[test]
    fn everything_else_is_unknown() {
        for name in [
            "FutureCollection",
            "",
            "acctsnew",
            "AcctsNew ",
            "Payees2",
            "contacts",
            "Acct",
            "Acct007",
            "Acct+7",
            "Acct12x",
            "Stock",
            "ModelSettings.bak",
        ] {
            for is_dir in [false, true] {
                assert_eq!(
                    classify_root(name, is_dir, true),
                    RootClass::Unknown,
                    "{name:?}"
                );
            }
        }
        for name in [
            "FutureThing",
            "stmt",
            "Stmt2",
            "x.txt",
            "12.doc",
            ".pdf",
            "-.pdf",
            "12.PDF",
        ] {
            assert_eq!(
                classify_in_account(name, false),
                InnerClass::Unknown,
                "{name:?}"
            );
        }
        assert_eq!(
            classify_in_account("800391600.pdf", true),
            InnerClass::Unknown
        );
    }

    #[test]
    fn the_demo_vault_loads_with_the_counts_of_its_manifest() {
        let snapshot = load(&unlocked(demo_bytes())).unwrap();
        assert_eq!(snapshot.accounts.len(), 6);
        assert_eq!(snapshot.account_groups.len(), 2);
        assert_eq!(snapshot.bookings.len(), 634);
        assert_eq!(snapshot.payments.len(), 2);
        assert_eq!(snapshot.standing_orders.len(), 3);
        assert_eq!(snapshot.payees.len(), 30);
        assert_eq!(snapshot.payers.len(), 0);
        assert_eq!(snapshot.categories.len(), 30);
        assert_eq!(snapshot.mandates.len(), 1);
        assert_eq!(snapshot.exchange_rates.len(), 10);
        assert_eq!(snapshot.stock_instruments.len(), 4);
        assert_eq!(snapshot.stock_prices.len(), 240);
        assert_eq!(snapshot.portfolio_items.len(), 3);
        assert_eq!(snapshot.portfolio_transactions.len(), 2);
        assert_eq!(snapshot.tax_exemptions.len(), 1);
        assert_eq!(snapshot.assets.len(), 0);
        assert_eq!(snapshot.entries_never_read, 9);
        assert!(snapshot.notes.is_empty(), "{:?}", snapshot.notes);
    }

    #[test]
    fn each_unknown_entry_of_the_demo_vault_gives_exactly_one_warning() {
        let snapshot = load(&unlocked(demo_bytes())).unwrap();
        assert_eq!(
            snapshot.warnings,
            [
                "unknown entry \"FutureCollection\" in the vault (300 bytes): skipped without \
                 reading it",
                "unknown entry \"FutureThing\" in 1 account directory (first: Acct757073100): \
                 skipped without reading it",
            ]
        );
    }

    #[test]
    fn a_bank_access_entry_that_cannot_be_read_does_not_disturb_loading() {
        let intact = load(&unlocked(demo_bytes())).unwrap();
        for name in NEVER_READ {
            let mut image = demo_bytes();
            break_first_cluster(&mut image, name);
            let vault = unlocked(image);

            // The patch works: whoever touches the entry fails.
            let entry = vault
                .root()
                .iter()
                .find(|entry| entry.name() == name)
                .unwrap_or_else(|| panic!("{name} is in the demo vault"));
            let touched = if entry.is_dir() {
                vault.read_dir(entry).map(|_| ())
            } else {
                vault.read_file(entry).map(|_| ())
            };
            assert!(
                touched.is_err(),
                "{name} must be unreadable after the patch"
            );

            // And loading does not touch it.
            let snapshot = load(&unlocked({
                let mut image = demo_bytes();
                break_first_cluster(&mut image, name);
                image
            }))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(snapshot.entries_never_read, 9, "{name}");
            assert_eq!(snapshot, intact, "{name}");
        }
    }

    #[test]
    fn unknown_and_unexported_entries_are_not_read_either() {
        let intact = load(&unlocked(demo_bytes())).unwrap();
        // `Accts` is class 3 here because `AcctsNew` exists.
        for name in ["FutureCollection", "FutureThing", "Accts"] {
            let mut image = demo_bytes();
            break_first_cluster(&mut image, name);
            let snapshot = load(&unlocked(image)).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(snapshot, intact, "{name}");
        }
    }

    #[test]
    fn a_decoded_entry_that_cannot_be_read_fails_with_its_name() {
        let mut image = demo_bytes();
        break_first_cluster(&mut image, "Payees");
        let error = load(&unlocked(image)).unwrap_err();
        assert_eq!(error.to_string(), "cannot read the vault entry Payees");
        let LoadError::Entry { source, .. } = error else {
            panic!("{error:?}");
        };
        assert!(
            matches!(source, VaultError::CorruptChain { .. }),
            "{source}"
        );
    }

    #[test]
    fn the_bookings_of_the_demo_vault_are_regrouped_and_carry_the_account_currency() {
        let snapshot = load(&unlocked(demo_bytes())).unwrap();
        let masters: Vec<_> = snapshot
            .bookings
            .iter()
            .filter(|row| !row.record.is_counted())
            .collect();
        assert_eq!(masters.len(), 2);
        assert!(masters.iter().all(|row| row.account_id == 757_073_100));
        let parts = snapshot
            .bookings
            .iter()
            .filter(|row| row.record.batch_role() == crate::model::payments::BatchRole::Slave)
            .count();
        assert_eq!(parts, 6);
        assert!(snapshot
            .bookings
            .iter()
            .all(|row| row.currency.as_deref() == Some("EUR")));
        // 04 §6.2 step 4: bookings of the cash account have no value date.
        assert!(snapshot
            .bookings
            .iter()
            .filter(|row| row.account_id == 757_073_340)
            .all(|row| row.record.value_date.is_none()));
    }

    fn tally(records: usize) -> Tally {
        Tally {
            records,
            first: Some(0),
        }
    }

    /// A loader over the demo vault that has read nothing yet.
    fn sentences(reports: &[(&str, LoadReport)]) -> (Vec<String>, Vec<String>) {
        let vault = Vault::from_bytes(demo_bytes()).unwrap();
        let mut loader = Loader::new(&vault, Date::MAX);
        for (path, report) in reports {
            loader.account_for(path, report);
        }
        let snapshot = loader.finish();
        (snapshot.warnings, snapshot.notes)
    }

    #[test]
    fn unread_bytes_become_one_warning_per_record_type() {
        let report = |decoded, bytes| LoadReport {
            record_type: "payee",
            decoded,
            unread_bytes: tally(decoded),
            unread_histogram: BTreeMap::from([(UnreadBytes { chunk: 1, bytes }, decoded)]),
            ..LoadReport::default()
        };
        let clean = LoadReport {
            record_type: "payee",
            decoded: 5,
            ..LoadReport::default()
        };
        let (warnings, notes) = sentences(&[
            ("Payees", report(455, 2)),
            ("Other/Payees", report(1, 2)),
            ("Third/Payees", clean),
        ]);
        assert_eq!(
            warnings,
            [
                "456 of 461 payee records in 2 files carry bytes from a newer Banking4 version \
              that b4 does not read (2 bytes at the end of chunk 1 in 456)"
            ]
        );
        assert!(notes.is_empty());
    }

    #[test]
    fn the_account_warning_names_the_record_version() {
        let vault = Vault::from_bytes(demo_bytes()).unwrap();
        let mut loader = Loader::new(&vault, Date::MAX);
        loader.account_for(
            "AcctsNew",
            &LoadReport {
                record_type: "account",
                decoded: 21,
                unread_bytes: tally(21),
                unread_histogram: BTreeMap::from([(
                    UnreadBytes {
                        chunk: 1,
                        bytes: 10,
                    },
                    21,
                )]),
                ..LoadReport::default()
            },
        );
        loader.tallies.entry("account").or_default().newer_version = 27;
        assert_eq!(
            loader.finish().warnings,
            [
                "21 of 21 account records in 1 file carry bytes from a newer Banking4 version \
              that b4 does not read (10 bytes at the end of chunk 1 in 21); their record \
              version is 27, b4 knows up to 26"
            ]
        );
    }

    #[test]
    fn legacy_records_become_one_note_per_record_type_and_no_warning() {
        let report = |decoded, legacy| LoadReport {
            record_type: "booking",
            decoded,
            legacy: tally(legacy),
            ..LoadReport::default()
        };
        let plain = LoadReport {
            record_type: "booking",
            decoded: 100,
            ..LoadReport::default()
        };
        let (warnings, notes) = sentences(&[
            ("Acct1/Stmt", report(837, 837)),
            ("Acct2/Stmt", plain),
            ("Acct3/Stmt", report(1, 1)),
        ]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            notes,
            [
                "838 of 938 booking records in 2 files use the layout of an old Banking4 version; \
              they were decoded in full"
            ]
        );
    }

    #[test]
    fn damage_in_a_file_is_a_warning_that_names_the_file() {
        let undecodable = Undecodable {
            index: 4,
            offset: 300,
            error: WireError::Value {
                field: "amount",
                offset: 12,
                problem: crate::wire::ValueProblem::InvalidDecimal,
            }
            .into(),
        };
        let report = LoadReport {
            record_type: "booking",
            decoded: 7,
            undecodable: 2,
            undecodable_samples: vec![undecodable],
            extra_chunks: tally(3),
            extra_chunk_count: 4,
            overlong_strings: tally(1),
            duplicate_oids: tally(2),
            stopped: Some(crate::model::base::Stop::Record(undecodable)),
            ..LoadReport::default()
        };
        let (warnings, _) = sentences(&[("Acct5/Stmt", report)]);
        assert_eq!(warnings.len(), 5, "{warnings:?}");
        for warning in &warnings {
            assert!(warning.starts_with("Acct5/Stmt: "), "{warning}");
        }
        assert!(
            warnings[0].contains("reading stopped at"),
            "{}",
            warnings[0]
        );
        assert!(warnings[0].ends_with("; kept the 7 booking records before it"));
        assert!(
            warnings[1].starts_with(
                "Acct5/Stmt: skipped 2 booking records that cannot be decoded (first: "
            ),
            "{}",
            warnings[1]
        );
        assert!(warnings[1].contains("amount"), "{}", warnings[1]);
        assert_eq!(
            warnings[2],
            "Acct5/Stmt: 3 of 7 booking records have chunks from a newer Banking4 version that \
             b4 does not read (4 skipped)"
        );
        assert_eq!(
            warnings[3],
            "Acct5/Stmt: a text runs past its chunk in 1 booking record; the fields after it \
             were read as empty"
        );
        assert_eq!(
            warnings[4],
            "Acct5/Stmt: repeated object id in 2 booking records; all were kept"
        );
    }

    #[test]
    fn directory_findings_are_counted_not_listed() {
        let mut seen = Seen::default();
        seen.add("Acct1");
        seen.add("Acct2");
        assert_eq!(
            unknown_inner_warning(Place::Account, "FutureThing", false, &seen),
            "unknown entry \"FutureThing\" in 2 account directories (first: Acct1): skipped \
             without reading it"
        );
        assert_eq!(
            unknown_inner_warning(Place::Stock, "Deep", true, &seen),
            "unknown directory \"Deep\" in 2 stock directories (first: Acct1): skipped without \
             reading it"
        );
        assert_eq!(
            orphan_warning(Place::Account, &seen),
            "2 account directories (first: Acct1) without an account: the records there were \
             decoded and keep the directory's id as account_id"
        );
        let mut one = Seen::default();
        one.add("Stock9");
        assert_eq!(
            orphan_warning(Place::Stock, &one),
            "1 stock directory (first: Stock9) without a stock instrument: the records there \
             were decoded and keep the directory's id as instrument_id"
        );
    }

    #[test]
    fn an_orphan_directory_is_decoded_with_a_warning() {
        // Rename the directory of the cash account: same length, another id.
        let mut image = demo_bytes();
        let at = slot_of(&image, "Acct757073340");
        image[at + 10..at + 10 + 13].copy_from_slice(b"Acct757073341");
        let snapshot = load(&unlocked(image)).unwrap();
        assert_eq!(snapshot.bookings.len(), 634);
        let moved: Vec<_> = snapshot
            .bookings
            .iter()
            .filter(|row| row.account_id == 757_073_341)
            .collect();
        assert_eq!(moved.len(), 55);
        // No account: the stored currency stands in, and no date is cleared.
        assert!(moved
            .iter()
            .all(|row| row.currency.as_deref() == Some("EUR")));
        assert!(snapshot.warnings.contains(
            &"1 account directory (first: Acct757073341) without an account: the records there \
              were decoded and keep the directory's id as account_id"
                .to_owned()
        ));
        assert_eq!(snapshot.warnings.len(), 3, "{:?}", snapshot.warnings);
    }

    #[test]
    fn a_vault_without_the_new_account_file_reads_the_legacy_one() {
        // Rename `AcctsNew` out of the way: `Accts` (one placeholder record)
        // is then the account file, and every account directory an orphan.
        let mut image = demo_bytes();
        let at = slot_of(&image, "AcctsNew");
        image[at + 10..at + 18].copy_from_slice(b"AcctsOld");
        let snapshot = load(&unlocked(image)).unwrap();
        assert!(snapshot.accounts.is_empty());
        assert_eq!(snapshot.bookings.len(), 634);
        assert!(snapshot
            .warnings
            .iter()
            .any(|warning| warning.starts_with("unknown entry \"AcctsOld\" in the vault")));
        assert!(snapshot
            .warnings
            .iter()
            .any(|warning| warning.starts_with("6 account directories (first: Acct757073100)")));
    }

    #[test]
    fn names_in_messages_are_escaped_and_bounded() {
        assert_eq!(shown("Stmt"), "Stmt");
        assert_eq!(shown("a\nb\u{1b}[31m"), "a\\nb\\u{1b}[31m");
        assert_eq!(shown(&"x".repeat(200)).len(), 83);
    }

    #[test]
    fn warnings_of_the_demo_vault_hold_no_record_values() {
        let snapshot = load(&unlocked(demo_bytes())).unwrap();
        for warning in snapshot.warnings.iter().chain(&snapshot.notes) {
            for value in ["Mustermann", "DE31", "Girokonto", "B4-DECOY"] {
                assert!(!warning.contains(value), "{warning}");
            }
        }
    }

    // --- The fence --------------------------------------------------------------

    const CLUSTER: usize = 512;

    fn le32(image: &[u8], at: usize) -> i32 {
        i32::from_le_bytes(image[at..at + 4].try_into().unwrap())
    }

    /// Offset of the allocation table entry of `cluster` (01 §6.2): the
    /// table has one page per 128 clusters, and page `k` is cluster `128 k`.
    fn table_entry(cluster: usize) -> usize {
        let per_page = CLUSTER / 4;
        (cluster / per_page * per_page + 1) * CLUSTER + cluster % per_page * 4
    }

    /// The chain that starts at `first`, read from the allocation table of
    /// the image by hand, without the reader under test.
    fn chain_of(image: &[u8], first: i32) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut next = first;
        while next > 0 {
            let cluster = next as usize;
            assert!(chain.len() < image.len() / CLUSTER, "the chain loops");
            chain.push(cluster as u32);
            let at = table_entry(cluster);
            next = match image[at] {
                0x01 => i32::from_le_bytes([image[at + 1], image[at + 2], image[at + 3], 0]),
                0xFF => -1,
                status => panic!("cluster {cluster} has the table status {status:#x}"),
            };
        }
        chain
    }

    /// The live slots of the directory whose chain starts at `first`: name,
    /// first cluster, whether it is a directory.
    fn slots_of(image: &[u8], first: i32) -> Vec<(String, i32, bool)> {
        let mut slots = Vec::new();
        for page in chain_of(image, first) {
            let start = (page as usize + 1) * CLUSTER;
            for at in (start..start + CLUSTER).step_by(64) {
                if le32(image, at) == 0 {
                    continue;
                }
                let name = &image[at + 10..at + 10 + usize::from(image[at + 9])];
                slots.push((
                    String::from_utf8(name.to_vec()).unwrap(),
                    le32(image, at),
                    image[at + 8] & 2 != 0,
                ));
            }
        }
        slots
    }

    /// Where an entry that is not read keeps its bytes.
    #[derive(Debug, Default, PartialEq)]
    struct Storage {
        /// Clusters of file data, of the entry and of everything below it.
        data: BTreeSet<u32>,
        /// Clusters of directory pages, likewise.
        pages: BTreeSet<u32>,
    }

    impl Storage {
        fn add(&mut self, image: &[u8], first: i32, is_dir: bool) {
            let chain = chain_of(image, first);
            if is_dir {
                self.pages.extend(&chain);
                for (_, first, is_dir) in slots_of(image, first) {
                    self.add(image, first, is_dir);
                }
            } else {
                self.data.extend(&chain);
            }
        }
    }

    /// The storage of the root entries called `names`, worked out from the
    /// directory slots and the allocation table of the image alone. Every
    /// name must be there.
    fn storage_of(image: &[u8], names: &[&str]) -> Storage {
        let root = slots_of(image, le32(image, 0x0C));
        let mut storage = Storage::default();
        for name in names {
            let (_, first, is_dir) = root
                .iter()
                .find(|(slot, _, _)| slot == name)
                .unwrap_or_else(|| panic!("{name} is in the root directory"));
            storage.add(image, *first, *is_dir);
        }
        storage
    }

    /// Load `image` and return the result with what the vault did with its
    /// clusters.
    fn load_touching(image: Vec<u8>) -> (Result<Snapshot, LoadError>, crate::vault::Touched) {
        let vault = unlocked(image);
        let result = load(&vault);
        (result, vault.touched())
    }

    /// Assert that loading stopped because `entry` leads into `cluster`,
    /// which belongs to the reserved entry `holder`.
    fn assert_fenced(
        result: &Result<Snapshot, LoadError>,
        entry: &str,
        holder: &str,
        cluster: u32,
    ) {
        match result {
            Err(LoadError::Fenced {
                entry: path,
                source:
                    VaultError::CrossLinked {
                        cluster: at,
                        first,
                        second,
                    },
            }) => {
                assert_eq!(path, entry);
                assert_eq!(*at, cluster);
                assert_eq!(first, &format!("entry {holder:?}"));
                let name = entry.rsplit('/').next().unwrap();
                assert_eq!(second, &format!("entry {name:?}"));
            }
            other => panic!("{entry}: {other:?}"),
        }
    }

    /// Assert that nothing in `storage` was copied out or decrypted.
    fn assert_untouched(touched: &crate::vault::Touched, storage: &Storage) {
        for cluster in storage.data.iter().chain(&storage.pages) {
            assert!(
                !touched.data.contains_key(cluster),
                "cluster {cluster} was read as data"
            );
            assert!(
                !touched.decrypted.contains(cluster),
                "cluster {cluster} was decrypted"
            );
        }
        // File data is not taken for directory pages either.
        for cluster in &storage.data {
            assert!(
                !touched.pages.contains(cluster),
                "cluster {cluster} was decoded as a directory page"
            );
        }
    }

    /// Everything the demo vault has that b4 does not read: the nine entries
    /// of class 2, and those of class 3 and 4.
    fn unread_storage(image: &[u8]) -> Storage {
        let mut storage = storage_of(image, &NEVER_READ);
        let others = storage_of(image, &["Accts", "FutureCollection"]);
        storage.data.extend(others.data);
        // `FutureThing` in the directory of the first account.
        let account = slots_of(image, le32(image, 0x0C))
            .into_iter()
            .find(|(name, _, _)| name == "Acct757073100")
            .unwrap();
        let (_, first, _) = slots_of(image, account.1)
            .into_iter()
            .find(|(name, _, _)| name == "FutureThing")
            .unwrap();
        storage.data.extend(chain_of(image, first));
        storage
    }

    #[test]
    fn a_normal_load_reads_no_data_from_the_storage_of_the_nine_never_read_entries() {
        let image = demo_bytes();
        let never_read = storage_of(&image, &NEVER_READ);
        // Seven files, two of them of two clusters, and two directories of
        // one page with one file each.
        assert_eq!(
            never_read,
            Storage {
                data: BTreeSet::from([528, 529, 530, 531, 532, 533, 534, 535, 537, 539, 541]),
                pages: BTreeSet::from([538, 540]),
            }
        );
        let unread = unread_storage(&image);
        assert_eq!(unread.data.len(), never_read.data.len() + 3);

        let (result, touched) = load_touching(image);
        result.unwrap();
        assert_untouched(&touched, &never_read);
        assert_untouched(&touched, &unread);
        // The two directories were walked as structure, which is how the
        // files in them were found and fenced.
        assert!(never_read.pages.is_subset(&touched.pages));

        // The instrument does see what is read: every file b4 decodes went
        // through the cipher, starting with `AcctGroups` in cluster 2.
        assert_eq!(touched.data.get(&2), Some(&128));
        assert!(touched.decrypted.contains(&2));
        assert_eq!(touched.data.len(), touched.decrypted.len());
        assert!(touched.data.len() > 400, "{}", touched.data.len());
    }

    #[test]
    fn fencing_reads_no_data_and_costs_less_than_one_step_per_cluster() {
        let image = demo_bytes();
        let unread = unread_storage(&image);
        let vault = unlocked(image);
        let loader = Loader::new(&vault, Date::MAX);
        let root = vault.root();
        let classes: Vec<RootClass> = root
            .iter()
            .map(|entry| classify_root(entry.name(), entry.is_dir(), true))
            .collect();
        let before = vault.chain_steps();
        let listings = loader.fence(root, &classes).unwrap();
        let steps = vault.chain_steps() - before;

        assert_eq!(listings.iter().flatten().count(), 10);
        let touched = vault.touched();
        assert!(touched.data.is_empty() && touched.decrypted.is_empty());
        for cluster in unread.data.iter().chain(&unread.pages) {
            assert!(vault.is_reserved(*cluster), "cluster {cluster}");
        }
        // One step per reserved cluster and per page of a listed directory.
        assert_eq!(steps, (unread.data.len() + unread.pages.len() + 10) as u64);
        assert!(steps < u64::from(vault.cluster_count()));

        // What was fenced stays unread for good.
        let contacts = root
            .iter()
            .find(|entry| entry.name() == "Contacts")
            .unwrap();
        assert!(matches!(
            vault.read_file(contacts),
            Err(VaultError::Reserved)
        ));
    }

    /// The patch of the review: the slot of `AcctGroups`, the first file the
    /// loader reads, gets the first cluster, the size and the flags of
    /// `Contacts` and keeps its name.
    fn reviewers_patch(image: &mut [u8]) {
        let (allowed, forbidden) = (slot_of(image, "AcctGroups"), slot_of(image, "Contacts"));
        assert_eq!((allowed, forbidden), (1024, 264_320));
        image.copy_within(forbidden..forbidden + 9, allowed);
    }

    #[test]
    fn an_allowed_name_on_the_clusters_of_a_never_read_entry_is_refused_unread() {
        let mut image = demo_bytes();
        let unread = unread_storage(&image);
        reviewers_patch(&mut image);

        // The patch does what the review says: read by name alone, the
        // entry delivers the content of `Contacts`.
        let vault = unlocked(image.clone());
        let entry = vault
            .root()
            .iter()
            .find(|e| e.name() == "AcctGroups")
            .unwrap();
        let content = vault.read_file(entry).unwrap();
        assert_eq!(content.len(), 400);
        assert!(content.starts_with(b"B4-DECOY-MUST-NOT-BE-READ"));

        // The loader refuses, and has copied and decrypted nothing at all by
        // then: `AcctGroups` is the first file it reads.
        let (result, touched) = load_touching(image);
        assert_fenced(&result, "AcctGroups", "Contacts", 528);
        assert_untouched(&touched, &unread);
        assert!(touched.data.is_empty(), "{:?}", touched.data);
        assert!(touched.decrypted.is_empty());
        assert_eq!(
            result.unwrap_err().to_string(),
            "cannot read the vault entry AcctGroups"
        );
    }

    #[test]
    fn a_never_read_entry_cannot_hide_behind_a_size_of_zero() {
        // The size of the entry that is not read is forged too: its chain is
        // reserved all the same.
        let mut image = demo_bytes();
        let unread = unread_storage(&image);
        reviewers_patch(&mut image);
        let at = slot_of(&image, "Contacts");
        image[at + 4..at + 8].copy_from_slice(&0i32.to_le_bytes());
        let (result, touched) = load_touching(image);
        assert_fenced(&result, "AcctGroups", "Contacts", 528);
        assert_untouched(&touched, &unread);
    }

    #[test]
    fn a_file_in_an_account_directory_on_never_read_clusters_is_refused_unread() {
        // Mirror case (a): `Stmt` of the first account points at `Contacts`.
        let mut image = demo_bytes();
        let unread = unread_storage(&image);
        let account = slots_of(&image, le32(&image, 0x0C))
            .into_iter()
            .find(|(name, _, _)| name == "Acct757073100")
            .unwrap();
        let page = (chain_of(&image, account.1)[0] as usize + 1) * CLUSTER;
        assert_eq!(&image[page + 10..page + 14], b"Stmt");
        let contacts = slot_of(&image, "Contacts");
        image.copy_within(contacts..contacts + 9, page);

        let (result, touched) = load_touching(image);
        assert_fenced(&result, "Acct757073100/Stmt", "Contacts", 528);
        assert_untouched(&touched, &unread);
        // The root collections were read before, as they should be.
        assert!(touched.data.contains_key(&2));
    }

    #[test]
    fn a_directory_of_class_1_on_the_pages_of_a_never_read_directory_is_not_listed() {
        // Mirror case (b): an account directory, or a stock directory, whose
        // first cluster is the page of `SpoolerContacts` or `OutboxData`.
        // Listed under the allowed name, the files in it would be read.
        for (allowed, forbidden) in [
            ("Acct757073160", "SpoolerContacts"),
            ("Stock779824800", "OutboxData"),
            ("Acct757073160", "OutboxData"),
        ] {
            let mut image = demo_bytes();
            let unread = unread_storage(&image);
            let (to, from) = (slot_of(&image, allowed), slot_of(&image, forbidden));
            let page = le32(&image, from);
            image.copy_within(from..from + 4, to);

            // Without the fence the forbidden directory lists under the
            // allowed name.
            let vault = unlocked(image.clone());
            let entry = vault.root().iter().find(|e| e.name() == allowed).unwrap();
            assert_eq!(vault.read_dir(entry).unwrap().len(), 1, "{allowed}");

            let (result, touched) = load_touching(image);
            assert_fenced(&result, allowed, forbidden, page as u32);
            assert_untouched(&touched, &unread);
            // Refused while fencing: not one file was read.
            assert!(touched.data.is_empty(), "{allowed}");
        }
    }

    /// Link `from` to `to` in the allocation table.
    fn link(image: &mut [u8], from: usize, to: u32) {
        let at = table_entry(from);
        assert_eq!(image[at], 0xFF, "cluster {from} ends a chain");
        let [a, b, c, _] = to.to_le_bytes();
        image[at..at + 4].copy_from_slice(&[0x01, a, b, c]);
    }

    fn set_size(image: &mut [u8], name: &str, size: i32) {
        let at = slot_of(image, name);
        image[at + 4..at + 8].copy_from_slice(&size.to_le_bytes());
    }

    #[test]
    fn a_chain_that_links_into_never_read_storage_is_refused_before_its_first_cluster() {
        // Mirror case (c): `AcctGroups` starts in its own cluster 2, and the
        // allocation table leads on from there into `Contacts`.
        let intact = load(&unlocked(demo_bytes())).unwrap();
        let mut image = demo_bytes();
        let unread = unread_storage(&image);
        link(&mut image, 2, 528);

        // As long as the size does not reach across the link, the entry
        // reads as before and nothing of `Contacts` is touched (01 §8.1).
        let (result, touched) = load_touching(image.clone());
        assert_eq!(result.unwrap(), intact);
        assert_untouched(&touched, &unread);

        // One byte more than its own cluster holds, and it is refused: with
        // nothing copied, its own first cluster included.
        for size in [513, 912, 1024] {
            let mut image = image.clone();
            set_size(&mut image, "AcctGroups", size);
            let (result, touched) = load_touching(image);
            assert_fenced(&result, "AcctGroups", "Contacts", 528);
            assert_untouched(&touched, &unread);
            assert!(touched.data.is_empty(), "size {size}");
        }
    }

    #[test]
    fn a_size_that_runs_on_into_never_read_storage_is_refused() {
        // Mirror case (d): the size of an allowed file is larger than its
        // own clusters hold, and the table chains them on into the middle of
        // a never-read chain: cluster 532 is the second cluster of
        // `OnlineContactKeys`, which no directory slot names.
        let mut image = demo_bytes();
        let unread = unread_storage(&image);
        assert_eq!(chain_of(&image, 531), [531, 532]);
        assert_eq!(chain_of(&image, 28), [28]);
        link(&mut image, 28, 532);
        set_size(&mut image, "Mandates", 512 + 13);
        let (result, touched) = load_touching(image);
        assert_fenced(&result, "Mandates", "OnlineContactKeys", 532);
        assert_untouched(&touched, &unread);
        assert!(!touched.data.contains_key(&28));

        // The same through the last cluster of a longer file of its own:
        // `Payees` ends in cluster 27 and is sent on into `ModelSettings`.
        let mut image = demo_bytes();
        let payees = chain_of(&image, le32(&image, slot_of(&image, "Payees")));
        assert_eq!(payees.len(), 11);
        link(&mut image, payees[10] as usize, 533);
        set_size(&mut image, "Payees", 11 * 512 + 1);
        let (result, touched) = load_touching(image);
        assert_fenced(&result, "Payees", "ModelSettings", 533);
        assert_untouched(&touched, &unread);
        for cluster in payees {
            assert!(!touched.data.contains_key(&cluster), "cluster {cluster}");
        }
    }

    #[test]
    fn entries_that_are_skipped_or_unknown_are_fenced_like_bank_access_data() {
        // Class 3 (`Accts` next to `AcctsNew`) and class 4, in the root and
        // in an account directory.
        for (allowed, holder) in [
            ("AcctGroups", "Accts"),
            ("AcctGroups", "FutureCollection"),
            ("Mandates", "FutureThing"),
        ] {
            let mut image = demo_bytes();
            let unread = unread_storage(&image);
            let (to, from) = (slot_of(&image, allowed), slot_of(&image, holder));
            let first = le32(&image, from);
            image.copy_within(from..from + 9, to);
            let (result, touched) = load_touching(image);
            assert_fenced(&result, allowed, holder, first as u32);
            assert_untouched(&touched, &unread);
        }
    }

    #[test]
    fn a_never_read_entry_that_points_into_a_decoded_one_is_refused_too() {
        // The other way round: nothing says which of two slots a cluster
        // really belongs to, so the vault is refused whichever was forged.
        let mut image = demo_bytes();
        let (to, from) = (slot_of(&image, "Contacts"), slot_of(&image, "Payees"));
        image.copy_within(from..from + 9, to);
        let (result, touched) = load_touching(image);
        assert_fenced(&result, "Payees", "Contacts", 17);
        assert!(!touched.data.contains_key(&17));
    }

    #[test]
    fn two_decoded_entries_on_the_same_clusters_are_still_refused() {
        // Mirror case (e): no reserved storage is involved, so this is the
        // plain cross-link, which a vault in use can show for a moment.
        let mut image = demo_bytes();
        let (to, from) = (slot_of(&image, "Mandates"), slot_of(&image, "AcctGroups"));
        image.copy_within(from..from + 9, to);
        match load(&unlocked(image)) {
            Err(LoadError::Entry {
                entry,
                source:
                    VaultError::CrossLinked {
                        cluster: 2,
                        first,
                        second,
                    },
            }) => {
                assert_eq!(entry, "Mandates");
                assert_eq!(first, "entry \"AcctGroups\"");
                assert_eq!(second, "entry \"Mandates\"");
            }
            other => panic!("{other:?}"),
        }
    }

    // --- Rules that span records ----------------------------------------------

    /// Replace the content of the root file `name` by `content`, stored
    /// unencrypted in clusters added at the end of the image.
    fn replace_plain(image: &mut Vec<u8>, name: &str, content: &[u8]) {
        let first = image.len() / CLUSTER - 1;
        let count = content.len().div_ceil(CLUSTER);
        image.extend_from_slice(content);
        image.resize((first + count + 1) * CLUSTER, 0);
        for cluster in first..first + count {
            // The demo vault's last table page has room for these.
            assert!(cluster % (CLUSTER / 4) != 0 && cluster < 640);
            let at = table_entry(cluster);
            let entry = if cluster + 1 < first + count {
                let [a, b, c, _] = (cluster as u32 + 1).to_le_bytes();
                [0x01, a, b, c]
            } else {
                [0xFF, 0, 0, 0]
            };
            image[at..at + 4].copy_from_slice(&entry);
        }
        let at = slot_of(image, name);
        image[at..at + 4].copy_from_slice(&(first as i32).to_le_bytes());
        image[at + 4..at + 8].copy_from_slice(&(content.len() as i32).to_le_bytes());
        image[at + 8] = 0;
    }

    fn record_fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/tests/fixtures/records/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(path).unwrap()
    }

    #[test]
    fn a_category_that_repeats_a_special_role_is_loaded_as_an_ordinary_one() {
        // The record fixture: record 2 is the reclassification category and
        // record 5 stores that role again, record 3 is the excluded category
        // and record 7 stores that again.
        let mut image = demo_bytes();
        replace_plain(&mut image, "Categories", &record_fixture("category.bin"));
        let snapshot = load(&unlocked(image)).unwrap();
        let roles: Vec<(usize, u8, u8, bool)> = snapshot
            .categories
            .iter()
            .enumerate()
            .filter(|(_, loaded)| loaded.record.special_category != SpecialCategory::None)
            .map(|(index, loaded)| {
                (
                    index,
                    loaded.record.special_category.raw(),
                    loaded.special.raw(),
                    loaded.hidden_by_banking4,
                )
            })
            .collect();
        assert_eq!(
            roles,
            [
                (2, 1, 1, false),
                (3, 2, 2, false),
                (4, 3, 3, false),
                (5, 1, 0, false),
                (6, 4, 4, false),
                (7, 2, 0, false),
            ]
        );
        // A record Banking4 drops has no role, and none stored here.
        let dropped: Vec<&LoadedCategory> = snapshot
            .categories
            .iter()
            .filter(|loaded| loaded.hidden_by_banking4)
            .collect();
        assert_eq!(dropped.len(), 2);
        assert!(dropped
            .iter()
            .all(|loaded| loaded.special == SpecialCategory::None));
    }

    #[test]
    fn a_booking_of_a_paypal_account_shows_the_currency_of_the_account() {
        // 04 §6.2: in a PayPal account the record keeps its stored currency,
        // and Banking4 shows the account's all the same. The first account
        // of the demo vault is made a PayPal account in dollars; its
        // bookings store euros.
        let vault = unlocked(demo_bytes());
        let mut loader = Loader::new(&vault, Date::MAX);
        loader.accounts.insert(
            757_073_100,
            AccountFacts {
                kind: crate::model::payments::ACCOUNT_KIND_PAYPAL,
                currency: Some("USD".to_owned()),
            },
        );
        loader.walk_root().unwrap();
        let snapshot = loader.finish();
        let paypal: Vec<_> = snapshot
            .bookings
            .iter()
            .filter(|row| row.account_id == 757_073_100)
            .collect();
        assert_eq!(paypal.len(), 427);
        for row in &paypal {
            assert_eq!(row.currency.as_deref(), Some("USD"));
            assert_eq!(row.record.base.currency.as_deref(), Some("EUR"));
            // What Banking4 keeps in the record is the stored code.
            assert_eq!(
                row.record
                    .adjusted_currency(crate::model::payments::ACCOUNT_KIND_PAYPAL, Some("USD")),
                Some("EUR")
            );
        }
        // The other accounts are as they were.
        assert!(snapshot
            .bookings
            .iter()
            .filter(|row| row.account_id != 757_073_100)
            .all(|row| row.currency.as_deref() == Some("EUR")));

        // A PayPal account without a currency of its own: the stored code.
        let mut loader = Loader::new(&vault, Date::MAX);
        loader.accounts.insert(
            757_073_100,
            AccountFacts {
                kind: crate::model::payments::ACCOUNT_KIND_PAYPAL,
                currency: None,
            },
        );
        // The vault was read once already; reading the same entries again
        // is fine.
        loader.walk_root().unwrap();
        assert!(loader
            .finish()
            .bookings
            .iter()
            .all(|row| row.currency.as_deref() == Some("EUR")));
    }

    #[test]
    fn repeated_isins_and_split_lines_that_do_not_parse_are_reported_as_counts() {
        // The record fixture has two instruments whose ISIN an earlier one
        // has (05 §3.6) and no malformed split.
        let fixture = record_fixture("stock_instrument.bin");
        let mut image = demo_bytes();
        replace_plain(&mut image, "StockInstruments", &fixture);
        let snapshot = load(&unlocked(image)).unwrap();
        assert_eq!(
            duplicate_isins(&snapshot.stock_instruments),
            [5, 8],
            "as the fixture's own test has it"
        );
        let about_instruments = |snapshot: &Snapshot| -> Vec<String> {
            snapshot
                .warnings
                .iter()
                .filter(|warning| warning.starts_with("StockInstruments: "))
                .cloned()
                .collect()
        };
        assert_eq!(
            about_instruments(&snapshot),
            [
                "StockInstruments: repeated ISIN in 2 stock instrument records; all were kept, \
                 so an ISIN can name more than one instrument"
            ]
        );

        // Two split texts are damaged: a date that is none in a text of one
        // line, a factor that is no number in a text of two lines (05
        // §3.6.1).
        let mut damaged = fixture.clone();
        for (good, bad) in [
            (&b"20190603//2/"[..], &b"2019060x//2/"[..]),
            (&b"20200131/R/4/"[..], &b"20200131/R/x/"[..]),
        ] {
            let at = damaged
                .windows(good.len())
                .position(|window| window == good)
                .unwrap();
            damaged[at..at + bad.len()].copy_from_slice(bad);
        }
        let mut image = demo_bytes();
        replace_plain(&mut image, "StockInstruments", &damaged);
        let snapshot = load(&unlocked(image)).unwrap();
        assert_eq!(snapshot.stock_instruments.len(), 19);
        assert_eq!(
            about_instruments(&snapshot),
            [
                "StockInstruments: 2 split lines in 2 stock instrument records cannot be read; \
                 the other splits and all other fields were kept",
                "StockInstruments: repeated ISIN in 2 stock instrument records; all were kept, \
                 so an ISIN can name more than one instrument",
            ]
        );
        // Counts only: no line, no ISIN.
        for warning in about_instruments(&snapshot) {
            for value in ["2019", "/R/", "DE000", "TEST"] {
                assert!(!warning.contains(value), "{warning}");
            }
        }
        // The line that does parse in the text of two lines is kept.
        let kept: usize = snapshot
            .stock_instruments
            .iter()
            .filter(|instrument| instrument.malformed_split_lines > 0)
            .map(|instrument| instrument.split_list.len())
            .sum();
        assert_eq!(kept, 1);

        // One line in one record.
        let one = [StockInstrument {
            malformed_split_lines: 1,
            ..snapshot.stock_instruments[0].clone()
        }];
        assert_eq!(
            split_warning("StockInstruments", &one).unwrap(),
            "StockInstruments: 1 split line in 1 stock instrument record cannot be read; the \
             other splits and all other fields were kept"
        );
        assert_eq!(split_warning("StockInstruments", &[]), None);
    }

    #[test]
    fn many_stock_directories_are_matched_through_one_index() {
        // The lookup of a directory's instrument is by an index built once.
        let vault = unlocked(demo_bytes());
        let mut loader = Loader::new(&vault, Date::MAX);
        loader.walk_root().unwrap();
        assert_eq!(
            loader.instruments.iter().copied().collect::<Vec<_>>(),
            [779_824_800, 779_824_900, 779_825_000, 779_825_100]
        );
        assert!(loader.orphans.is_empty());
    }
}
