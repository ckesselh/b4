//! `b4 status`: describe the database and whether it is stale.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::cli::{Format, Globals, StatusArgs};
use crate::db::read::Database;
use crate::db::{content_checksum, meta_key, META_TABLE, WARNINGS_TABLE};
use crate::error::Context;
use crate::output::{self, Cell, Column, Record};
use crate::vault::Header;
use crate::Error;

/// Describe the database `globals.db` and say whether its vault has changed
/// since it was decrypted.
///
/// # What is printed
///
/// * `table` (the default), for a person: one `Title: value` line per fact
///   (path and size of the database, the b4 and schema version that wrote
///   it, when it was decrypted, path, id and modification time of the vault,
///   how many entries with bank access data were skipped unread), the number
///   of rows of every table, the number of stored warnings, what was
///   compared with the vault, and the status as the last line.
/// * `json` and `jsonl`, for a script: one object with the same facts under
///   stable keys, `tables` as an object of row counts, `warnings` as the
///   full list of messages, and the status: `state` (one of `up_to_date`,
///   `vault_changed`, `other_vault`, `vault_unreachable`,
///   `metadata_incomplete`), `up_to_date` (boolean), `state_detail` (why the
///   vault is not reachable or what the database does not record, else
///   null) and `checks` (the comparisons that were made, in order, from
///   `vault_id`, `modified_stamp`, `size`, `checksum`).
/// * `csv`: a header line and one row of the scalar facts; `tables`, `rows`
///   and `warnings` are counts, `checks` is the names separated by blanks.
///
/// # The status
///
/// What the database recorded about its vault at decrypt time is compared
/// with the vault at the recorded path now. The comparisons run from the
/// cheapest to the dearest, and the first one that differs decides:
///
/// 1. the header of the vault is read, which needs no password. If it
///    cannot be read: "vault not reachable";
/// 2. the vault id: if it differs, "another vault is at that path now";
/// 3. the modification stamp of the header and the size of the file: if
///    either differs, "vault changed since decrypt";
/// 4. the content: the whole file is read and its checksum
///    ([`content_checksum`]) compared, because the stamp has a resolution of
///    one second and is only rewritten when Banking4 closes the vault. If it
///    differs, "vault changed since decrypt"; otherwise "up to date".
///
/// The file is only read in full for the last step, when everything before
/// it matched. "Up to date" is therefore never said without all four
/// comparisons, and the output names the ones that were made.
///
/// A recorded value that is missing or cannot be read (no vault id, a size
/// that is not a number, no checksum) is never taken as a match. If no
/// comparison that could be made found a difference, the status is then
/// "database metadata incomplete", with the missing keys as its detail: the
/// database was written by another version of b4 or changed by hand, and
/// `b4 decrypt` writes it again.
///
/// # `--check`
///
/// Ends with exit status 1 unless the database is up to date; an
/// unreachable vault and incomplete metadata count as not up to date. That
/// is a result, not a failure, so nothing is added to the output. With
/// `--quiet` as well the description is left out: nothing at all is printed
/// when the database is up to date, and one `warning:` line with the status
/// otherwise.
pub fn run(args: &StatusArgs, globals: &Globals) -> Result<(), Error> {
    let database = Database::open(&globals.db)?;
    let report = Report::gather(&database)?;

    let silent = args.check && globals.quiet;
    if !silent {
        match globals.format() {
            Format::Json => {
                let mut text = serde_json::to_string_pretty(&report)
                    .map_err(|error| Error::new(format!("cannot write the status: {error}")))?;
                text.push('\n');
                output::print_json(&text)?;
            }
            Format::Jsonl => {
                let mut text = serde_json::to_string(&report)
                    .map_err(|error| Error::new(format!("cannot write the status: {error}")))?;
                text.push('\n');
                output::print_json(&text)?;
            }
            Format::Table => output::print_record(&report.record(true), globals)?,
            Format::Csv => output::print_record(&report.record(false), globals)?,
        }
    }

    if args.check && !report.up_to_date {
        if silent {
            output::warning(&report.status_line());
        }
        return Err(Error::exit(1));
    }
    Ok(())
}

/// How the database relates to the vault it was decrypted from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    UpToDate,
    Changed,
    OtherVault,
    Unreachable(String),
    /// The database does not record what a comparison needs; the detail
    /// names the keys.
    Incomplete(String),
}

impl State {
    /// The stable name for scripts.
    fn key(&self) -> &'static str {
        match self {
            State::UpToDate => "up_to_date",
            State::Changed => "vault_changed",
            State::OtherVault => "other_vault",
            State::Unreachable(_) => "vault_unreachable",
            State::Incomplete(_) => "metadata_incomplete",
        }
    }

    /// The wording for people.
    fn text(&self) -> &'static str {
        match self {
            State::UpToDate => "up to date",
            State::Changed => "vault changed since decrypt",
            State::OtherVault => "another vault is at that path now",
            State::Unreachable(_) => "vault not reachable",
            State::Incomplete(_) => "database metadata incomplete",
        }
    }
}

/// Everything `b4 status` says. The field order is the order of the keys in
/// JSON output.
#[derive(Debug, Serialize)]
struct Report {
    database: String,
    size_bytes: u64,
    b4_version: Option<String>,
    schema_version: Option<String>,
    decrypted_at: Option<String>,
    vault_path: Option<String>,
    vault_id: Option<String>,
    vault_created: Option<String>,
    vault_modified: Option<String>,
    entries_never_read: Option<u64>,
    state: &'static str,
    up_to_date: bool,
    state_detail: Option<String>,
    checks: Vec<&'static str>,
    rows: u64,
    tables: BTreeMap<String, u64>,
    warnings: Vec<String>,
    #[serde(skip)]
    state_text: &'static str,
}

impl Report {
    fn gather(database: &Database) -> Result<Report, Error> {
        let path = database.path();
        let size_bytes = std::fs::metadata(path)
            .context(format!("cannot read the size of {}", path.display()))?
            .len();

        // An empty value means "not set", the same as a missing one.
        let meta = |key: &str| {
            database
                .meta(key)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };

        let mut tables = BTreeMap::new();
        for table in database.tables()? {
            // These two describe the database, not the vault's content.
            if table.name == META_TABLE || table.name == WARNINGS_TABLE {
                continue;
            }
            let rows = database.row_count(&table)?;
            tables.insert(table.name, rows);
        }

        let (state, checks) = vault_state(database);
        Ok(Report {
            database: path.display().to_string(),
            size_bytes,
            b4_version: meta(meta_key::B4_VERSION),
            schema_version: meta(meta_key::SCHEMA_VERSION),
            decrypted_at: meta(meta_key::DECRYPTED_AT),
            vault_path: meta(meta_key::VAULT_PATH),
            vault_id: meta(meta_key::VAULT_ID),
            vault_created: meta(meta_key::VAULT_CREATED),
            vault_modified: meta(meta_key::VAULT_MODIFIED),
            entries_never_read: meta(meta_key::ENTRIES_NEVER_READ)
                .and_then(|count| count.parse().ok()),
            state: state.key(),
            up_to_date: state == State::UpToDate,
            state_text: state.text(),
            state_detail: match state {
                State::Unreachable(detail) | State::Incomplete(detail) => Some(detail),
                _ => None,
            },
            checks: checks.iter().map(|check| check.key()).collect(),
            rows: tables.values().sum(),
            tables,
            warnings: database.warnings()?,
        })
    }

    /// The status with its reason: `vault not reachable: cannot read ...`.
    fn status_line(&self) -> String {
        match &self.state_detail {
            Some(detail) => format!("{}: {detail}", self.state_text),
            None => self.state_text.to_owned(),
        }
    }

    /// The report as a record: worded for a person (`for_people`), or the
    /// scalar facts under the keys of the JSON object for CSV.
    fn record(&self, for_people: bool) -> Record {
        let count = |value: u64| Cell::Integer(i64::try_from(value).unwrap_or(i64::MAX));
        let mut record = Record::new();
        record.push(Column::text("database", "Database"), self.database.as_str());
        if for_people {
            record.push(Column::text("size", "Size"), size_text(self.size_bytes));
            let written_by = match (&self.b4_version, &self.schema_version) {
                (Some(b4), Some(schema)) => Some(format!("b4 {b4}, schema version {schema}")),
                (Some(b4), None) => Some(format!("b4 {b4}")),
                (None, Some(schema)) => Some(format!("schema version {schema}")),
                (None, None) => None,
            };
            record.push(Column::text("written_by", "Written by"), written_by);
        } else {
            record.push(
                Column::integer("size_bytes", "Size"),
                count(self.size_bytes),
            );
            record.push(
                Column::text("b4_version", "b4 version"),
                self.b4_version.clone(),
            );
            record.push(
                Column::text("schema_version", "Schema version"),
                self.schema_version.clone(),
            );
        }
        record.push(
            Column::text("decrypted_at", "Decrypted"),
            self.decrypted_at.clone(),
        );
        record.push(Column::text("vault_path", "Vault"), self.vault_path.clone());
        record.push(Column::text("vault_id", "Vault ID"), self.vault_id.clone());
        if !for_people {
            record.push(
                Column::text("vault_created", "Vault created"),
                self.vault_created.clone(),
            );
        }
        record.push(
            Column::text("vault_modified", "Vault modified"),
            self.vault_modified.clone(),
        );

        if for_people {
            let never_read = self.entries_never_read.map(|entries| {
                let noun = if entries == 1 { "entry" } else { "entries" };
                format!("{entries} {noun} with bank access data")
            });
            record.push(Column::text("entries_never_read", "Never read"), never_read);
            record.push(Column::text("tables", "Rows"), self.row_counts());
            let warnings = match self.warnings.len() {
                0 => "none".to_owned(),
                n => format!("{n} (listed by `b4 status --output json`)"),
            };
            record.push(Column::text("warnings", "Warnings"), warnings);
            record.push(Column::text("checks", "Compared"), self.checks_text());
            record.push(Column::text("state", "Status"), self.status_line());
        } else {
            record.push(
                Column::integer("entries_never_read", "Never read"),
                self.entries_never_read.map(count),
            );
            record.push(Column::text("state", "Status"), self.state);
            record.push(Column::boolean("up_to_date", "Up to date"), self.up_to_date);
            record.push(
                Column::text("state_detail", "Detail"),
                self.state_detail.clone(),
            );
            record.push(Column::text("checks", "Compared"), self.checks.join(" "));
            record.push(Column::integer("rows", "Rows"), count(self.rows));
            record.push(
                Column::integer("tables", "Tables"),
                count(self.tables.len() as u64),
            );
            record.push(
                Column::integer("warnings", "Warnings"),
                count(self.warnings.len() as u64),
            );
        }
        record
    }

    /// The comparisons that were made, for people.
    fn checks_text(&self) -> String {
        let names: Vec<&str> = Check::ALL
            .iter()
            .filter(|check| self.checks.contains(&check.key()))
            .map(|check| check.text())
            .collect();
        if names.is_empty() {
            "nothing".to_owned()
        } else {
            names.join(", ")
        }
    }

    /// One line per table: the name and its number of rows, in two columns.
    fn row_counts(&self) -> String {
        if self.tables.is_empty() {
            return "no tables".to_owned();
        }
        let name_width = self
            .tables
            .keys()
            .map(|name| output::display_width(name))
            .max()
            .unwrap_or(0);
        let count_width = self
            .tables
            .values()
            .map(|rows| rows.to_string().len())
            .max()
            .unwrap_or(0);
        self.tables
            .iter()
            .map(|(name, rows)| {
                let padding = name_width.saturating_sub(output::display_width(name));
                format!("{name}{}  {rows:>count_width$}", " ".repeat(padding))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// One comparison between the vault and what the database recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    VaultId,
    Stamp,
    Size,
    Checksum,
}

impl Check {
    const ALL: [Check; 4] = [Check::VaultId, Check::Stamp, Check::Size, Check::Checksum];

    /// The stable name for scripts.
    fn key(self) -> &'static str {
        match self {
            Check::VaultId => "vault_id",
            Check::Stamp => "modified_stamp",
            Check::Size => "size",
            Check::Checksum => "checksum",
        }
    }

    /// The wording for people.
    fn text(self) -> &'static str {
        match self {
            Check::VaultId => "vault id",
            Check::Stamp => "modification stamp",
            Check::Size => "size",
            Check::Checksum => "content checksum",
        }
    }

    /// The key of the `meta` table that holds the recorded value.
    fn meta_key(self) -> &'static str {
        match self {
            Check::VaultId => meta_key::VAULT_ID,
            Check::Stamp => meta_key::VAULT_MODIFIED_STAMP,
            Check::Size => meta_key::VAULT_SIZE,
            Check::Checksum => meta_key::VAULT_CHECKSUM,
        }
    }
}

/// What the database recorded about its vault. `None` is a value that is
/// missing, empty or not of its kind: it can be compared with nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Recorded {
    vault_id: Option<String>,
    stamp: Option<i32>,
    size: Option<u64>,
    checksum: Option<String>,
}

impl Recorded {
    fn read(database: &Database) -> Recorded {
        let stored = |check: Check| {
            database
                .meta(check.meta_key())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        Recorded {
            vault_id: stored(Check::VaultId).map(str::to_owned),
            stamp: stored(Check::Stamp).and_then(|stamp| stamp.parse().ok()),
            size: stored(Check::Size).and_then(|size| size.parse().ok()),
            // What `content_checksum` writes, and nothing else.
            checksum: stored(Check::Checksum)
                .filter(|sum| sum.len() == 16 && sum.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(str::to_ascii_lowercase),
        }
    }
}

/// Compare the vault at the stored path with what was stored about it.
/// Returns the state and the comparisons that were made to reach it.
fn vault_state(database: &Database) -> (State, Vec<Check>) {
    let Some(vault_path) = database
        .meta(meta_key::VAULT_PATH)
        .filter(|value| !value.is_empty())
    else {
        let reason = "the database does not say where its vault is";
        return (State::Unreachable(reason.to_owned()), Vec::new());
    };
    let vault_path = Path::new(vault_path);
    let header = match Header::read(vault_path) {
        Ok(header) => header,
        Err(error) => return (State::Unreachable(error.to_string()), Vec::new()),
    };
    let size = match std::fs::metadata(vault_path) {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            let reason = format!("cannot read the size of {}: {error}", vault_path.display());
            return (State::Unreachable(reason), Vec::new());
        }
    };
    compare(
        &Recorded::read(database),
        &header.file_guid().to_string(),
        header.modified_at().raw(),
        size,
        || {
            std::fs::read(vault_path)
                .map(|bytes| content_checksum(&bytes))
                .map_err(|error| format!("cannot read {}: {error}", vault_path.display()))
        },
    )
}

/// Decide the state from what was `recorded` and what the vault is now: its
/// id, modification stamp and size, and `checksum`, which reads the whole
/// file and is only called when the three before it match.
///
/// A recorded value that is missing never counts as a match. A difference
/// found by a comparison that could be made still decides; without one, a
/// missing value makes the state [`State::Incomplete`].
fn compare(
    recorded: &Recorded,
    vault_id: &str,
    stamp: i32,
    size: u64,
    checksum: impl FnOnce() -> Result<String, String>,
) -> (State, Vec<Check>) {
    let mut made = Vec::new();
    let mut missing = Vec::new();
    // Whether the recorded value equals the present one, noting that the
    // comparison was made or could not be.
    let mut same = |check: Check, equal: Option<bool>| {
        match equal {
            Some(_) => made.push(check),
            None => missing.push(check),
        }
        equal
    };

    let same_id = same(
        Check::VaultId,
        recorded
            .vault_id
            .as_deref()
            .map(|id| id.eq_ignore_ascii_case(vault_id)),
    );
    if same_id == Some(false) {
        return (State::OtherVault, made);
    }
    // The stamp is written when Banking4 closes the vault; the size is a
    // second witness.
    let same_stamp = same(Check::Stamp, recorded.stamp.map(|s| s == stamp));
    let same_size = same(Check::Size, recorded.size.map(|s| s == size));
    if same_stamp == Some(false) || same_size == Some(false) {
        return (State::Changed, made);
    }
    if recorded.checksum.is_none() {
        missing.push(Check::Checksum);
    }
    if !missing.is_empty() {
        let keys: Vec<&str> = missing.iter().map(|check| check.meta_key()).collect();
        let detail = format!(
            "the database does not record {}: run `b4 decrypt <VAULT>` to write it again",
            keys.join(", ")
        );
        return (State::Incomplete(detail), made);
    }

    // Id, stamp and size all match: only the content can tell now.
    let now = match checksum() {
        Ok(now) => now,
        Err(reason) => return (State::Unreachable(reason), made),
    };
    made.push(Check::Checksum);
    if recorded.checksum.as_deref() == Some(now.as_str()) {
        (State::UpToDate, made)
    } else {
        (State::Changed, made)
    }
}

/// A file size for people: `512 bytes`, `1.3 MB`.
fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        let noun = if bytes == 1 { "byte" } else { "bytes" };
        return format!("{bytes} {noun}");
    }
    // Tenths of the unit, in integers: no float decides a digit.
    let mut tenths = bytes / 100;
    let mut unit = 0;
    while tenths >= 10_000 && unit + 1 < UNITS.len() {
        tenths /= 1000;
        unit += 1;
    }
    let name = UNITS.get(unit).copied().unwrap_or("TB");
    format!("{}.{} {name}", tenths / 10, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_readable() {
        assert_eq!(size_text(0), "0 bytes");
        assert_eq!(size_text(1), "1 byte");
        assert_eq!(size_text(999), "999 bytes");
        assert_eq!(size_text(1000), "1.0 kB");
        assert_eq!(size_text(1_536_000), "1.5 MB");
        assert_eq!(size_text(999_999), "999.9 kB");
        assert_eq!(size_text(3_000_000_000), "3.0 GB");
    }

    #[test]
    fn every_state_has_a_key_and_a_wording() {
        let states = [
            State::UpToDate,
            State::Changed,
            State::OtherVault,
            State::Unreachable("gone".to_owned()),
            State::Incomplete("no size".to_owned()),
        ];
        let keys: Vec<&str> = states.iter().map(State::key).collect();
        assert_eq!(
            keys,
            [
                "up_to_date",
                "vault_changed",
                "other_vault",
                "vault_unreachable",
                "metadata_incomplete"
            ]
        );
        assert_eq!(states[1].text(), "vault changed since decrypt");
        assert_eq!(states[4].text(), "database metadata incomplete");
    }

    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    fn recorded() -> Recorded {
        Recorded {
            vault_id: Some(ID.to_owned()),
            stamp: Some(804_600_000),
            size: Some(4096),
            checksum: Some(content_checksum(b"vault")),
        }
    }

    /// The state of a vault with these facts, the keys of the comparisons
    /// made, and whether the file was read in full.
    fn state_of(
        recorded: &Recorded,
        id: &str,
        stamp: i32,
        size: u64,
        content: &[u8],
    ) -> (State, Vec<&'static str>, bool) {
        let mut read = false;
        let (state, checks) = compare(recorded, id, stamp, size, || {
            read = true;
            Ok(content_checksum(content))
        });
        (state, checks.iter().map(|c| c.key()).collect(), read)
    }

    #[test]
    fn up_to_date_takes_all_four_comparisons() {
        let (state, checks, read) = state_of(&recorded(), ID, 804_600_000, 4096, b"vault");
        assert_eq!(state, State::UpToDate);
        assert_eq!(checks, ["vault_id", "modified_stamp", "size", "checksum"]);
        assert!(read);
        // The id is compared without regard to case.
        let upper = ID.to_uppercase();
        assert_eq!(
            state_of(&recorded(), &upper, 804_600_000, 4096, b"vault").0,
            State::UpToDate
        );
    }

    #[test]
    fn the_content_decides_when_stamp_and_size_are_the_same() {
        let (state, checks, read) = state_of(&recorded(), ID, 804_600_000, 4096, b"vaulT");
        assert_eq!(state, State::Changed);
        assert_eq!(checks, ["vault_id", "modified_stamp", "size", "checksum"]);
        assert!(read);
    }

    #[test]
    fn an_earlier_difference_decides_without_reading_the_file() {
        let other = "ffffffff-89ab-cdef-0123-456789abcdef";
        let (state, checks, read) = state_of(&recorded(), other, 804_600_000, 4096, b"vault");
        assert_eq!((state, read), (State::OtherVault, false));
        assert_eq!(checks, ["vault_id"]);

        let (state, checks, read) = state_of(&recorded(), ID, 804_600_001, 4096, b"vault");
        assert_eq!((state, read), (State::Changed, false));
        assert_eq!(checks, ["vault_id", "modified_stamp", "size"]);

        let (state, _, read) = state_of(&recorded(), ID, 804_600_000, 4097, b"vault");
        assert_eq!((state, read), (State::Changed, false));
    }

    #[test]
    fn a_missing_recorded_value_is_never_a_match() {
        let incomplete = |keys: &str| {
            State::Incomplete(format!(
                "the database does not record {keys}: run `b4 decrypt <VAULT>` to write it again"
            ))
        };
        let without = |change: fn(&mut Recorded)| {
            let mut recorded = recorded();
            change(&mut recorded);
            recorded
        };

        // Each key on its own, with a vault that is otherwise the same.
        for (recorded, key, made) in [
            (
                without(|r| r.vault_id = None),
                "vault_id",
                vec!["modified_stamp", "size"],
            ),
            (
                without(|r| r.stamp = None),
                "vault_modified_stamp",
                vec!["vault_id", "size"],
            ),
            (
                without(|r| r.size = None),
                "vault_size",
                vec!["vault_id", "modified_stamp"],
            ),
            (
                without(|r| r.checksum = None),
                "vault_checksum",
                vec!["vault_id", "modified_stamp", "size"],
            ),
        ] {
            let (state, checks, read) = state_of(&recorded, ID, 804_600_000, 4096, b"vault");
            assert_eq!(state, incomplete(key));
            assert_eq!(checks, made, "{key}");
            assert!(!read, "{key}");
        }
        let (state, checks, _) = state_of(&Recorded::default(), ID, 804_600_000, 4096, b"vault");
        assert_eq!(
            state,
            incomplete("vault_id, vault_modified_stamp, vault_size, vault_checksum")
        );
        assert!(checks.is_empty());

        // The finding: no recorded size, and the vault grew. Not up to date.
        let (state, _, _) = state_of(
            &without(|r| r.size = None),
            ID,
            804_600_000,
            999_999,
            b"vault",
        );
        assert_eq!(state, incomplete("vault_size"));
        // A difference that can still be seen is reported as what it is.
        let (state, _, _) = state_of(&without(|r| r.size = None), ID, 804_600_001, 4096, b"vault");
        assert_eq!(state, State::Changed);
        let other = "ffffffff-89ab-cdef-0123-456789abcdef";
        let (state, _, _) = state_of(
            &without(|r| r.checksum = None),
            other,
            804_600_000,
            4096,
            b"vault",
        );
        assert_eq!(state, State::OtherVault);
    }

    #[test]
    fn a_vault_that_cannot_be_read_in_full_is_unreachable() {
        let (state, checks) = compare(&recorded(), ID, 804_600_000, 4096, || {
            Err("cannot read vault.sub: permission denied".to_owned())
        });
        assert_eq!(
            state,
            State::Unreachable("cannot read vault.sub: permission denied".to_owned())
        );
        assert_eq!(checks.len(), 3);
    }
}
