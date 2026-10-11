//! Writes a decoded vault into a new database.
//!
//! [`write()`] never touches an existing database until the new one is
//! complete: it builds the file in a private directory next to the target,
//! closes the connection (which checkpoints, so no `.wal` is left) and then
//! renames the file over the target. A reader sees the old database or the
//! new one, never half of either. Whatever goes wrong, the private directory
//! is removed and the target is as it was.
//!
//! Of what was there before, b4 moves or deletes two things and nothing
//! else: the database at the target, which [`check_target`] has found to be
//! one, and a write-ahead log next to it, if that is a regular file. Anything
//! else with the name of the log (a directory, a symbolic link, a socket)
//! stops the command before the password is asked for. The private directory
//! is created under a fresh name and never taken over from someone else.
//!
//! Values are stored as they are. Two kinds do not always fit their column:
//!
//! * a decimal with more decimals than the column has is rounded half to
//!   even, and one with more digits before the point than the column holds
//!   is stored as NULL;
//! * a stored date that is no calendar date (month 13, 31 February, a year
//!   alone) is stored as NULL.
//!
//! Each case is counted per table and column and reported as one warning
//! that names the column and the count, never the value.

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use duckdb::types::{TimeUnit, Value};
use duckdb::{appender_params_from_iter, Appender, Config, Connection};
use rust_decimal::{Decimal, RoundingStrategy};

use super::schema::{self, table, Kind, Table};
use super::{meta_key, read::message_of, META_TABLE, SCHEMA_VERSION, WARNINGS_TABLE};
use crate::error::Context;
use crate::load::{InAccount, Snapshot};
use crate::model::accounts::{AccountFlags, GroupFlags};
use crate::model::base::RecordBase;
use crate::model::masterdata::{category_key, MandateFlags, PortfolioItemState};
use crate::model::payments::{BatchRole, EntryBits, PaymentBase, StandingOrderState};
use crate::vault::{Header, UtcDateTime};
use crate::wire::{Date, RawDate, Stamp, TimeOfDay, Timestamp};
use crate::Error;

/// Where the magic bytes of a DuckDB file are, and what they are.
const DUCKDB_MAGIC_OFFSET: usize = 8;
const DUCKDB_MAGIC: &[u8] = b"DUCK";

/// Name of the database inside its private directory while it is written.
const WORK_FILE: &str = "b4.duckdb";

/// Name, inside the private directory, of a write-ahead log that was found
/// next to the target and moved out of the way.
const ASIDE_LOG: &str = "replaced.wal";

/// What DuckDB appends to the name of a database for its write-ahead log.
const LOG_SUFFIX: &str = ".wal";

/// What the database records about the vault it was decrypted from.
#[derive(Debug, Clone)]
pub struct Source<'a> {
    /// Absolute path of the vault file.
    pub path: &'a Path,
    /// The vault's header.
    pub header: &'a Header,
    /// Size of the vault file in bytes.
    pub size: u64,
    /// [`content_checksum`](super::content_checksum) of the vault file.
    pub checksum: &'a str,
}

/// What [`write()`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// Every record table with the number of rows written, in schema order.
    pub rows: Vec<(&'static str, usize)>,
    /// The writer's own warnings: values that did not fit their column. They
    /// are stored in the database after those of the snapshot.
    pub warnings: Vec<String>,
}

/// Check that `target` can be written without harm: it is not the vault, the
/// write-ahead log that goes with it is not the vault either and is nothing
/// but a regular file, and if the target exists it is a DuckDB database. Call
/// this before asking for the password, so a wrong target fails at once.
pub fn check_target(target: &Path, vault: &Path) -> Result<(), Error> {
    let shown = target.display();
    if same_file(target, vault) {
        return Err(Error::new(format!(
            "{shown} is the vault itself: the database needs a file of its own"
        )));
    }
    // Replacing a database takes the file with the name of its write-ahead
    // log out of the way ([`replace`]). That must never be the vault.
    let log = log_of(target);
    if same_file(&log, vault) {
        return Err(Error::new(format!(
            "{} is the vault itself, and a database at {shown} would use that name for its \
             write-ahead log: name another file",
            log.display()
        )));
    }
    log_in_the_way(&log, target)?;
    let mut file = match fs::File::open(target) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(Error::from(error).context(format!("cannot check the target {shown}")))
        }
    };
    let refuse = || {
        Error::new(format!(
            "{shown} exists and is not a DuckDB database: refusing to replace it; name another \
             file or remove it first"
        ))
    };
    if file.metadata().is_ok_and(|metadata| metadata.is_dir()) {
        return Err(refuse());
    }
    let mut start = [0u8; DUCKDB_MAGIC_OFFSET + DUCKDB_MAGIC.len()];
    match file.read_exact(&mut start) {
        Ok(()) if start.ends_with(DUCKDB_MAGIC) => Ok(()),
        Ok(()) => Err(refuse()),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Err(refuse()),
        Err(error) => Err(Error::from(error).context(format!("cannot check the target {shown}"))),
    }
}

/// What is at `log`, the name of the write-ahead log of `target`: `true` for
/// a regular file, which is a log and may be moved out of the way, `false`
/// for nothing. Anything else is an error: a directory, a symbolic link
/// (which is not followed, wherever it leads) or a special file is not a log
/// b4 could move aside and delete, and DuckDB would still look for a log
/// under that name.
fn log_in_the_way(log: &Path, target: &Path) -> Result<bool, Error> {
    let kind = match fs::symlink_metadata(log) {
        Ok(metadata) => metadata.file_type(),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(Error::from(error).context(format!(
                "cannot check for a write-ahead log at {}; nothing was replaced",
                log.display()
            )))
        }
    };
    if kind.is_file() {
        return Ok(true);
    }
    let what = if kind.is_dir() {
        "a directory"
    } else if kind.is_symlink() {
        "a symbolic link"
    } else {
        "not a regular file"
    };
    Err(Error::new(format!(
        "{} is {what}, where the write-ahead log of a database at {} belongs: b4 will not \
         replace a database while that entry is in the way, and does not touch it; move it away \
         or name another file",
        log.display(),
        target.display()
    )))
}

/// Whether the two paths lead to the same file, through links of any kind. A
/// path that does not exist is the same as nothing.
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (fs::metadata(a), fs::metadata(b)) {
            return a.dev() == b.dev() && a.ino() == b.ino();
        }
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The private directory the database is built in. Removing it on drop
/// covers every way out of [`write()`], the early returns included.
struct WorkDir {
    path: PathBuf,
    /// Set when the directory holds something of the user's that could not
    /// be put back: it is then left where it is.
    keep: bool,
}

impl WorkDir {
    /// Create it next to `target`, so that the final rename stays within one
    /// file system. Only the owner can enter it: DuckDB creates its files
    /// with the process's default mode, and nobody else may read the
    /// database while it is still in here.
    fn create(target: &Path) -> Result<Self, Error> {
        let parent = match target.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        let path = parent.join(format!(".b4-decrypt-{}-{nanos:09}", std::process::id()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).context(format!(
            "cannot create a working directory in {}",
            parent.display()
        ))?;
        Ok(Self { path, keep: false })
    }

    fn database(&self) -> PathBuf {
        self.path.join(WORK_FILE)
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        // The one thing in here that b4 did not create is a log that was
        // moved aside. [`replace`] has put it back or deleted it; should it
        // still be here, it goes as the single file it is, and if it is
        // anything else the directory stays.
        match fs::remove_file(self.path.join(ASIDE_LOG)) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return,
        }
        // The rest is b4's own, in a directory it created a moment ago under
        // a fresh name.
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The write-ahead log DuckDB keeps, and looks for, next to the database at
/// `target`.
fn log_of(target: &Path) -> PathBuf {
    let mut log = target.as_os_str().to_owned();
    log.push(LOG_SUFFIX);
    PathBuf::from(log)
}

/// Put the finished `database` at `target`, replacing what is there, and
/// make sure no write-ahead log of an earlier database is left next to it.
///
/// A log next to the target is the unfinished business of a database that
/// was there before: changes another program committed and had not yet
/// written into the file when it ended. DuckDB replays a log it finds next
/// to a database whenever it opens one, read-only (as every `b4` command
/// does) or for writing (as its own shell does), and it does not check that
/// the log belongs to that database: the old log is replayed into the new
/// database without a word (observed with DuckDB 1.5). So there are two
/// states that must never exist, not even for the moment in which the
/// process can be killed:
///
/// - the old database without its log, with the log destroyed: its last
///   changes would be lost although the replacement never happened;
/// - the new database next to the old log: the next open would mix the two.
///
/// Removing the log first allows the first state, renaming first the second.
/// Hence three steps, each one atomic rename or unlink:
///
/// Only a regular file is a log. Anything else under its name is refused by
/// [`check_target`] and again here, and is not touched.
///
/// 1. The log is moved into the working directory. Killed after this, the
///    old database is at its last checkpoint and its log is whole, in a
///    directory next to it; nothing can replay it into the wrong file. If it
///    cannot be moved, nothing has changed yet and the error says so.
/// 2. The database is renamed onto the target. If that fails, the log is
///    moved back, and the old database is as it was. If even that fails,
///    the working directory is kept and the error says where the log is.
/// 3. The old log is deleted, as the single file it is. It belonged to a
///    database that no longer exists.
///
/// A log that is next to the target after step 2 was put there in between
/// by a program that had the old database open. It is removed if it is a
/// regular file; if it is not, or cannot be removed, that is an error which
/// names what to delete, because the database must not be opened with it
/// there.
fn replace(database: &Path, target: &Path, work: &mut WorkDir) -> Result<(), Error> {
    let log = log_of(target);
    let aside = work.path.join(ASIDE_LOG);
    // Asked first, so that "there is no log" is not confused with a rename
    // that failed for a reason of its own, and so that nothing but a regular
    // file is ever moved: the check of [`check_target`] again, because the
    // database took its time to write.
    let had_log = log_in_the_way(&log, target)?;
    if had_log {
        fs::rename(&log, &aside).map_err(|error| {
            Error::from(error).context(format!(
                "cannot move the write-ahead log {} out of the way; nothing was replaced, \
                 delete that file if the database it belongs to is not needed and decrypt again",
                log.display()
            ))
        })?;

        // What was moved is what was checked, unless it was exchanged in the
        // moment between. Then it goes back where it was.
        if !fs::symlink_metadata(&aside).is_ok_and(|metadata| metadata.is_file()) {
            let failed = format!(
                "{} is not a regular file, where the write-ahead log of a database at {} \
                 belongs: b4 will not replace a database while that entry is in the way",
                log.display(),
                target.display()
            );
            return Err(match fs::rename(&aside, &log) {
                Ok(()) => Error::new(format!("{failed}; nothing was replaced")),
                Err(_) => {
                    work.keep = true;
                    Error::new(format!(
                        "{failed}; nothing was replaced, and that entry is now at {}, move it \
                         back",
                        aside.display()
                    ))
                }
            });
        }
    }

    if let Err(error) = fs::rename(database, target) {
        let failed =
            Error::from(error).context(format!("cannot put the database at {}", target.display()));
        if !had_log {
            return Err(failed);
        }
        return Err(match fs::rename(&aside, &log) {
            Ok(()) => failed,
            Err(_) => {
                // The log is the user's: it must not go with the directory.
                work.keep = true;
                Error::new(format!(
                    "{failed}; the write-ahead log of the database that is there is now at {}, \
                     move it back to {}",
                    aside.display(),
                    log.display()
                ))
            }
        });
    }

    // The old log, one file, by its name: never as a part of removing the
    // directory it is in.
    if had_log {
        match fs::remove_file(&aside) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                work.keep = true;
                return Err(Error::from(error).context(format!(
                    "the database at {} was replaced, but the write-ahead log of the old one, \
                     now at {}, cannot be deleted; delete it",
                    target.display(),
                    aside.display()
                )));
            }
        }
    }

    let in_the_way = |reason: String| {
        Error::new(format!(
            "the database at {} was replaced, but {} is next to it where its write-ahead log \
             belongs and {reason}; remove it before the database is opened",
            target.display(),
            log.display()
        ))
    };
    match fs::symlink_metadata(&log) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(in_the_way(format!("cannot be checked ({error})"))),
        Ok(metadata) if !metadata.is_file() => Err(in_the_way(
            "is not a regular file, so b4 does not touch it".to_owned(),
        )),
        Ok(_) => match fs::remove_file(&log) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(in_the_way(format!("cannot be removed ({error})"))),
        },
    }
}

/// Write `snapshot` into a new database and put it at `target`, replacing a
/// database that is there. `target` must have passed [`check_target`].
///
/// If this fails, the database that was at `target` is still there and as
/// it was, with its write-ahead log if it had one (see [`replace`] for the
/// one case in which the log ends up elsewhere, and is named).
pub fn write(snapshot: &Snapshot, source: &Source<'_>, target: &Path) -> Result<Written, Error> {
    check_target(target, source.path)?;
    let mut work = WorkDir::create(target)?;
    let database = work.database();

    let connection = open(&database)?;
    let written = fill(&connection, snapshot, source)
        .context(format!("cannot write the database {}", target.display()))?;
    // Closing checkpoints: everything is in the one file afterwards.
    connection.close().map_err(|(_, error)| {
        Error::new(format!(
            "cannot finish the database {}: {}",
            target.display(),
            message_of(&error)
        ))
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600))
            .context("cannot restrict the database to its owner")?;
    }
    replace(&database, target, &mut work)?;
    Ok(written)
}

/// Open a new database for writing. b4 never reaches the network: extension
/// auto-install and auto-load are off on every connection.
fn open(path: &Path) -> Result<Connection, Error> {
    Config::default()
        .enable_autoload_extension(false)
        .and_then(|config| config.with("autoinstall_known_extensions", "false"))
        .and_then(|config| Connection::open_with_flags(path, config))
        .map_err(|error| {
            Error::new(format!(
                "cannot create the database in {}: {}",
                path.parent().unwrap_or(path).display(),
                message_of(&error)
            ))
        })
}

/// Create the schema and load every table.
fn fill(
    connection: &Connection,
    snapshot: &Snapshot,
    source: &Source<'_>,
) -> Result<Written, Error> {
    // One transaction for everything. Outside of one, each of the several
    // hundred `COMMENT ON` statements is a commit of its own with its own
    // sync to disk, which costs seconds on an ordinary file system.
    connection.execute_batch("begin transaction")?;
    schema::create(connection)?;

    let mut written = Written {
        rows: Vec::new(),
        warnings: Vec::new(),
    };
    for table in schema::tables() {
        if table.name == META_TABLE || table.name == WARNINGS_TABLE {
            continue;
        }
        let mut sink = Sink::new(connection, table)?;
        write_table(&mut sink, snapshot)?;
        let (rows, warnings) = sink.finish()?;
        written.rows.push((table.name, rows));
        written.warnings.extend(warnings);
    }

    let mut sink = Sink::new(connection, find(META_TABLE)?)?;
    for (key, value) in meta(snapshot, source) {
        sink.text("key", Some(key));
        // An empty value is a value here: "not set", as the keys document.
        sink.put("value", Kind::Text, Value::Text(value));
        sink.end()?;
    }
    sink.finish()?;

    let mut sink = Sink::new(connection, find(WARNINGS_TABLE)?)?;
    for warning in snapshot.warnings.iter().chain(&written.warnings) {
        sink.text("message", Some(warning));
        sink.end()?;
    }
    sink.finish()?;
    connection.execute_batch("commit")?;
    Ok(written)
}

fn find(name: &str) -> Result<&'static Table, Error> {
    schema::find_table(name).ok_or_else(|| Error::new(format!("the schema has no table {name}")))
}

/// The rows of the `meta` table.
fn meta(snapshot: &Snapshot, source: &Source<'_>) -> Vec<(&'static str, String)> {
    let header = source.header;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let now = UtcDateTime::from_unix_seconds(i64::try_from(now).unwrap_or(i64::MAX));
    vec![
        (meta_key::SCHEMA_VERSION, SCHEMA_VERSION.to_string()),
        (meta_key::B4_VERSION, env!("CARGO_PKG_VERSION").to_owned()),
        (meta_key::VAULT_PATH, source.path.display().to_string()),
        (meta_key::VAULT_ID, header.file_guid().to_string()),
        (
            meta_key::VAULT_CREATED,
            header.created_at().to_iso8601().unwrap_or_default(),
        ),
        (
            meta_key::VAULT_MODIFIED,
            header.modified_at().to_iso8601().unwrap_or_default(),
        ),
        (
            meta_key::VAULT_MODIFIED_STAMP,
            header.modified_at().raw().to_string(),
        ),
        (meta_key::VAULT_SIZE, source.size.to_string()),
        (meta_key::DECRYPTED_AT, now.to_string()),
        (meta_key::VAULT_CHECKSUM, source.checksum.to_owned()),
        (
            meta_key::ENTRIES_NEVER_READ,
            snapshot.entries_never_read.to_string(),
        ),
    ]
}

/// What happened to a value that did not fit its column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Misfit {
    Rounded,
    TooLarge,
    NoCalendarDate,
}

/// A decimal as the integer DuckDB stores for a `DECIMAL(width, scale)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fitted {
    /// The value, exactly.
    Exact(i128),
    /// The value rounded half to even to the column's scale.
    Rounded(i128),
    /// More digits before the point than the column holds.
    TooLarge,
}

/// Fit `value` into a `DECIMAL(width, scale)` column.
fn fit(value: Decimal, (width, scale): (u8, u8)) -> Fitted {
    let rounded =
        value.round_dp_with_strategy(u32::from(scale), RoundingStrategy::MidpointNearestEven);
    // Rounding never leaves more decimals than asked for, so the exponent
    // below is not negative.
    let missing = u32::from(scale).saturating_sub(rounded.scale());
    let limit = 10_i128.checked_pow(u32::from(width));
    let scaled = 10_i128
        .checked_pow(missing)
        .and_then(|factor| rounded.mantissa().checked_mul(factor));
    match (scaled, limit) {
        (Some(scaled), Some(limit)) if scaled.abs() < limit => {
            if rounded == value {
                Fitted::Exact(scaled)
            } else {
                Fitted::Rounded(scaled)
            }
        }
        _ => Fitted::TooLarge,
    }
}

/// A stored date as a calendar date, or why it is none.
fn calendar_date(raw: Option<RawDate>) -> Result<Option<Date>, Misfit> {
    let Some(raw) = raw else { return Ok(None) };
    // 0001-01-01 is how the format spells "no date" (02 §4.12).
    if (raw.year, raw.month, raw.day) == (1, 1, 1) {
        return Ok(None);
    }
    if !raw.is_valid() {
        return Err(Misfit::NoCalendarDate);
    }
    u16::try_from(raw.year)
        .ok()
        .and_then(|year| Date::new(year, raw.month, raw.day))
        .map(Some)
        .ok_or(Misfit::NoCalendarDate)
}

/// Appends the rows of one table, one value at a time.
///
/// Every value is given with the name of its column, and the sink checks
/// name and type against the schema at the position the value arrives at. A
/// row can therefore not silently go into the wrong columns when the schema
/// changes: the first mismatch fails the whole write.
struct Sink<'c> {
    table: &'static Table,
    appender: Appender<'c>,
    row: Vec<Value>,
    rows: usize,
    fault: Option<String>,
    misfits: BTreeMap<(usize, Misfit), usize>,
}

impl<'c> Sink<'c> {
    fn new(connection: &'c Connection, table: &'static Table) -> Result<Self, Error> {
        Ok(Self {
            table,
            appender: connection.appender(table.name)?,
            row: Vec::with_capacity(table.columns.len()),
            rows: 0,
            fault: None,
            misfits: BTreeMap::new(),
        })
    }

    fn put(&mut self, column: &'static str, kind: Kind, value: Value) {
        let index = self.row.len();
        let expected = self.table.columns.get(index);
        if !expected.is_some_and(|expected| expected.name == column && expected.kind == kind) {
            self.fault.get_or_insert_with(|| {
                format!(
                    "value {index} of a row of {} is {column} ({}), the schema has {} there",
                    self.table.name,
                    kind.sql_type(),
                    expected.map_or("no column", |expected| expected.name)
                )
            });
        }
        self.row.push(value);
    }

    fn misfit(&mut self, misfit: Misfit) {
        *self.misfits.entry((self.row.len(), misfit)).or_default() += 1;
    }

    /// A text. An empty one is NULL: the format does not tell them apart.
    fn text(&mut self, column: &'static str, text: Option<&str>) {
        let value = match text {
            Some(text) if !text.is_empty() => Value::Text(text.to_owned()),
            _ => Value::Null,
        };
        self.put(column, Kind::Text, value);
    }

    /// A coded value or a flag set by its name; an empty flag set is NULL.
    fn name(&mut self, column: &'static str, name: impl ToString) {
        self.text(column, Some(&name.to_string()));
    }

    fn int(&mut self, column: &'static str, value: impl Into<i64>) {
        self.put(column, Kind::BigInt, Value::BigInt(value.into()));
    }

    /// A reference to another record, where 0 means "none".
    fn reference(&mut self, column: &'static str, oid: i32) {
        let value = if oid == 0 {
            Value::Null
        } else {
            Value::BigInt(i64::from(oid))
        };
        self.put(column, Kind::BigInt, value);
    }

    fn boolean(&mut self, column: &'static str, value: bool) {
        self.put(column, Kind::Boolean, Value::Boolean(value));
    }

    fn fitted(&mut self, column: &'static str, kind: Kind, value: Decimal, shape: (u8, u8)) {
        let scaled = match fit(value, shape) {
            Fitted::Exact(scaled) => Some(scaled),
            Fitted::Rounded(scaled) => {
                self.misfit(Misfit::Rounded);
                Some(scaled)
            }
            Fitted::TooLarge => {
                self.misfit(Misfit::TooLarge);
                None
            }
        };
        let value = scaled
            .and_then(|scaled| duckdb::types::Decimal::new(shape.0, shape.1, scaled).ok())
            .map_or(Value::Null, Value::Decimal);
        self.put(column, kind, value);
    }

    fn money(&mut self, column: &'static str, value: Decimal) {
        self.fitted(column, Kind::Money, value, schema::MONEY);
    }

    fn decimal(&mut self, column: &'static str, value: Decimal) {
        self.fitted(column, Kind::Decimal, value, schema::DECIMAL);
    }

    fn date(&mut self, column: &'static str, date: Option<Date>) {
        let value = date.map_or(Value::Null, |date| {
            Value::Date32(date.days_since_unix_epoch())
        });
        self.put(column, Kind::Date, value);
    }

    fn raw_date(&mut self, column: &'static str, raw: Option<RawDate>) {
        let date = calendar_date(raw).unwrap_or_else(|misfit| {
            self.misfit(misfit);
            None
        });
        self.date(column, date);
    }

    fn timestamp(&mut self, column: &'static str, timestamp: Option<Timestamp>) {
        let value = timestamp.map_or(Value::Null, |timestamp| {
            Value::Timestamp(TimeUnit::Microsecond, timestamp.unix_micros())
        });
        self.put(column, Kind::Timestamp, value);
    }

    fn stamp(&mut self, column: &'static str, stamp: Option<Stamp>) {
        let value = stamp.map_or(Value::Null, |stamp| {
            Value::Timestamp(TimeUnit::Microsecond, stamp.unix_micros())
        });
        self.put(column, Kind::Timestamp, value);
    }

    fn time(&mut self, column: &'static str, time: Option<TimeOfDay>) {
        let value = time.map_or(Value::Null, |time| {
            Value::Time64(TimeUnit::Microsecond, time.micros_since_midnight())
        });
        self.put(column, Kind::Time, value);
    }

    /// `id`, `created_at`, `updated_at`.
    fn record(&mut self, base: &RecordBase) {
        self.int("id", base.oid);
        self.stamp("created_at", Some(base.created()));
        // S7: a stamp of 0 says "before 2000", which is "unknown".
        self.stamp(
            "updated_at",
            base.updated().filter(|_| base.update_stamp != 0),
        );
    }

    /// `category` and its matching key.
    fn category(&mut self, category: Option<&str>) {
        self.text("category", category);
        self.text("category_key", category.map(category_key).as_deref());
    }

    fn end(&mut self) -> Result<(), Error> {
        if let Some(fault) = &self.fault {
            return Err(Error::new(format!("internal error: {fault}")));
        }
        if self.row.len() != self.table.columns.len() {
            return Err(Error::new(format!(
                "internal error: a row of {} has {} values, the schema has {} columns",
                self.table.name,
                self.row.len(),
                self.table.columns.len()
            )));
        }
        self.appender
            .append_row(appender_params_from_iter(self.row.drain(..)))?;
        self.rows += 1;
        Ok(())
    }

    /// Flush, and turn the counts of values that did not fit into warnings.
    fn finish(mut self) -> Result<(usize, Vec<String>), Error> {
        self.appender.flush()?;
        let warnings = self
            .misfits
            .iter()
            .filter_map(|((index, misfit), count)| {
                let column = self.table.columns.get(*index)?;
                Some(misfit_warning(
                    self.table.name,
                    column.name,
                    column.kind,
                    *misfit,
                    *count,
                ))
            })
            .collect();
        Ok((self.rows, warnings))
    }
}

fn misfit_warning(table: &str, column: &str, kind: Kind, misfit: Misfit, count: usize) -> String {
    let (values, were) = if count == 1 {
        ("value", "was")
    } else {
        ("values", "were")
    };
    let (width, scale) = if kind == Kind::Money {
        schema::MONEY
    } else {
        schema::DECIMAL
    };
    match misfit {
        Misfit::Rounded => format!(
            "{table}.{column}: {count} {values} with more than {scale} decimals {were} rounded"
        ),
        Misfit::TooLarge => format!(
            "{table}.{column}: {count} {values} with more than {} digits before the decimal \
             point {were} stored as NULL",
            width - scale
        ),
        Misfit::NoCalendarDate => format!(
            "{table}.{column}: {count} stored {values} that {} no calendar date {were} stored \
             as NULL",
            if count == 1 { "is" } else { "are" }
        ),
    }
}

/// The columns bookings, payments and standing orders share, group by group
/// in the order of the schema.
struct PaymentColumns<'a> {
    base: &'a PaymentBase,
}

impl PaymentColumns<'_> {
    fn head(&self, sink: &mut Sink<'_>, account_id: i32) {
        sink.int("account_id", account_id);
        sink.record(&self.base.record);
    }

    fn scheme(&self, sink: &mut Sink<'_>) {
        sink.name("scheme", self.base.scheme_type);
        sink.name("stored_scheme", self.base.stored_scheme_type);
    }

    fn amount(&self, sink: &mut Sink<'_>, stored_direction: bool) {
        sink.money("amount", self.base.signed_amount());
        sink.boolean("debit", self.base.is_debit());
        sink.decimal("stored_amount", self.base.amount);
        if stored_direction {
            sink.boolean("stored_debit_flag", self.base.debit_flag);
        }
    }

    fn currency(&self, sink: &mut Sink<'_>, currency: Option<&str>) {
        sink.text("currency", currency);
        sink.text("stored_currency", self.base.currency.as_deref());
    }

    fn payee(&self, sink: &mut Sink<'_>) {
        let base = self.base;
        sink.text("payee_name", base.counterparty_name.as_deref());
        sink.text("payee_iban", base.counterparty_account.as_deref());
        sink.text("payee_bic", base.counterparty_bank_code.as_deref());
        sink.text("payee_country", base.counterparty_country.as_deref());
        sink.text("payee_street", base.counterparty_street.as_deref());
        sink.text("payee_town", base.counterparty_town.as_deref());
        sink.text(
            "ultimate_payee_name",
            base.ultimate_counterparty_name.as_deref(),
        );
    }

    fn purpose_and_category(&self, sink: &mut Sink<'_>) {
        sink.text("purpose", self.base.purpose().as_deref());
        sink.category(self.base.category.as_deref());
    }

    fn sepa(&self, sink: &mut Sink<'_>) {
        let base = self.base;
        sink.text("end_to_end_id", base.end_to_end_id.as_deref());
        sink.text("payment_info_id", base.payment_info_id.as_deref());
        sink.text("mandate_id", base.mandate_id.as_deref());
        sink.raw_date("mandate_signed_date", base.mandate_signed_date);
        sink.text("creditor_id", base.creditor_id.as_deref());
        sink.text("purpose_code", base.purpose_code.as_deref());
        sink.text(
            "category_purpose_code",
            base.category_purpose_code.as_deref(),
        );
        sink.text(
            "local_instrument_code",
            base.local_instrument_code.as_deref(),
        );
        sink.text("sequence_type", base.sequence_type.as_deref());
        sink.text("order_id", base.order_id.as_deref());
    }

    fn foreign(&self, sink: &mut Sink<'_>) {
        let base = self.base;
        sink.name("cost_bearer", base.cost_bearer);
        sink.text(
            "intl_remittance_method",
            base.intl_remittance_method.as_deref(),
        );
        sink.text(
            "intl_remittance_instruction",
            base.intl_remittance_instruction.as_deref(),
        );
        sink.text(
            "regulatory_reporting_code",
            base.regulatory_reporting_code.as_deref(),
        );
        sink.text("payee_bank_name", base.counterparty_bank_name.as_deref());
        sink.text(
            "payee_bank_street",
            base.counterparty_bank_street.as_deref(),
        );
        sink.text("payee_bank_town", base.counterparty_bank_town.as_deref());
    }
}

/// Position within a split or batch as the schema defines it: 0 for a master
/// and for a record on its own, 1 and up for the parts.
fn batch_position(role: BatchRole, index_among_parts: u32) -> i64 {
    match role {
        BatchRole::Slave => i64::from(index_among_parts) + 1,
        BatchRole::Master | BatchRole::None => 0,
    }
}

/// A reference to an outbox order, where 0 and -1 both mean "none".
fn outbox(oid: i32) -> i32 {
    if oid == -1 {
        0
    } else {
        oid
    }
}

#[allow(clippy::too_many_lines)]
fn write_table(sink: &mut Sink<'_>, snapshot: &Snapshot) -> Result<(), Error> {
    match sink.table.name {
        table::ACCOUNTS => {
            for account in &snapshot.accounts {
                sink.record(&account.base);
                sink.name("kind", account.account_kind);
                sink.name("stored_kind", account.stored_account_kind);
                sink.text("name", account.name.as_deref());
                sink.text("iban", account.iban.as_deref());
                sink.text("bic", account.bic.as_deref());
                sink.text("bank_code", account.bank_code.as_deref());
                sink.text("account_number", account.account_number.as_deref());
                sink.text("country_code", account.country_code.as_deref());
                sink.text("currency", account.currency.as_deref());
                sink.text("holder_name", account.holder_name.as_deref());
                sink.text("holder_street", account.holder_street.as_deref());
                sink.text("holder_town", account.holder_town.as_deref());
                sink.text("description", account.description.as_deref());
                sink.reference("group_id", account.group_oid);
                sink.int("position", account.position);
                sink.date("opening_date", account.opening_date);
                sink.money("balance", account.statement_balance);
                sink.timestamp("balance_at", account.statement_balance_time);
                sink.money("online_balance", account.online_balance);
                sink.timestamp("online_balance_at", account.online_balance_time);
                sink.money("available_funds", account.available_funds);
                sink.money("overdraft_limit", account.overdraft_limit);
                sink.decimal("credit_interest_rate", account.credit_interest_rate);
                sink.decimal("debit_interest_rate", account.debit_interest_rate);
                sink.decimal("overdraft_interest_rate", account.overdraft_interest_rate);
                sink.timestamp("last_download_at", account.last_download_time);
                sink.timestamp("last_sync_at", account.last_sync_time);
                sink.name("flags", account.flags);
                sink.boolean("hidden", account.flags.contains(AccountFlags::HIDDEN));
                sink.boolean(
                    "exclude_from_totals",
                    account.flags.contains(AccountFlags::EXCLUDE_FROM_TOTALS),
                );
                sink.name("balance_flags", account.balance_flags);
                sink.text("creditor_id", account.creditor_id.as_deref());
                sink.text(
                    "reference_account_iban",
                    account.reference_account_iban.as_deref(),
                );
                sink.text("originator_name", account.originator_name.as_deref());
                sink.text("icon_name", account.icon_name.as_deref());
                sink.int("record_version", account.record_version);
                sink.end()?;
            }
        }
        table::ACCOUNT_GROUPS => {
            for group in &snapshot.account_groups {
                sink.record(&group.base);
                sink.int("position", group.position);
                sink.text("name", group.name.as_deref());
                sink.text("description", group.description.as_deref());
                sink.name("flags", group.flags);
                sink.boolean(
                    "exclude_from_totals",
                    group.flags.contains(GroupFlags::EXCLUDE_FROM_TOTALS),
                );
                sink.end()?;
            }
        }
        table::BOOKINGS => {
            for InAccount {
                account_id,
                currency,
                record: booking,
            } in &snapshot.bookings
            {
                let shared = PaymentColumns {
                    base: &booking.base,
                };
                shared.head(sink, *account_id);
                sink.raw_date("booking_date", booking.booking_date);
                sink.raw_date("value_date", booking.value_date);
                sink.name("status", booking.booking_status);
                sink.boolean("pending", booking.booking_status.raw() != 0);
                shared.amount(sink, false);
                shared.currency(sink, currency.as_deref());
                shared.payee(sink);
                shared.purpose_and_category(sink);
                sink.text("booking_text", booking.booking_text.as_deref());
                sink.text("notes", booking.notes.as_deref());
                sink.boolean("counted", booking.is_counted());
                sink.name("split_role", booking.effective_split());
                sink.reference("master_id", booking.effective_batch_oid());
                sink.int(
                    "batch_index",
                    batch_position(booking.batch_role(), booking.batch_index),
                );
                sink.name("flag", booking.flag);
                sink.boolean("is_read", booking.entry_bits.contains(EntryBits::READ));
                sink.name("entry_bits", booking.entry_bits);
                sink.name("transaction_code", booking.transaction_code);
                sink.int("gvc", booking.gvc);
                sink.int("gvc_extension", booking.gvc_extension);
                sink.text(
                    "swift_transaction_code",
                    booking.swift_transaction_code.as_deref(),
                );
                sink.text("primanota", booking.primanota.as_deref());
                sink.text("bank_reference", booking.bank_reference.as_deref());
                shared.sepa(sink);
                sink.date("transaction_date", booking.transaction_date);
                sink.date("analysis_date", booking.analysis_date);
                sink.money("vat_amount", booking.vat_amount);
                sink.name("vat_rate_type", booking.vat_rate_type);
                sink.money("instructed_amount", booking.instructed_amount);
                sink.text(
                    "instructed_currency",
                    booking.instructed_currency.as_deref(),
                );
                sink.money("charges_amount", booking.charges_amount);
                sink.text("charges_currency", booking.charges_currency.as_deref());
                sink.text("virtual_iban", booking.virtual_iban.as_deref());
                sink.text(
                    "debit_return_order_id",
                    booking.debit_return_order_id.as_deref(),
                );
                sink.raw_date("debit_return_date", booking.debit_return_date);
                sink.time("debit_return_time", booking.debit_return_time);
                shared.foreign(sink);
                sink.end()?;
            }
        }
        table::PAYMENTS => {
            for InAccount {
                account_id,
                currency,
                record: payment,
            } in &snapshot.payments
            {
                let shared = PaymentColumns {
                    base: &payment.base,
                };
                shared.head(sink, *account_id);
                shared.scheme(sink);
                shared.amount(sink, true);
                shared.currency(sink, currency.as_deref());
                sink.raw_date("scheduled_date", payment.base.scheduled_date);
                sink.time("scheduled_time", payment.base.scheduled_time);
                shared.payee(sink);
                shared.purpose_and_category(sink);
                shared.sepa(sink);
                sink.name("state", payment.state);
                sink.name("flags", payment.flags);
                sink.boolean("hidden_by_banking4", payment.hidden_by_banking4);
                sink.reference("master_id", payment.effective_batch_oid());
                sink.int(
                    "batch_index",
                    batch_position(payment.batch_role(), payment.batch_index),
                );
                sink.name("flag", payment.color_flag);
                sink.text("initiator_name", payment.initiator_name.as_deref());
                sink.name("batch_booking", payment.batch_booking);
                sink.text("import_id", payment.import_id.as_deref());
                sink.text("payment_reference", payment.payment_reference.as_deref());
                sink.reference("reconciled_booking_id", payment.reconciled_booking_oid);
                sink.reference("outbox_id", outbox(payment.outbox_oid));
                sink.name("sepa_status", payment.sepa_status);
                sink.text("sepa_status_code", payment.sepa_status_code.as_deref());
                sink.text("sepa_reason_code", payment.sepa_reason_code.as_deref());
                sink.text(
                    "sepa_additional_info",
                    payment.sepa_additional_info.as_deref(),
                );
                sink.name(
                    "payee_verification_choice",
                    payment.payee_verification_choice,
                );
                sink.text(
                    "payee_verification_json",
                    payment.payee_verification_json.as_deref(),
                );
                sink.reference(
                    "cost_bearer_account_id",
                    payment.base.cost_bearer_account_oid,
                );
                shared.foreign(sink);
                sink.end()?;
            }
        }
        table::STANDING_ORDERS => {
            for InAccount {
                account_id,
                currency,
                record: order,
            } in &snapshot.standing_orders
            {
                let shared = PaymentColumns { base: &order.base };
                shared.head(sink, *account_id);
                shared.scheme(sink);
                shared.amount(sink, true);
                shared.currency(sink, currency.as_deref());
                shared.payee(sink);
                shared.purpose_and_category(sink);
                shared.sepa(sink);
                sink.name("state", order.state);
                sink.boolean("is_local", order.state.contains(StandingOrderState::LOCAL));
                sink.raw_date("first_execution_date", order.first_execution_date);
                sink.raw_date("last_execution_date", order.last_execution_date);
                sink.raw_date("next_execution_date", order.next_execution_date);
                sink.raw_date(
                    "stored_next_execution_date",
                    order.stored_next_execution_date,
                );
                sink.name("period_unit", order.period_unit);
                sink.int("period_length", order.period_length);
                sink.int("execution_day", order.execution_day);
                sink.time("execution_time", order.execution_time);
                sink.reference("outbox_id", outbox(order.outbox_oid));
                shared.foreign(sink);
                sink.end()?;
            }
        }
        table::PAYEES => {
            for payee in &snapshot.payees {
                sink.record(&payee.base);
                sink.text("name", payee.name.as_deref());
                sink.text("iban", payee.iban.as_deref());
                sink.text("bic", payee.bic.as_deref());
                sink.text("account_number", payee.account_number.as_deref());
                sink.text("bank_code", payee.bank_code.as_deref());
                sink.text("street", payee.street.as_deref());
                sink.text("town", payee.town.as_deref());
                sink.text("country", payee.country.as_deref());
                sink.text("bank_name", payee.bank_name.as_deref());
                sink.text("bank_street", payee.bank_street.as_deref());
                sink.text("bank_town", payee.bank_town.as_deref());
                sink.text("purpose", payee.purpose.as_deref());
                sink.category(payee.category.as_deref());
                sink.money("amount", payee.amount);
                sink.text("currency", payee.currency.as_deref());
                sink.text("email", payee.email.as_deref());
                sink.text("customer_number", payee.customer_number.as_deref());
                sink.text("notes", payee.notes.as_deref());
                sink.name("kind", payee.payee_kind);
                sink.name("stored_kind", payee.stored_payee_kind);
                sink.name("flags", payee.payee_flags);
                sink.name("vop_status", payee.vop_status);
                sink.text("vop_matched_name", payee.vop_matched_name.as_deref());
                sink.text("mandate_id", payee.mandate_id.as_deref());
                sink.raw_date("mandate_signed_date", payee.mandate_signature_date);
                sink.raw_date("mandate_revocation_date", payee.mandate_revocation_date);
                sink.text(
                    "local_instrument_code",
                    payee.local_instrument_code.as_deref(),
                );
                sink.end()?;
            }
        }
        table::PAYERS => {
            for payer in &snapshot.payers {
                sink.record(&payer.base);
                sink.reference("payee_id", payer.party_oid);
                sink.text("name", payer.party_name.as_deref());
                sink.text("mandate_id", payer.mandate_id.as_deref());
                sink.raw_date("mandate_signed_date", payer.mandate_signature_date);
                sink.raw_date("mandate_revocation_date", payer.mandate_revocation_date);
                sink.text(
                    "local_instrument_code",
                    payer.local_instrument_code.as_deref(),
                );
                sink.text("notes", payer.notes.as_deref());
                sink.category(payer.category.as_deref());
                sink.end()?;
            }
        }
        table::CATEGORIES => {
            for loaded in &snapshot.categories {
                let category = &loaded.record;
                sink.record(&category.base);
                sink.text("full_name", category.full_name.as_deref());
                sink.text("category_key", category.key().as_deref());
                sink.text("name", category.name());
                sink.text("parent", category.parent());
                sink.text("parent_key", category.parent().map(category_key).as_deref());
                let depth = category
                    .full_name
                    .as_ref()
                    .and_then(|_| i64::try_from(category.depth()).ok());
                sink.put(
                    "depth",
                    Kind::BigInt,
                    depth.map_or(Value::Null, Value::BigInt),
                );
                sink.text("description", category.description.as_deref());
                // The role in Banking4, which is what the views go by; the
                // stored one follows at the end of the row.
                sink.name("special", loaded.special);
                sink.name("totals_bucket", category.totals_bucket);
                sink.name("budget_period", category.budget_period);
                sink.money("budget_amount", category.budget_amount);
                sink.decimal("vat_percent", category.vat_percent);
                sink.text("icon_name", category.icon_name.as_deref());
                sink.text("color_name", category.color_name.as_deref());
                sink.text("rules", category.rules.as_deref());
                sink.int("layout_version", category.layout_version);
                sink.text("stored_full_name", category.stored_full_name.as_deref());
                sink.boolean("hidden_by_banking4", loaded.hidden_by_banking4);
                sink.name("stored_special", category.special_category);
                sink.end()?;
            }
        }
        table::MANDATES => {
            for marked in &snapshot.mandates {
                let mandate = &marked.record;
                sink.record(&mandate.base);
                sink.text("creditor_id", mandate.creditor_id.as_deref());
                sink.text("mandate_id", mandate.mandate_id.as_deref());
                sink.text("creditor_name", mandate.creditor_name.as_deref());
                sink.category(mandate.category.as_deref());
                sink.name("flags", mandate.mandate_flags);
                sink.boolean(
                    "hidden",
                    mandate.mandate_flags.contains(MandateFlags::HIDDEN),
                );
                sink.boolean(
                    "revoked",
                    mandate.mandate_flags.contains(MandateFlags::REVOKED),
                );
                sink.timestamp("mandate_date", mandate.mandate_date);
                sink.boolean("hidden_by_banking4", marked.hidden_by_banking4);
                sink.end()?;
            }
        }
        table::EXCHANGE_RATES => {
            for marked in &snapshot.exchange_rates {
                let rate = &marked.record;
                sink.record(&rate.base);
                sink.text("base_currency", rate.base_currency.as_deref());
                sink.text("target_currency", rate.target_currency.as_deref());
                sink.decimal("rate", rate.quote_rate);
                sink.timestamp("quoted_at", rate.quote_time);
                sink.boolean("hidden_by_banking4", marked.hidden_by_banking4);
                sink.end()?;
            }
        }
        table::STOCK_INSTRUMENTS => {
            for instrument in &snapshot.stock_instruments {
                sink.record(&instrument.base);
                sink.text("isin", instrument.isin.as_deref());
                sink.text("stored_isin", instrument.stored_isin.as_deref());
                sink.text("name", instrument.name.as_deref());
                sink.text("price_currency", instrument.price_currency.as_deref());
                sink.text("splits", instrument.splits.as_deref());
                sink.end()?;
            }
        }
        table::STOCK_PRICES => {
            for owned in &snapshot.stock_prices {
                sink.int("instrument_id", owned.owner);
                sink.record(&owned.record.base);
                sink.raw_date("price_date", owned.record.date);
                sink.decimal("price", owned.record.price);
                sink.end()?;
            }
        }
        table::PORTFOLIO_ITEMS => {
            for owned in &snapshot.portfolio_items {
                let item = &owned.record;
                sink.int("account_id", owned.owner);
                sink.record(&item.base);
                sink.name("state", item.item_state);
                sink.boolean(
                    "deleted",
                    item.item_state.contains(PortfolioItemState::DELETED),
                );
                sink.text("isin", item.isin.as_deref());
                sink.text("stored_isin", item.stored_isin.as_deref());
                sink.text("name", item.name.as_deref());
                sink.decimal("quantity", item.quantity);
                sink.name("quantity_type", item.quantity_type);
                sink.text("quantity_currency", item.quantity_currency.as_deref());
                sink.name("price_type", item.price_type);
                sink.decimal("current_price", item.current_price);
                sink.text(
                    "current_price_currency",
                    item.current_price_currency.as_deref(),
                );
                sink.raw_date("current_price_date", item.current_price_date);
                sink.time("current_price_time", item.current_price_time);
                sink.decimal("purchase_price", item.purchase_price);
                sink.text(
                    "purchase_price_currency",
                    item.purchase_price_currency.as_deref(),
                );
                sink.raw_date("purchase_price_date", item.purchase_price_date);
                sink.decimal("interest_rate", item.interest_rate);
                sink.raw_date("maturity_date", item.maturity_date);
                sink.raw_date("blocked_until_date", item.blocked_until_date);
                sink.decimal("alert_lower_limit", item.alert_lower_limit);
                sink.decimal("alert_upper_limit", item.alert_upper_limit);
                sink.text("portfolio_key", item.portfolio_key.as_deref());
                sink.text("additional_info", Some(&item.additional_info.join("\n")));
                sink.end()?;
            }
        }
        table::PORTFOLIO_TRANSACTIONS => {
            for owned in &snapshot.portfolio_transactions {
                let transaction = &owned.record;
                sink.int("account_id", owned.owner);
                sink.record(&transaction.base);
                sink.text("transaction_id", transaction.transaction_id.as_deref());
                sink.name("state", transaction.transaction_state);
                sink.name("delivery_direction", transaction.delivery_direction);
                sink.name("activity", transaction.activity);
                sink.text("isin", transaction.isin.as_deref());
                sink.text("stored_isin", transaction.stored_isin.as_deref());
                sink.text("name", transaction.name.as_deref());
                sink.decimal("quantity", transaction.quantity);
                sink.name("quantity_type", transaction.quantity_type);
                sink.text(
                    "quantity_currency",
                    transaction.quantity_currency.as_deref(),
                );
                sink.timestamp("traded_at", transaction.trading_date);
                sink.decimal("trading_price", transaction.trading_price);
                sink.text("trading_currency", transaction.trading_currency.as_deref());
                sink.timestamp("settled_at", transaction.settlement_date);
                sink.money("settlement_amount", transaction.settlement_amount);
                sink.text(
                    "settlement_currency",
                    transaction.settlement_currency.as_deref(),
                );
                sink.boolean("interest_accrued", transaction.interest_accrued);
                sink.int("interest_days", transaction.interest_days);
                sink.money("interest_amount", transaction.interest_amount);
                sink.text(
                    "interest_currency",
                    transaction.interest_currency.as_deref(),
                );
                sink.boolean("is_read", transaction.is_read);
                sink.end()?;
            }
        }
        table::TAX_EXEMPTIONS => {
            for owned in &snapshot.tax_exemptions {
                let exemption = &owned.record;
                sink.int("account_id", owned.owner);
                sink.record(&exemption.base);
                sink.timestamp("valid_from", exemption.valid_from);
                sink.timestamp("valid_until", exemption.valid_until);
                sink.money("amount", exemption.amount);
                sink.money("used_amount", exemption.used_amount);
                sink.money("remaining_amount", exemption.remaining_amount);
                sink.end()?;
            }
        }
        table::ASSETS => {
            for owned in &snapshot.assets {
                let asset = &owned.record;
                sink.int("account_id", owned.owner);
                sink.record(&asset.base);
                sink.text("asset_id", asset.asset_id.as_deref());
                sink.money("amount", asset.signed_amount());
                sink.boolean("debit", asset.debit);
                sink.text("currency", asset.currency.as_deref());
                sink.raw_date("deposit_date", asset.deposit_date);
                sink.raw_date("expiry_date", asset.expiry_date);
                sink.decimal("interest_rate", asset.interest_rate);
                sink.text("interest_method", asset.interest_method.as_deref());
                sink.text("description", asset.description.as_deref());
                sink.end()?;
            }
        }
        other => {
            return Err(Error::new(format!(
                "internal error: the writer has no rows for the table {other}"
            )))
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap()
    }

    #[test]
    fn a_decimal_that_fits_is_stored_exactly() {
        assert_eq!(fit(dec("12.34"), schema::MONEY), Fitted::Exact(123_400));
        assert_eq!(fit(dec("-12.3400"), schema::MONEY), Fitted::Exact(-123_400));
        assert_eq!(fit(dec("0"), schema::MONEY), Fitted::Exact(0));
        assert_eq!(
            fit(dec("7"), schema::DECIMAL),
            Fitted::Exact(70_000_000_000)
        );
        // Trailing zeros beyond the scale lose nothing.
        assert_eq!(fit(dec("1.230000"), schema::MONEY), Fitted::Exact(12_300));
        // The largest and the smallest money value.
        assert_eq!(
            fit(dec("99999999999999.9999"), schema::MONEY),
            Fitted::Exact(999_999_999_999_999_999)
        );
        assert_eq!(
            fit(dec("-99999999999999.9999"), schema::MONEY),
            Fitted::Exact(-999_999_999_999_999_999)
        );
    }

    #[test]
    fn a_decimal_with_too_many_decimals_is_rounded_half_to_even() {
        assert_eq!(fit(dec("1.00005"), schema::MONEY), Fitted::Rounded(10_000));
        assert_eq!(fit(dec("1.00015"), schema::MONEY), Fitted::Rounded(10_002));
        assert_eq!(fit(dec("1.00025"), schema::MONEY), Fitted::Rounded(10_002));
        assert_eq!(
            fit(dec("-1.00015"), schema::MONEY),
            Fitted::Rounded(-10_002)
        );
        assert_eq!(fit(dec("1.000051"), schema::MONEY), Fitted::Rounded(10_001));
        assert_eq!(fit(dec("0.00004"), schema::MONEY), Fitted::Rounded(0));
        assert_eq!(
            fit(dec("0.12345678905"), schema::DECIMAL),
            Fitted::Rounded(1_234_567_890)
        );
        assert_eq!(
            fit(dec("0.12345678915"), schema::DECIMAL),
            Fitted::Rounded(1_234_567_892)
        );
    }

    #[test]
    fn a_decimal_with_too_many_digits_does_not_fit() {
        assert_eq!(fit(dec("100000000000000"), schema::MONEY), Fitted::TooLarge);
        assert_eq!(
            fit(dec("-100000000000000"), schema::MONEY),
            Fitted::TooLarge
        );
        // Rounding can carry into one digit too many.
        assert_eq!(
            fit(dec("99999999999999.99995"), schema::MONEY),
            Fitted::TooLarge
        );
        // The largest 96-bit value has 29 digits: too many for 18 before the
        // point, and scaling it would not fit 128 bits either.
        assert_eq!(fit(Decimal::MAX, schema::DECIMAL), Fitted::TooLarge);
        assert_eq!(fit(Decimal::MIN, schema::DECIMAL), Fitted::TooLarge);
        assert_eq!(
            fit(dec("999999999999999999.9999999999"), schema::DECIMAL),
            Fitted::Exact(9_999_999_999_999_999_999_999_999_999)
        );
        // 28 decimals, the most the format can store.
        assert_eq!(
            fit(dec("0.0000000000000000000000000001"), schema::DECIMAL),
            Fitted::Rounded(0)
        );
    }

    fn raw(year: i16, month: u8, day: u8) -> Option<RawDate> {
        Some(RawDate { year, month, day })
    }

    #[test]
    fn a_stored_date_becomes_a_calendar_date_or_nothing() {
        assert_eq!(calendar_date(None), Ok(None));
        assert_eq!(calendar_date(raw(1, 1, 1)), Ok(None));
        assert_eq!(calendar_date(raw(2024, 2, 29)), Ok(Date::new(2024, 2, 29)));
        assert_eq!(
            calendar_date(raw(9999, 12, 31)),
            Ok(Date::new(9999, 12, 31))
        );
        assert_eq!(calendar_date(raw(1, 1, 2)), Ok(Date::new(1, 1, 2)));
        for invalid in [
            raw(2023, 2, 29),
            raw(2024, 13, 1),
            raw(2024, 0, 0),
            raw(2024, 4, 31),
            raw(0, 1, 1),
            raw(-5, 6, 7),
            raw(2024, 6, 0),
        ] {
            assert_eq!(
                calendar_date(invalid),
                Err(Misfit::NoCalendarDate),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn each_kind_of_misfit_has_its_sentence() {
        assert_eq!(
            misfit_warning("bookings", "amount", Kind::Money, Misfit::Rounded, 3),
            "bookings.amount: 3 values with more than 4 decimals were rounded"
        );
        assert_eq!(
            misfit_warning("stock_prices", "price", Kind::Decimal, Misfit::Rounded, 1),
            "stock_prices.price: 1 value with more than 10 decimals was rounded"
        );
        assert_eq!(
            misfit_warning("bookings", "amount", Kind::Money, Misfit::TooLarge, 2),
            "bookings.amount: 2 values with more than 14 digits before the decimal point were \
             stored as NULL"
        );
        assert_eq!(
            misfit_warning("stock_prices", "price", Kind::Decimal, Misfit::TooLarge, 1),
            "stock_prices.price: 1 value with more than 18 digits before the decimal point was \
             stored as NULL"
        );
        assert_eq!(
            misfit_warning(
                "bookings",
                "value_date",
                Kind::Date,
                Misfit::NoCalendarDate,
                1
            ),
            "bookings.value_date: 1 stored value that is no calendar date was stored as NULL"
        );
        assert_eq!(
            misfit_warning(
                "bookings",
                "value_date",
                Kind::Date,
                Misfit::NoCalendarDate,
                12
            ),
            "bookings.value_date: 12 stored values that are no calendar date were stored as NULL"
        );
    }

    fn demo_snapshot() -> Snapshot {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault/demo.sub");
        let mut vault = crate::vault::Vault::open(path).unwrap();
        vault.unlock("demo").unwrap();
        crate::load::load(&vault).unwrap()
    }

    /// A snapshot with one booking whose values do not fit, a credit, and
    /// one whose value date and amount do not, a debit.
    fn misfitting_snapshot() -> Snapshot {
        let mut snapshot = demo_snapshot();
        snapshot.bookings.truncate(2);
        let odd = &mut snapshot.bookings[0].record;
        odd.base.amount = dec("12.345678");
        odd.vat_amount = dec("123456789012345678");
        odd.booking_date = raw(2024, 2, 30);
        odd.value_date = raw(2024, 0, 0);
        odd.base.mandate_signed_date = raw(1, 1, 1);
        let other = &mut snapshot.bookings[1].record;
        other.value_date = raw(2023, 13, 1);
        other.base.amount = dec("950.00015");
        snapshot
    }

    fn source<'a>(header: &'a Header, path: &'a Path) -> Source<'a> {
        Source {
            path,
            header,
            size: 279_040,
            checksum: "0123456789abcdef",
        }
    }

    fn demo_header() -> Header {
        Header::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/vault/demo.sub"
        ))
        .unwrap()
    }

    #[test]
    fn values_that_do_not_fit_are_rounded_or_null_and_counted_per_column() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.duckdb");
        let header = demo_header();
        let snapshot = misfitting_snapshot();
        let written = write(&snapshot, &source(&header, Path::new("/v.sub")), &target).unwrap();

        assert_eq!(
            written.warnings,
            [
                "bookings.booking_date: 1 stored value that is no calendar date was stored as \
                 NULL",
                "bookings.value_date: 2 stored values that are no calendar date were stored as \
                 NULL",
                "bookings.amount: 2 values with more than 4 decimals were rounded",
                "bookings.vat_amount: 1 value with more than 14 digits before the decimal point \
                 was stored as NULL",
            ]
        );
        // No value in any of them.
        for warning in &written.warnings {
            for value in ["12.3", "950", "1234567", "2024", "2023"] {
                assert!(!warning.contains(value), "{warning}");
            }
        }

        let connection = Connection::open(&target).unwrap();
        let (amount, stored, vat, booking_date, value_date): (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = connection
            .query_row(
                "select amount::varchar, stored_amount::varchar, vat_amount::varchar, \
                        booking_date::varchar, value_date::varchar \
                 from bookings order by rowid limit 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        // Half to even at the fourth decimal; the stored amount keeps all six.
        // The first booking of the demo vault is a credit: the sign is that
        // of the record, not just the digits.
        assert_eq!(amount, "12.3457");
        let signs: Vec<(String, bool, String)> = connection
            .prepare(
                "select amount::varchar, debit, stored_amount::varchar from bookings order by rowid",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            signs,
            [
                ("12.3457".to_owned(), false, "12.3456780000".to_owned()),
                // The second is a debit, rounded half to even away from the
                // odd digit, and negative.
                ("-950.0002".to_owned(), true, "950.0001500000".to_owned()),
            ]
        );
        assert_eq!(stored, "12.3456780000");
        assert_eq!((vat, booking_date, value_date), (None, None, None));

        let stored_warnings: Vec<String> = connection
            .prepare("select message from warnings order by rowid")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let expected: Vec<String> = snapshot
            .warnings
            .iter()
            .chain(&written.warnings)
            .cloned()
            .collect();
        assert_eq!(stored_warnings, expected);
    }

    #[test]
    fn the_demo_vault_fits_without_rounding_and_every_table_is_counted() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.duckdb");
        let header = demo_header();
        let snapshot = demo_snapshot();
        let written = write(&snapshot, &source(&header, Path::new("/v.sub")), &target).unwrap();
        assert_eq!(written.warnings, Vec::<String>::new());
        let names: Vec<&str> = written.rows.iter().map(|(name, _)| *name).collect();
        let expected: Vec<&str> = schema::tables()
            .iter()
            .map(|table| table.name)
            .filter(|name| *name != META_TABLE && *name != WARNINGS_TABLE)
            .collect();
        assert_eq!(names, expected);
        assert_eq!(written.rows[2], ("bookings", 634));

        // Nothing but the database is left in the directory.
        let left: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["out.duckdb"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let connection = Connection::open(&target).unwrap();
        let meta: BTreeMap<String, String> = connection
            .prepare("select key, value from meta")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let keys: Vec<&str> = meta.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "b4_version",
                "decrypted_at",
                "entries_never_read",
                "schema_version",
                "vault_checksum",
                "vault_created",
                "vault_id",
                "vault_modified",
                "vault_modified_stamp",
                "vault_path",
                "vault_size",
            ]
        );
        assert_eq!(meta["schema_version"], "1");
        assert_eq!(meta["vault_path"], "/v.sub");
        assert_eq!(meta["vault_size"], "279040");
        assert_eq!(meta["vault_checksum"], "0123456789abcdef");
        assert_eq!(meta["entries_never_read"], "9");
        assert_eq!(meta["vault_id"], header.file_guid().to_string());
        assert_eq!(
            meta["vault_modified_stamp"],
            header.modified_at().raw().to_string()
        );
        assert_eq!(meta["decrypted_at"].len(), "2026-01-01T00:00:00Z".len());
    }

    #[test]
    fn a_repeated_special_role_and_a_paypal_currency_are_exported_as_banking4_has_them() {
        use crate::model::masterdata::SpecialCategory;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.duckdb");
        let header = demo_header();
        let mut snapshot = demo_snapshot();
        snapshot.bookings.truncate(2);

        // Two categories store the role of the reclassification category.
        // The loader gives it to the first; the second is an ordinary one.
        let reclassification = SpecialCategory::from_raw(1);
        snapshot.categories.retain(|loaded| {
            loaded.record.special_category == SpecialCategory::None
                && loaded.record.full_name.is_some()
        });
        snapshot.categories.truncate(3);
        for (index, special) in [(0, reclassification), (1, SpecialCategory::None)] {
            let loaded = &mut snapshot.categories[index];
            loaded.record.special_category = reclassification;
            loaded.special = special;
        }
        // One booking under each of them.
        for index in 0..2 {
            let name = snapshot.categories[index].record.full_name.clone();
            snapshot.bookings[index].record.base.category = name;
        }
        // The first booking is in a PayPal account kept in dollars and
        // stores euros.
        snapshot.bookings[0].currency = Some("USD".to_owned());
        assert_eq!(
            snapshot.bookings[0].record.base.currency.as_deref(),
            Some("EUR")
        );

        write(&snapshot, &source(&header, Path::new("/v.sub")), &target).unwrap();
        let connection = Connection::open(&target).unwrap();
        let pairs = |query: &str| -> Vec<(String, String)> {
            connection
                .prepare(query)
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let text = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        assert_eq!(
            pairs("select special, stored_special from categories order by rowid"),
            [
                text("reclassification", "reclassification"),
                text("none", "reclassification"),
                text("none", "none"),
            ]
        );
        // The view goes by the role in Banking4: only the first booking is
        // a transfer between own accounts.
        assert_eq!(
            pairs(
                "select v.category_special, v.currency from bookings_view v \
                 join bookings b using (id) order by b.rowid"
            ),
            [text("reclassification", "USD"), text("none", "EUR")]
        );
        assert_eq!(
            pairs("select currency, stored_currency from bookings order by rowid"),
            [text("USD", "EUR"), text("EUR", "EUR")]
        );
    }

    #[test]
    fn a_target_that_is_not_a_database_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("v.sub");
        fs::write(&vault, b"SUB\x01 and so on").unwrap();
        for content in [&b""[..], b"hello", b"0123456789abcdefghij"] {
            let target = dir.path().join("notes.txt");
            fs::write(&target, content).unwrap();
            let error = check_target(&target, &vault).unwrap_err().to_string();
            assert!(
                error.contains("is not a DuckDB database: refusing to replace it"),
                "{error}"
            );
            assert_eq!(fs::read(&target).unwrap(), content);
        }
        let error = check_target(dir.path(), &vault).unwrap_err().to_string();
        assert!(error.contains("refusing to replace it"), "{error}");

        // A database and a free name are fine.
        let database = dir.path().join("old.duckdb");
        fs::write(&database, b"\0\0\0\0\0\0\0\0DUCK rest").unwrap();
        check_target(&database, &vault).unwrap();
        check_target(&dir.path().join("new.duckdb"), &vault).unwrap();
    }

    #[test]
    fn the_vault_itself_is_refused_as_target_through_any_link() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("v.sub");
        // Even a vault that happened to look like a database.
        fs::write(&vault, b"\0\0\0\0\0\0\0\0DUCK rest").unwrap();
        let hard = dir.path().join("hard.duckdb");
        fs::hard_link(&vault, &hard).unwrap();
        let mut targets = vec![vault.clone(), dir.path().join("./v.sub"), hard];
        #[cfg(unix)]
        {
            let soft = dir.path().join("soft.duckdb");
            std::os::unix::fs::symlink(&vault, &soft).unwrap();
            targets.push(soft);
        }
        for target in targets {
            let error = check_target(&target, &vault).unwrap_err().to_string();
            assert!(error.ends_with("is the vault itself: the database needs a file of its own"));
        }

        // A target whose write-ahead log would have the vault's name: the
        // log of a replaced database is moved away and deleted, and that
        // must not happen to a vault. Refused by `write` too, with the vault
        // left as it is.
        let vault = dir.path().join("money.wal");
        fs::write(&vault, b"SUB\x01 a vault with an odd name").unwrap();
        let mut targets = vec![dir.path().join("money"), dir.path().join("./money")];
        #[cfg(unix)]
        {
            // Through a link: the log of `linked` is `linked.wal`.
            std::os::unix::fs::symlink(&vault, dir.path().join("linked.wal")).unwrap();
            targets.push(dir.path().join("linked"));
        }
        let header = demo_header();
        for target in targets {
            let error = check_target(&target, &vault).unwrap_err().to_string();
            assert!(
                error.ends_with("would use that name for its write-ahead log: name another file"),
                "{error}"
            );
            let source = Source {
                path: &vault,
                header: &header,
                size: 0,
                checksum: "",
            };
            assert!(write(&Snapshot::default(), &source, &target).is_err());
            assert!(!target.exists());
            assert_eq!(
                fs::read(&vault).unwrap(),
                b"SUB\x01 a vault with an odd name"
            );
        }
    }

    #[test]
    fn a_failed_write_leaves_the_target_and_its_directory_as_they_were() {
        let dir = tempfile::tempdir().unwrap();
        let header = demo_header();
        let snapshot = Snapshot::default();

        // The target's directory does not exist.
        let target = dir.path().join("missing").join("out.duckdb");
        let error = write(&snapshot, &source(&header, Path::new("/v.sub")), &target).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot create a working directory"),
            "{error}"
        );

        // The target is a directory: refused before anything is created.
        let target = dir.path().join("taken");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep"), b"x").unwrap();
        let error = write(&snapshot, &source(&header, Path::new("/v.sub")), &target).unwrap_err();
        assert!(
            error.to_string().ends_with(
                "exists and is not a DuckDB database: refusing to replace it; name \
                            another file or remove it first"
            ),
            "{error}"
        );

        let mut left: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["taken"]);
        assert_eq!(fs::read(target.join("keep")).unwrap(), b"x");
    }

    /// A database at `path` with one table that is in its file and one that
    /// is only in its write-ahead log, as a program leaves it that ends
    /// without a checkpoint. Returns the log.
    fn database_with_a_log(path: &Path) -> Vec<u8> {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "create table in_file(x integer); insert into in_file values (1); checkpoint; \
                 pragma disable_checkpoint_on_shutdown; set checkpoint_threshold = '10GB'; \
                 create table in_log(y integer); insert into in_log values (42);",
            )
            .unwrap();
        drop(connection);
        let log = fs::read(log_of(path)).unwrap();
        assert!(!log.is_empty());
        log
    }

    fn tables_of(path: &Path) -> Vec<String> {
        let config = Config::default()
            .access_mode(duckdb::AccessMode::ReadOnly)
            .unwrap();
        let connection = Connection::open_with_flags(path, config).unwrap();
        let tables = connection
            .prepare(
                "select table_name from information_schema.tables where table_type = 'BASE TABLE'",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        tables
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_log_of_a_replaced_database_does_not_reach_the_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.duckdb");
        let header = demo_header();
        database_with_a_log(&target);
        assert_eq!(names_in(dir.path()), ["out.duckdb", "out.duckdb.wal"]);
        // The log is live: opened as it is, the old database has both.
        assert!(tables_of(&target).contains(&"in_log".to_owned()));

        write(
            &Snapshot::default(),
            &source(&header, Path::new("/v.sub")),
            &target,
        )
        .unwrap();
        assert_eq!(names_in(dir.path()), ["out.duckdb"]);
        let tables = tables_of(&target);
        assert!(tables.contains(&"bookings".to_owned()), "{tables:?}");
        assert!(!tables.contains(&"in_log".to_owned()), "{tables:?}");
        assert!(!tables.contains(&"in_file".to_owned()), "{tables:?}");
        // Opening it did not bring a log back.
        assert_eq!(names_in(dir.path()), ["out.duckdb"]);

        // A log without a database next to a free name: the same.
        let free = dir.path().join("free.duckdb");
        let log = database_with_a_log(&dir.path().join("other.duckdb"));
        fs::write(log_of(&free), &log).unwrap();
        write(
            &Snapshot::default(),
            &source(&header, Path::new("/v.sub")),
            &free,
        )
        .unwrap();
        assert!(!log_of(&free).exists());
        assert!(!tables_of(&free).contains(&"in_log".to_owned()));
    }

    #[test]
    fn a_failed_replacement_leaves_the_old_database_with_its_log() {
        let dir = tempfile::tempdir().unwrap();

        // The rename onto the target fails: the target is a directory that
        // is not empty. A log next to it is back where it was, byte for
        // byte, and nothing else is left.
        let target = dir.path().join("taken");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep"), b"x").unwrap();
        fs::write(log_of(&target), b"the log of something").unwrap();
        let mut work = WorkDir::create(&target).unwrap();
        let new = work.database();
        fs::write(&new, b"new").unwrap();
        let error = replace(&new, &target, &mut work).unwrap_err().to_string();
        assert!(error.starts_with("cannot put the database at "), "{error}");
        assert!(!work.keep);
        drop(work);
        assert_eq!(names_in(dir.path()), ["taken", "taken.wal"]);
        assert_eq!(fs::read(log_of(&target)).unwrap(), b"the log of something");
        assert_eq!(fs::read(target.join("keep")).unwrap(), b"x");

        // The same with a real database and its log, at the level of the
        // replacement: the new file is missing, so the rename fails.
        let target = dir.path().join("old.duckdb");
        let log = database_with_a_log(&target);
        let before = fs::read(&target).unwrap();
        let mut work = WorkDir::create(&target).unwrap();
        let missing = work.database();
        let error = replace(&missing, &target, &mut work).unwrap_err();
        assert!(
            error.to_string().starts_with("cannot put the database at "),
            "{error}"
        );
        assert!(!work.keep);
        drop(work);
        assert_eq!(fs::read(&target).unwrap(), before);
        assert_eq!(fs::read(log_of(&target)).unwrap(), log);
        assert_eq!(
            names_in(dir.path()),
            ["old.duckdb", "old.duckdb.wal", "taken", "taken.wal"]
        );
        // And it still is the database it was, log and all.
        let tables = tables_of(&target);
        assert!(tables.contains(&"in_log".to_owned()), "{tables:?}");
    }

    #[test]
    fn a_log_that_cannot_be_moved_stops_the_replacement_and_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("old.duckdb");
        let log = database_with_a_log(&target);
        let before = fs::read(&target).unwrap();
        let new = dir.path().join("new.duckdb");
        fs::write(&new, b"new").unwrap();

        // The working directory is gone, so the log has nowhere to go.
        let mut work = WorkDir {
            path: dir.path().join("gone"),
            keep: false,
        };
        let error = replace(&new, &target, &mut work).unwrap_err().to_string();
        let name = log_of(&target).display().to_string();
        assert!(
            error.starts_with(&format!(
                "cannot move the write-ahead log {name} out of the way; nothing was replaced, \
                 delete that file if the database it belongs to is not needed and decrypt again: "
            )),
            "{error}"
        );
        // Nothing was replaced: the old database, its log, and the new file
        // where it was.
        assert_eq!(fs::read(&target).unwrap(), before);
        assert_eq!(fs::read(log_of(&target)).unwrap(), log);
        assert_eq!(fs::read(&new).unwrap(), b"new");
    }

    #[test]
    fn a_log_that_is_a_regular_file_goes_and_nothing_else_does() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.duckdb");
        let new = dir.path().join("new.duckdb");
        fs::write(&new, b"new").unwrap();
        let mut work = WorkDir::create(&target).unwrap();
        replace(&new, &target, &mut work).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");

        // A regular file with the name of the log is deleted at once, not
        // when the working directory goes.
        fs::write(&new, b"newer").unwrap();
        fs::write(log_of(&target), b"a log").unwrap();
        replace(&new, &target, &mut work).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"newer");
        assert!(!log_of(&target).exists());
        assert_eq!(names_in(&work.path), Vec::<String>::new());
        drop(work);
        assert_eq!(names_in(dir.path()), ["out.duckdb"]);
    }

    /// With something that is no log at the name of the log of `out.duckdb`
    /// in `dir`: neither [`check_target`] nor [`replace`] nor [`write()`]
    /// touches it, and the target stays as it was.
    fn assert_log_name_is_left_alone(dir: &Path, what: &str) {
        let target = dir.join("out.duckdb");
        let log = log_of(&target);
        let vault = dir.join("v.sub");
        fs::write(&vault, b"SUB\x01 and so on").unwrap();
        fs::write(&target, b"\0\0\0\0\0\0\0\0DUCK the old one").unwrap();
        let before = names_in(dir);
        let expected = format!(
            "{} is {what}, where the write-ahead log of a database at {} belongs: b4 will not \
             replace a database while that entry is in the way, and does not touch it; move it \
             away or name another file",
            log.display(),
            target.display()
        );

        assert_eq!(
            check_target(&target, &vault).unwrap_err().to_string(),
            expected
        );
        // Without a database at the target as well.
        let free = dir.join("free");
        fs::rename(&log, log_of(&free)).unwrap();
        let error = check_target(&free, &vault).unwrap_err().to_string();
        assert!(error.contains("b4 will not replace a database"), "{error}");
        fs::rename(log_of(&free), &log).unwrap();

        let header = demo_header();
        let error = write(&Snapshot::default(), &source(&header, &vault), &target).unwrap_err();
        assert_eq!(error.to_string(), expected);

        // The replacement on its own, as if the entry had appeared while the
        // database was written.
        let mut work = WorkDir::create(&target).unwrap();
        let new = work.database();
        fs::write(&new, b"new").unwrap();
        let error = replace(&new, &target, &mut work).unwrap_err();
        assert_eq!(error.to_string(), expected);
        assert!(!work.keep);
        drop(work);

        assert_eq!(names_in(dir), before);
        assert_eq!(
            fs::read(&target).unwrap(),
            b"\0\0\0\0\0\0\0\0DUCK the old one"
        );
    }

    #[test]
    fn a_directory_with_the_name_of_the_log_is_refused_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("out.duckdb.wal");
        fs::create_dir(&log).unwrap();
        fs::write(log.join("inside"), b"x").unwrap();
        assert_log_name_is_left_alone(dir.path(), "a directory");
        assert_eq!(names_in(&log), ["inside"]);
        assert_eq!(fs::read(log.join("inside")).unwrap(), b"x");

        // An empty one as well.
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("out.duckdb.wal")).unwrap();
        assert_log_name_is_left_alone(dir.path(), "a directory");
        assert!(dir.path().join("out.duckdb.wal").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_or_a_special_file_with_the_name_of_the_log_is_refused_and_kept() {
        use std::os::unix::fs::symlink;

        // A link to a file, to a directory, and to nothing.
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("file");
        fs::write(&file, b"someone's").unwrap();
        let log = dir.path().join("out.duckdb.wal");
        symlink(&file, &log).unwrap();
        assert_log_name_is_left_alone(dir.path(), "a symbolic link");
        assert_eq!(fs::read_link(&log).unwrap(), file);
        assert_eq!(fs::read(&file).unwrap(), b"someone's");

        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("out.duckdb.wal");
        fs::write(outside.path().join("inside"), b"x").unwrap();
        symlink(outside.path(), &log).unwrap();
        assert_log_name_is_left_alone(dir.path(), "a symbolic link");
        assert_eq!(fs::read_link(&log).unwrap(), outside.path());
        assert_eq!(names_in(outside.path()), ["file", "inside"]);

        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("out.duckdb.wal");
        symlink(dir.path().join("nothing"), &log).unwrap();
        assert_log_name_is_left_alone(dir.path(), "a symbolic link");
        assert!(fs::symlink_metadata(&log).unwrap().is_symlink());

        // A socket.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("out.duckdb.wal");
        let _listener = std::os::unix::net::UnixListener::bind(&log).unwrap();
        assert_log_name_is_left_alone(dir.path(), "not a regular file");
    }

    #[test]
    fn the_working_directory_does_not_take_what_is_not_a_log_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.duckdb");

        // Should a directory ever be where the log moved aside is kept, the
        // working directory is left as it is.
        let work = WorkDir::create(&target).unwrap();
        let path = work.path.clone();
        fs::create_dir(path.join(ASIDE_LOG)).unwrap();
        fs::write(path.join(ASIDE_LOG).join("inside"), b"x").unwrap();
        fs::write(work.database(), b"new").unwrap();
        drop(work);
        assert_eq!(fs::read(path.join(ASIDE_LOG).join("inside")).unwrap(), b"x");

        // A regular file there goes with it, and so does b4's own.
        let work = WorkDir::create(&target).unwrap();
        let path = work.path.clone();
        fs::write(path.join(ASIDE_LOG), b"a log").unwrap();
        fs::write(work.database(), b"new").unwrap();
        fs::create_dir(path.join("b4.duckdb.tmp")).unwrap();
        drop(work);
        assert!(!path.exists());
    }

    #[test]
    fn a_row_in_the_wrong_order_fails_instead_of_shifting_columns() {
        let connection = Connection::open_in_memory().unwrap();
        schema::create(&connection).unwrap();
        let mut sink = Sink::new(&connection, find(table::ACCOUNT_GROUPS).unwrap()).unwrap();
        sink.int("id", 1);
        sink.text("name", Some("x"));
        let error = sink.end().unwrap_err().to_string();
        assert_eq!(
            error,
            "internal error: value 1 of a row of account_groups is name (VARCHAR), the schema \
             has created_at there"
        );

        let mut sink = Sink::new(&connection, find(WARNINGS_TABLE).unwrap()).unwrap();
        let error = sink.end().unwrap_err().to_string();
        assert_eq!(
            error,
            "internal error: a row of warnings has 0 values, the schema has 1 columns"
        );
    }

    #[test]
    fn an_empty_text_is_null_and_references_to_nothing_are_null() {
        let connection = Connection::open_in_memory().unwrap();
        schema::create(&connection).unwrap();
        let mut sink = Sink::new(&connection, find(table::PAYERS).unwrap()).unwrap();
        sink.record(&RecordBase {
            oid: 5,
            update_stamp: 0,
        });
        sink.reference("payee_id", 0);
        sink.text("name", Some(""));
        sink.text("mandate_id", None);
        sink.raw_date("mandate_signed_date", None);
        sink.raw_date("mandate_revocation_date", None);
        sink.text("local_instrument_code", Some("CORE"));
        sink.text("notes", Some("a\r\n\r\nb"));
        sink.category(Some("Wohnen:Miete"));
        sink.end().unwrap();
        sink.finish().unwrap();
        let row: (
            i64,
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            String,
            String,
        ) = connection
            .query_row(
                "select id, created_at::varchar, updated_at::varchar, payee_id, name, notes, \
                            category_key from payers",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            row,
            (
                5,
                "2000-01-01 00:00:05".to_owned(),
                // S7: stamp 0 is "unknown".
                None,
                None,
                None,
                // S2: free text verbatim.
                "a\r\n\r\nb".to_owned(),
                "WOHNEN:MIETE".to_owned(),
            )
        );
    }

    #[test]
    fn parts_of_a_group_are_numbered_from_one() {
        assert_eq!(batch_position(BatchRole::None, 0), 0);
        assert_eq!(batch_position(BatchRole::Master, 0), 0);
        assert_eq!(batch_position(BatchRole::Slave, 0), 1);
        assert_eq!(batch_position(BatchRole::Slave, 2), 3);
        assert_eq!(outbox(-1), 0);
        assert_eq!(outbox(7), 7);
    }
}
