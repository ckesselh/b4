//! Read-only, sandboxed access to a decrypted database.
//!
//! Every query command goes through [`Database`]:
//!
//! ```ignore
//! let db = Database::open(&globals.db)?;
//! let table = db.query_with(
//!     "select id, name, balance, currency from accounts where id = ?",
//!     &[Param::from(7_i64)],
//!     &[
//!         Hint::new("name").title("Account"),
//!         Hint::new("balance").money("currency"),
//!     ],
//! )?;
//! output::print_table(&table, globals)?;
//! ```
//!
//! # The sandbox
//!
//! A query handed to b4, by a person or by an agent, can only read this one
//! database. Three settings make that boundary, and all three are in place
//! before [`Database::open`] returns, so before any SQL from outside runs:
//!
//! 1. **Read-only.** The database is opened with `access_mode = read_only`:
//!    no statement can change it. It also means that any number of processes
//!    can query the same file at once.
//! 2. **No file system.** After the database file is open, the connection
//!    runs `SET disabled_filesystems = 'LocalFileSystem'`. DuckDB then has no
//!    file system at all: `read_text`, `read_blob`, `read_csv`, `glob`,
//!    `COPY ... TO` and `FROM`, `ATTACH`, `EXPORT`, `INSTALL` and profile or
//!    log files all fail with "File system LocalFileSystem has been disabled
//!    by configuration". The open database keeps its file handle and is read
//!    as before. `enable_external_access = false` and the extension
//!    auto-install and auto-load switches are set at open as well, but that
//!    setting alone is not enough: with it DuckDB still allows the paths it
//!    considers its own, which are the database file, `<database>.wal` and
//!    its siblings, and everything under `<database>.tmp`, a name that
//!    someone else can plant as a symbolic link.
//! 3. **Locked configuration.** `SET lock_configuration = true` is the last
//!    statement of the setup. No `SET`, `RESET` or `PRAGMA name = value` can
//!    change a setting afterwards, the two above included.
//!
//! The lock cannot be part of the configuration the database is opened
//! with, because the file system has to be disabled after the file is open,
//! and that is a setting too.
//!
//! ## Memory
//!
//! DuckDB may use at most [`MEMORY_LIMIT_BYTES`] (1 GiB), or its own default
//! of 80 % of the machine's memory where that is less; the limit is set
//! before the lock. One gibibyte is far more than a query over a bank vault
//! needs (a scan streams, and a 1.3 GB table was scanned under 256 MiB) and
//! little enough for a small machine or a container. A query that needs more
//! cannot spill to disk, because there is no file system: it fails, and
//! DuckDB words that failure like a refused file access ("File system
//! LocalFileSystem has been disabled by configuration"), not as "out of
//! memory". [`is_file_system_refusal`] recognises the message so that a
//! command can say both. Nothing is created next to the database, not even
//! the empty `<database>.tmp` directory.
//!
//! The limit covers the memory DuckDB accounts for, which is what queries
//! over tables use: scans, sorts, joins, aggregates, the result. It is not a
//! hard cap on the process. A single huge value that a function builds
//! (`repeat('x', 2000000000)`) is not counted, and b4's own copy of a chunk
//! comes on top.
//!
//! ## Switches that are not settings
//!
//! DuckDB has switches that are functions, not settings, and the lock does
//! not cover them: `PRAGMA enable_profiling`, `CALL enable_logging(...)`,
//! `PRAGMA enable_progress_bar` and their relatives ([`REFUSED_SWITCHES`]).
//! They cannot reach a file, but they change the process the query runs in:
//! a profile or a log printed to standard output between the rows, or every
//! statement run several times for verification. [`Database`] refuses a
//! statement whose text names one of them, before it reaches DuckDB.
//!
//! That check reads the text and nothing else: it is hygiene, not part of
//! the boundary. It is conservative (the name is found in any case of
//! letters, inside quoted identifiers, and also inside a string or a
//! comment, where it would be harmless), and the table function `query(...)`,
//! which runs SQL text computed by the query itself, is refused for the same
//! reason, as is `write_log(...)`, which writes into the engine's log
//! ([`REFUSED_CALLS`]). Whatever might still get past it can spoil the output
//! of its own process and nothing more: the boundary is the three settings
//! above.
//!
//! # Values
//!
//! [`Database::query`] maps every DuckDB type onto the output model exactly;
//! the table is in the `value` module. Values are bound as parameters
//! ([`Param`]), never spliced into the SQL text. Names cannot be bound: where
//! a table or column name goes into a statement it comes from the catalog of
//! the database and is quoted with [`quote_identifier`].

mod value;

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use duckdb::core::LogicalTypeId;
use duckdb::types::Value;
use duckdb::{AccessMode, Config, Connection};
use rust_decimal::Decimal;

use super::{meta_key, META_TABLE, SCHEMA_VERSION, WARNINGS_TABLE};
use crate::error::{Context, Error};
use crate::output::{Cell, Column, Date, Kind, Table};

/// Rows handed to the callback of [`Database::for_each_chunk`] at a time
/// unless the caller asks for another number.
pub const CHUNK_ROWS: usize = 1024;

/// Where "DUCK" stands in the first block of every DuckDB database file.
const MAGIC_OFFSET: usize = 8;
const MAGIC: &[u8; 4] = b"DUCK";

/// The most memory DuckDB may use for a query: 1 GiB, or DuckDB's default
/// (80 % of the machine's memory) where that is less. See the module
/// documentation for the choice.
pub const MEMORY_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;

/// What DuckDB says when a statement needs the file system, which
/// [`Database::open`] disables.
const NO_FILE_SYSTEM: &str = "File system LocalFileSystem has been disabled by configuration";

/// The pragmas and table functions of DuckDB that switch a facility of the
/// engine on or off without being settings, so that `lock_configuration` does
/// not stop them: profiling, logging, the progress bar, the verification of
/// statements, the optimizer, the object cache and the checkpoint at
/// shutdown. A statement that names one is refused.
///
/// The list is every pragma and table function of DuckDB 1.5 whose name
/// starts with `enable_`, `disable_` or `verify_`, and `truncate_duckdb_logs`;
/// a test compares it with the function catalogue, so that a new DuckDB with
/// another switch fails the test. None of them is needed to read data.
pub const REFUSED_SWITCHES: &[&str] = &[
    // The profile is printed to standard output after every statement.
    "enable_profiling",
    "enable_profile",
    "disable_profiling",
    "disable_profile",
    // The log can go to standard output or to a file.
    "enable_logging",
    "disable_logging",
    "truncate_duckdb_logs",
    // The progress bar is drawn on the terminal.
    "enable_progress_bar",
    "disable_progress_bar",
    "enable_print_progress_bar",
    "disable_print_progress_bar",
    // Debugging aids that run every statement several times.
    "enable_verification",
    "disable_verification",
    "verify_external",
    "disable_verify_external",
    "verify_fetch_row",
    "disable_verify_fetch_row",
    "verify_parallelism",
    "disable_verify_parallelism",
    "verify_serializer",
    "disable_verify_serializer",
    // Switches of the engine with nothing to gain for a query.
    "enable_optimizer",
    "disable_optimizer",
    "enable_object_cache",
    "disable_object_cache",
    "enable_checkpoint_on_shutdown",
    "disable_checkpoint_on_shutdown",
];

/// The table function that runs SQL text computed by the query itself, which
/// the check of [`REFUSED_SWITCHES`] cannot read. Refused as a call only:
/// `query` remains a usable name for a column.
const DYNAMIC_SQL_FUNCTION: &str = "query";

/// The functions a statement must not call, each with what it is and why it
/// is refused. A name here is refused only where an opening parenthesis
/// follows it ([`is_called`]), so that it remains a usable name for a column.
///
/// `query` is the only function of DuckDB 1.5 that runs SQL text: no other
/// table function, pragma or scalar function takes a statement, or the name
/// of a table function, and executes it. `query_table` is not refused: it
/// takes names and looks each up as a table or view of the catalog, so it
/// reaches what `FROM name` reaches and no function. `json_execute_serialized_sql`
/// and `sql_auto_complete` belong to extensions that are not in this build
/// and cannot be loaded.
const REFUSED_CALLS: &[(&str, &str, &str)] = &[
    (
        DYNAMIC_SQL_FUNCTION,
        "the table function",
        "it runs SQL text that is computed by the query, which b4 cannot check; write the \
         statement out instead",
    ),
    // With logging on, its text goes to wherever the log goes, as it is.
    (
        "write_log",
        "the function",
        "it writes a text of the query's choosing into the log of the database engine, and a \
         query has nothing to log",
    ),
];

/// What DuckDB says when the sandbox refuses a statement. Each is the
/// message of one of the settings [`Database::open`] applies.
const REFUSALS: &[&str] = &[
    // disabled_filesystems = 'LocalFileSystem'
    NO_FILE_SYSTEM,
    // enable_external_access = false
    "file system operations are disabled by configuration",
    "Loading external extensions is disabled through configuration",
    // access_mode = read_only
    "read-only mode",
    // lock_configuration = true
    "the configuration has been locked",
];

/// A decrypted database, open read-only and sandboxed.
pub struct Database {
    connection: Connection,
    path: PathBuf,
    meta: BTreeMap<String, String>,
}

impl fmt::Debug for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// A table or a view of the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    /// The name, as the catalog spells it.
    pub name: String,
    /// Whether it is a view.
    pub is_view: bool,
    /// The description stored with `COMMENT ON TABLE` / `COMMENT ON VIEW`.
    pub description: Option<String>,
}

/// A column of a table or a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    /// The name, as the catalog spells it.
    pub name: String,
    /// The SQL type, such as `DECIMAL(18,4)`.
    pub data_type: String,
    /// The description stored with `COMMENT ON COLUMN`.
    pub description: Option<String>,
}

/// A value bound to a `?` or `$n` placeholder of a query.
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    /// SQL `NULL`.
    Null,
    /// A `VARCHAR`.
    Text(String),
    /// A `BIGINT`.
    Integer(i64),
    /// A `DOUBLE`.
    Float(f64),
    /// A `BOOLEAN`.
    Boolean(bool),
    /// An exact `DECIMAL`.
    Decimal(Decimal),
    /// A `DATE`.
    Date(Date),
}

impl From<&str> for Param {
    fn from(value: &str) -> Param {
        Param::Text(value.to_owned())
    }
}

impl From<String> for Param {
    fn from(value: String) -> Param {
        Param::Text(value)
    }
}

impl From<&String> for Param {
    fn from(value: &String) -> Param {
        Param::Text(value.clone())
    }
}

impl From<i64> for Param {
    fn from(value: i64) -> Param {
        Param::Integer(value)
    }
}

impl From<f64> for Param {
    fn from(value: f64) -> Param {
        Param::Float(value)
    }
}

impl From<bool> for Param {
    fn from(value: bool) -> Param {
        Param::Boolean(value)
    }
}

impl From<Decimal> for Param {
    fn from(value: Decimal) -> Param {
        Param::Decimal(value)
    }
}

impl From<Date> for Param {
    fn from(value: Date) -> Param {
        Param::Date(value)
    }
}

impl<T: Into<Param>> From<Option<T>> for Param {
    fn from(value: Option<T>) -> Param {
        value.map_or(Param::Null, Into::into)
    }
}

impl Param {
    /// The value DuckDB binds for this parameter.
    fn to_value(&self) -> Result<Value, Error> {
        Ok(match self {
            Param::Null => Value::Null,
            Param::Text(text) => Value::Text(text.clone()),
            Param::Integer(value) => Value::BigInt(*value),
            Param::Float(value) => Value::Double(*value),
            Param::Boolean(value) => Value::Boolean(*value),
            Param::Decimal(value) => {
                // A 96-bit decimal always fits DuckDB's widest DECIMAL.
                let scale = u8::try_from(value.scale()).unwrap_or(u8::MAX);
                let decimal = duckdb::types::Decimal::new(38, scale, value.mantissa())
                    .map_err(|error| Error::new(format!("cannot bind {value}: {error}")))?;
                Value::Decimal(decimal)
            }
            Param::Date(date) => {
                let days = i32::try_from(date.days_since_epoch())
                    .map_err(|_| Error::new(format!("cannot bind the date {date}")))?;
                Value::Date32(days)
            }
        })
    }
}

/// How a result column is presented. Without a hint a column gets its name
/// as the title and the kind of its DuckDB type, which is all `b4 sql` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    name: String,
    title: Option<String>,
    kind: Option<Kind>,
    flexible: bool,
}

impl Hint {
    /// A hint for the result column called `name`. A result that has no such
    /// column ignores it.
    pub fn new(name: impl Into<String>) -> Hint {
        Hint {
            name: name.into(),
            title: None,
            kind: None,
            flexible: false,
        }
    }

    /// The title a person reads instead of the column name.
    pub fn title(mut self, title: impl Into<String>) -> Hint {
        self.title = Some(title.into());
        self
    }

    /// Show the column as money in the currency of the column `currency` of
    /// the same result.
    pub fn money(self, currency: impl Into<String>) -> Hint {
        self.kind(Kind::Money {
            currency: currency.into(),
        })
    }

    /// Show the column as this kind instead of the kind of its type. The
    /// kind decides alignment and number format only; the cells stay what
    /// the database returned.
    pub fn kind(mut self, kind: Kind) -> Hint {
        self.kind = Some(kind);
        self
    }

    /// Let the table cut this column when the terminal is too narrow.
    pub fn flexible(mut self) -> Hint {
        self.flexible = true;
        self
    }
}

impl Database {
    /// Open the database at `path` read-only and sandboxed, and check that
    /// this version of b4 wrote it.
    ///
    /// Fails with advice to run `b4 decrypt` when there is no file, when the
    /// file is not a DuckDB database, when it has no `meta` table or when
    /// its schema version is not [`SCHEMA_VERSION`].
    pub fn open(path: impl AsRef<Path>) -> Result<Database, Error> {
        let path = path.as_ref();
        check_file(path)?;

        let connection = open_config()
            .and_then(|config| Connection::open_with_flags(path, config))
            .and_then(|connection| seal(&connection).map(|()| connection))
            .map_err(|error| {
                Error::new(format!(
                    "cannot open the database {}: {}",
                    crate::output::message_text(&path.display().to_string()),
                    message_of(&error)
                ))
            })?;
        let mut database = Database {
            connection,
            path: path.to_path_buf(),
            meta: BTreeMap::new(),
        };
        database.meta = database.read_meta()?;
        database.check_schema_version()?;
        Ok(database)
    }

    /// The path the database was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The value of `key` in the `meta` table (see
    /// [`meta_key`]), if there is one.
    pub fn meta(&self, key: &str) -> Option<&str> {
        self.meta.get(key).map(String::as_str)
    }

    /// The warnings the reader stored when the vault was decrypted, in the
    /// order they were stored. A database without a `warnings` table has
    /// none.
    pub fn warnings(&self) -> Result<Vec<String>, Error> {
        if !self.relations()?.iter().any(|r| r.name == WARNINGS_TABLE) {
            return Ok(Vec::new());
        }
        let sql = format!(
            "select message from {} order by rowid",
            quote_identifier(WARNINGS_TABLE)
        );
        let table = self.query(&sql, &[]).context("cannot read the warnings")?;
        Ok(table
            .rows()
            .iter()
            .filter_map(|row| text_at(row, 0))
            .collect())
    }

    /// The tables and views of the database, ordered by name. Internal and
    /// temporary ones are left out.
    pub fn relations(&self) -> Result<Vec<Relation>, Error> {
        let table = self
            .query(
                "select table_name, false, comment from duckdb_tables() \
                 where database_name = current_database() and schema_name = 'main' \
                   and not internal and not temporary \
                 union all \
                 select view_name, true, comment from duckdb_views() \
                 where database_name = current_database() and schema_name = 'main' \
                   and not internal and not temporary \
                 order by 1",
                &[],
            )
            .context("cannot list the tables")?;
        Ok(table
            .rows()
            .iter()
            .filter_map(|row| {
                Some(Relation {
                    name: text_at(row, 0)?,
                    is_view: matches!(row.get(1), Some(Cell::Boolean(true))),
                    description: text_at(row, 2),
                })
            })
            .collect())
    }

    /// The tables of the database without its views, ordered by name.
    pub fn tables(&self) -> Result<Vec<Relation>, Error> {
        let mut relations = self.relations()?;
        relations.retain(|relation| !relation.is_view);
        Ok(relations)
    }

    /// The table or view called `name`; the case of letters does not matter.
    /// An unknown name is an error that lists the names there are.
    pub fn relation(&self, name: &str) -> Result<Relation, Error> {
        let mut relations = self.relations()?;
        let position = relations
            .iter()
            .position(|relation| relation.name == name)
            .or_else(|| {
                relations
                    .iter()
                    .position(|relation| relation.name.eq_ignore_ascii_case(name))
            });
        match position {
            Some(index) => Ok(relations.swap_remove(index)),
            None => {
                let names: Vec<&str> = relations.iter().map(|r| r.name.as_str()).collect();
                // Names of a database from elsewhere are data like any other.
                Err(Error::new(crate::output::message_text(&format!(
                    "no table or view named `{name}`: there are {}",
                    names.join(", ")
                ))))
            }
        }
    }

    /// The columns of `relation`, in table order.
    pub fn columns(&self, relation: &Relation) -> Result<Vec<ColumnInfo>, Error> {
        let table = self
            .query(
                "select column_name, data_type, comment from duckdb_columns() \
                 where database_name = current_database() and schema_name = 'main' \
                   and table_name = ? \
                 order by column_index",
                &[Param::from(relation.name.as_str())],
            )
            .context(format!("cannot list the columns of {}", relation.name))?;
        Ok(table
            .rows()
            .iter()
            .filter_map(|row| {
                Some(ColumnInfo {
                    name: text_at(row, 0)?,
                    data_type: text_at(row, 1)?,
                    description: text_at(row, 2),
                })
            })
            .collect())
    }

    /// The number of rows of `relation`.
    pub fn row_count(&self, relation: &Relation) -> Result<u64, Error> {
        let sql = format!("select count(*) from {}", quote_identifier(&relation.name));
        let table = self
            .query(&sql, &[])
            .context(format!("cannot count the rows of {}", relation.name))?;
        match table.rows().first().and_then(|row| row.first()) {
            Some(Cell::Integer(count)) => Ok(u64::try_from(*count).unwrap_or(0)),
            _ => Err(Error::new(format!(
                "cannot count the rows of {}",
                relation.name
            ))),
        }
    }

    /// Run `sql` with `params` bound to its placeholders and return the whole
    /// result. Every column has its name as key and title and the kind of its
    /// type; see [`Database::query_with`] to present columns differently.
    ///
    /// `sql` may hold several statements separated by `;`. All of them run,
    /// in order, and the result is that of the last one; parameters belong to
    /// the last one. An error is DuckDB's own message, cleaned for a terminal
    /// (see [`message_of`]).
    ///
    /// The whole result is held in memory. That is right for the bounded
    /// queries of b4's own commands; a result of unknown size goes through
    /// [`Database::for_each_chunk`].
    pub fn query(&self, sql: &str, params: &[Param]) -> Result<Table, Error> {
        self.query_with(sql, params, &[])
    }

    /// [`Database::query`] with presentation hints: a [`Hint`] applies to the
    /// result column of its name and is ignored when there is none.
    pub fn query_with(&self, sql: &str, params: &[Param], hints: &[Hint]) -> Result<Table, Error> {
        let mut result: Option<Table> = None;
        self.for_each_chunk(sql, params, hints, usize::MAX, |chunk| {
            result = Some(chunk);
            Ok(())
        })?;
        Ok(result.unwrap_or_default())
    }

    /// Run `sql` like [`Database::query_with`], but hand the result to
    /// `consume` in tables of at most `chunk_rows` rows, so that a large
    /// result is never held as a whole: DuckDB streams the result of the last
    /// statement instead of materialising it, and b4 holds one chunk.
    ///
    /// `consume` is called at least once: an empty result arrives as one
    /// table with the columns and no rows, and a statement without a result
    /// as one table without columns. It is not called before the statement
    /// was accepted and has produced its first chunk, so a statement that is
    /// refused, or fails at once, has delivered nothing. An error later in
    /// the result arrives after the chunks before it were consumed.
    ///
    /// A statement that names one of the [`REFUSED_SWITCHES`] is refused
    /// before it reaches DuckDB.
    pub fn for_each_chunk(
        &self,
        sql: &str,
        params: &[Param],
        hints: &[Hint],
        chunk_rows: usize,
        mut consume: impl FnMut(Table) -> Result<(), Error>,
    ) -> Result<(), Error> {
        if is_blank(sql) {
            return Err(Error::new("the query is empty"));
        }
        check_switches(sql)?;
        let values = params
            .iter()
            .map(Param::to_value)
            .collect::<Result<Vec<Value>, Error>>()?;

        let mut statement = self.connection.prepare(sql).map_err(query_error)?;
        // `query` would have DuckDB materialise the whole result before the
        // first row is handed out. `stream_arrow` binds the parameters and
        // executes the statement as a streaming result; its Arrow iterator
        // is not used, the rows are read from the same result one by one.
        drop(
            statement
                .stream_arrow(duckdb::params_from_iter(values.iter()))
                .map_err(query_error)?,
        );
        let mut rows = statement.raw_query();

        // The columns are known once the statement ran, also for an empty
        // result.
        let (columns, types) = match rows.as_ref() {
            Some(statement) => result_columns(statement, hints)?,
            None => (Vec::new(), Vec::new()),
        };
        let chunk_rows = chunk_rows.max(1);
        let mut chunk = Table::new(columns.clone());
        let mut delivered = false;
        while let Some(row) = rows.next().map_err(query_error)? {
            let mut cells = Vec::with_capacity(types.len());
            for (index, id) in types.iter().enumerate() {
                let value = row.get_ref(index).map_err(query_error)?;
                cells.push(value::cell_of(value, *id));
            }
            chunk.push(cells);
            if chunk.len() >= chunk_rows {
                consume(std::mem::replace(&mut chunk, Table::new(columns.clone())))?;
                delivered = true;
            }
        }
        if !chunk.is_empty() || !delivered {
            consume(chunk)?;
        }
        Ok(())
    }

    fn read_meta(&self) -> Result<BTreeMap<String, String>, Error> {
        let advice = "run `b4 decrypt <VAULT>` to write it again";
        let has_meta = self
            .relations()?
            .iter()
            .any(|relation| relation.name == META_TABLE && !relation.is_view);
        if !has_meta {
            return Err(Error::new(format!(
                "{} is not a database written by b4 (it has no `{META_TABLE}` table): {advice}",
                self.path.display()
            )));
        }
        let sql = format!("select key, value from {}", quote_identifier(META_TABLE));
        let table = self.query(&sql, &[]).map_err(|error| {
            Error::new(format!(
                "{} is not a database written by b4 (cannot read `{META_TABLE}`: {error}): \
                 {advice}",
                self.path.display()
            ))
        })?;
        Ok(table
            .rows()
            .iter()
            .filter_map(|row| Some((text_at(row, 0)?, text_at(row, 1).unwrap_or_default())))
            .collect())
    }

    fn check_schema_version(&self) -> Result<(), Error> {
        let found = self.meta(meta_key::SCHEMA_VERSION);
        if found.and_then(|text| text.trim().parse::<u32>().ok()) == Some(SCHEMA_VERSION) {
            return Ok(());
        }
        let found = match found {
            Some(version) => format!("has schema version {version}"),
            None => "does not say which schema version it has".to_owned(),
        };
        let written_by = match self.meta(meta_key::B4_VERSION) {
            Some(version) => format!(" (written by b4 {version})"),
            None => String::new(),
        };
        Err(Error::new(format!(
            "the database {} {found}{written_by}, but this b4 reads schema version \
             {SCHEMA_VERSION}: run `b4 decrypt <VAULT>` to write it again",
            self.path.display()
        )))
    }
}

/// `name` as a quoted SQL identifier. For names that come from the catalog;
/// a name the user typed is looked up with [`Database::relation`] first.
pub fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Whether `message`, the text of a failed query, is the sandbox refusing a
/// statement: a write to the database, access to a file or the network, an
/// extension, or a change of the configuration.
pub fn is_refusal(message: &str) -> bool {
    REFUSALS.iter().any(|refusal| message.contains(refusal))
}

/// Whether `message` is DuckDB failing for want of a file system. That is a
/// statement that tried to reach a file, or one that needed more memory than
/// [`MEMORY_LIMIT_BYTES`] and could not spill to disk: DuckDB words both the
/// same way.
pub fn is_file_system_refusal(message: &str) -> bool {
    message.contains(NO_FILE_SYSTEM)
}

/// The configuration a reading connection is opened with. [`seal`] completes
/// it. See the module documentation.
fn open_config() -> duckdb::Result<Config> {
    Config::default()
        .access_mode(AccessMode::ReadOnly)?
        .enable_external_access(false)?
        .enable_autoload_extension(false)?
        .with("autoinstall_known_extensions", "false")?
        // Not reachable once the file system is disabled; kept so that
        // spilling stays off even if that ever fails to hold.
        .with("max_temp_directory_size", "0KB")
}

/// Finish the sandbox on a connection that was just opened: limit the
/// memory, take the file system away and lock the configuration. Nothing
/// else has run on the connection yet.
fn seal(connection: &Connection) -> duckdb::Result<()> {
    let default: String =
        connection.query_row("select current_setting('memory_limit')", [], |row| {
            row.get(0)
        })?;
    // Lower the limit, never raise it. A text this build cannot read is
    // treated as too much.
    if parse_memory_size(&default).is_none_or(|bytes| bytes > MEMORY_LIMIT_BYTES) {
        connection.execute_batch(&format!(
            "SET memory_limit = '{}MiB'",
            MEMORY_LIMIT_BYTES / (1024 * 1024)
        ))?;
    }
    connection.execute_batch(
        "SET disabled_filesystems = 'LocalFileSystem';
         SET lock_configuration = true;",
    )
}

/// The number of bytes in a size as DuckDB prints it: `98.7 GiB`,
/// `244.1 MiB`, `0 bytes`.
fn parse_memory_size(text: &str) -> Option<u64> {
    let (number, unit) = text.trim().split_once(' ')?;
    let factor: u64 = match unit.trim() {
        "byte" | "bytes" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        "PiB" => 1 << 50,
        _ => return None,
    };
    let number: f64 = number.parse().ok()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    // Sizes are far below 2^53 units, and the cast saturates.
    Some((number * factor as f64) as u64)
}

/// Refuse `sql` if it names one of the [`REFUSED_SWITCHES`] or calls one of
/// the [`REFUSED_CALLS`]. A check of the text, see the module documentation.
fn check_switches(sql: &str) -> Result<(), Error> {
    let text = sql.to_ascii_lowercase();
    if let Some(name) = REFUSED_SWITCHES
        .iter()
        .find(|name| words(&text, name).next().is_some())
    {
        return Err(Error::new(format!(
            "`{name}` cannot be used in a query: it switches a part of the database engine \
             (profiling, logging, the progress bar, verification) for the whole process, and \
             b4 keeps those as they are"
        )));
    }
    if let Some((name, what, why)) = REFUSED_CALLS
        .iter()
        .find(|(name, ..)| words(&text, name).any(|end| is_called(&text[end..])))
    {
        return Err(Error::new(format!(
            "{what} `{name}` cannot be used in a query: {why}"
        )));
    }
    Ok(())
}

/// Where `word` ends at each place it stands in `text` as a whole word: not
/// as a part of a longer name. Both are lower-case ASCII as far as letters
/// matter.
fn words<'a>(text: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let is_name = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    text.match_indices(word).filter_map(move |(start, _)| {
        let end = start + word.len();
        let before = start
            .checked_sub(1)
            .and_then(|index| text.as_bytes().get(index));
        let after = text.as_bytes().get(end);
        let whole =
            !before.is_some_and(|byte| is_name(*byte)) && !after.is_some_and(|byte| is_name(*byte));
        whole.then_some(end)
    })
}

/// Whether `rest`, the text after a name, makes the name a function call:
/// whether the next character that counts is an opening parenthesis.
///
/// What counts is a printable ASCII character outside a comment. Everything
/// else is passed over: the quote that closes a quoted name, comments, white
/// space, control characters and every character that is not ASCII. DuckDB
/// takes more characters for blanks than any list here could name (a
/// zero-width space, a byte order mark, a word joiner), so none is named:
/// whatever is not a letter, a digit or a punctuation mark of ASCII cannot
/// stand between a name and its parenthesis and hide the call. A name that
/// is not called is followed by one of those (`query,` `query.` `query)`
/// `query =` `query from`), and is left alone.
fn is_called(rest: &str) -> bool {
    let mut rest = rest.strip_prefix('"').unwrap_or(rest);
    loop {
        if let Some(comment) = rest.strip_prefix("--") {
            // To the end of the line, which either character ends.
            rest = comment
                .split_once(['\n', '\r'])
                .map_or("", |(_, after)| after);
        } else if let Some(comment) = rest.strip_prefix("/*") {
            rest = after_block_comment(comment);
        } else {
            let mut characters = rest.chars();
            match characters.next() {
                None => return false,
                Some(character) if character.is_ascii_graphic() => return character == '(',
                Some(_) => rest = characters.as_str(),
            }
        }
    }
}

/// What follows the block comment whose text after the opening `/*` is
/// `inside`. Block comments nest in DuckDB: the comment ends where every
/// `/*` in it has its `*/`. One that never ends takes the rest of the text.
fn after_block_comment(inside: &str) -> &str {
    let mut rest = inside;
    let mut depth = 1usize;
    while depth > 0 {
        let Some(close) = rest.find("*/") else {
            return "";
        };
        match rest.find("/*") {
            Some(open) if open < close => {
                depth += 1;
                rest = &rest[open + 2..];
            }
            _ => {
                depth -= 1;
                rest = &rest[close + 2..];
            }
        }
    }
    rest
}

/// Fail early, and clearly, when `path` cannot be a database: DuckDB's own
/// words for a missing file do not say what to do about it.
fn check_file(path: &Path) -> Result<(), Error> {
    let shown = path.display();
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(Error::new(format!(
                "there is no database at {shown}: run `b4 decrypt <VAULT>` to create it, or \
                 name an existing one with --db or B4_DB"
            )));
        }
        Err(error) => {
            return Err(Error::from(error).context(format!("cannot open the database {shown}")));
        }
    };
    let not_a_database = || {
        Error::new(format!(
            "{shown} is not a DuckDB database: run `b4 decrypt <VAULT>` to create one, or name \
             another file with --db or B4_DB"
        ))
    };
    // A directory opens like a file on Unix, and only fails when read.
    if file.metadata().is_ok_and(|metadata| metadata.is_dir()) {
        return Err(not_a_database());
    }
    let mut start = [0u8; MAGIC_OFFSET + MAGIC.len()];
    match file.read_exact(&mut start) {
        Ok(()) if start.ends_with(MAGIC) => Ok(()),
        Ok(()) => Err(not_a_database()),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Err(not_a_database()),
        Err(error) => Err(Error::from(error).context(format!("cannot read the database {shown}"))),
    }
}

/// Whether `sql` holds no statement: only blanks and semicolons. DuckDB
/// reports that case without any message.
fn is_blank(sql: &str) -> bool {
    sql.chars().all(|c| c.is_whitespace() || c == ';')
}

/// DuckDB's own message for `error`, without the wrapper the crate adds.
///
/// The message is cleaned for a terminal with
/// [`message_text`](crate::output::message_text): DuckDB quotes data in its
/// messages ("Could not convert string '...' to INT32"), and text from a
/// vault can hold escape sequences.
pub(crate) fn message_of(error: &duckdb::Error) -> String {
    let message = match error {
        duckdb::Error::DuckDBFailure(_, Some(message)) => message.trim_end().to_owned(),
        // Nothing to run, such as a comment alone.
        duckdb::Error::DuckDBFailure(_, None) => {
            "the database reported an error without a message (is the query empty?)".to_owned()
        }
        other => other.to_string(),
    };
    crate::output::message_text(&message)
}

fn query_error(error: duckdb::Error) -> Error {
    Error::new(message_of(&error))
}

/// The text of the cell at `index`, if it is text.
fn text_at(row: &[Cell], index: usize) -> Option<String> {
    match row.get(index) {
        Some(Cell::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

/// The output columns of an executed statement and the DuckDB type of each.
fn result_columns(
    statement: &duckdb::Statement<'_>,
    hints: &[Hint],
) -> Result<(Vec<Column>, Vec<Option<LogicalTypeId>>), Error> {
    let count = statement.column_count();
    let mut names = Vec::with_capacity(count);
    let mut types = Vec::with_capacity(count);
    let mut kinds = Vec::with_capacity(count);
    for index in 0..count {
        let name = statement.column_name(index).map_err(query_error)?;
        names.push(name.clone());
        let logical = statement.column_logical_type(index);
        // A type this build does not know is shown as text.
        let id = logical.try_id().ok();
        let scale = match id {
            Some(LogicalTypeId::Decimal) => u32::from(logical.decimal_scale()),
            _ => 0,
        };
        kinds.push(value::kind_of(id, scale));
        types.push(id);
    }

    let keys = distinct_keys(&names);
    let columns = names
        .iter()
        .zip(keys)
        .zip(kinds)
        .map(|((name, key), kind)| {
            let hint = hints.iter().find(|hint| &hint.name == name);
            let title = hint
                .and_then(|hint| hint.title.clone())
                .unwrap_or_else(|| key.clone());
            let kind = hint.and_then(|hint| hint.kind.clone()).unwrap_or(kind);
            let column = Column::new(key, title, kind);
            if hint.is_some_and(|hint| hint.flexible) {
                column.flexible()
            } else {
                column
            }
        })
        .collect();
    Ok((columns, types))
}

/// A distinct key for every result column: the name, `column_<position>` for
/// an empty name, and `_2`, `_3`, ... appended to a name that is already
/// taken (`sum`, `sum_2`). Names the query gives explicitly keep theirs
/// where possible: the suffix goes to the later column.
fn distinct_keys(names: &[String]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        let base = if name.is_empty() {
            format!("column_{}", index + 1)
        } else {
            name.clone()
        };
        let taken = |key: &str, keys: &[String]| {
            // A name that only appears later still wins over a made-up key.
            keys.iter().any(|k| k == key)
                || names
                    .iter()
                    .skip(index + 1)
                    .any(|later| later == key && key != base)
        };
        let mut key = base.clone();
        let mut suffix = 2usize;
        while taken(&key, &keys) {
            key = format!("{base}_{suffix}");
            suffix += 1;
        }
        keys.push(key);
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str]) -> Vec<String> {
        let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        distinct_keys(&names)
    }

    #[test]
    fn repeated_and_empty_names_get_distinct_keys() {
        assert_eq!(keys(&["a", "b"]), ["a", "b"]);
        assert_eq!(keys(&["sum", "sum", "sum"]), ["sum", "sum_2", "sum_3"]);
        assert_eq!(keys(&["", "x", ""]), ["column_1", "x", "column_3"]);
        // A made-up key does not take the name of a real column.
        assert_eq!(keys(&["a", "a", "a_2"]), ["a", "a_3", "a_2"]);
        assert_eq!(keys(&["a_2", "a", "a"]), ["a_2", "a", "a_3"]);
        assert_eq!(keys(&["", "column_1"]), ["column_1", "column_1_2"]);
    }

    #[test]
    fn identifiers_are_quoted() {
        assert_eq!(quote_identifier("bookings"), "\"bookings\"");
        assert_eq!(quote_identifier("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn blank_queries_are_recognised() {
        assert!(is_blank(""));
        assert!(is_blank(" ;\n; "));
        assert!(!is_blank("select 1;"));
    }

    #[test]
    fn refusals_are_recognised_by_their_message() {
        assert!(is_refusal(
            "Permission Error: Cannot access file \"/etc/passwd\" - file system operations are \
             disabled by configuration"
        ));
        assert!(is_refusal(
            "Invalid Input Error: Cannot execute statement of type \"INSERT\" on database \"x\" \
             which is attached in read-only mode!"
        ));
        assert!(is_refusal(
            "Invalid Input Error: Cannot change configuration option \"threads\" - the \
             configuration has been locked"
        ));
        assert!(!is_refusal("Parser Error: syntax error at end of input"));
    }

    /// A database with a `meta` table and one table of mixed columns.
    fn sample() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b4.duckdb");
        Connection::open(&path)
            .unwrap()
            .execute_batch(&format!(
                "create table meta(key varchar primary key, value varchar);
                 insert into meta values ('schema_version', '{SCHEMA_VERSION}'), ('b4_version', '0.1.0');
                 create table warnings(message varchar);
                 insert into warnings values ('second is first'), ('a later one');
                 create table bookings(
                   id bigint, amount decimal(18,4), currency varchar, day date, purpose varchar);
                 comment on table bookings is 'Bookings';
                 comment on column bookings.amount is 'Signed amount';
                 insert into bookings values
                   (1, -12.5, 'EUR', date '2024-03-01', 'it''s; drop table bookings'),
                   (2, 100, 'USD', date '2024-03-02', null);
                 create view booking_days as select day from bookings;"
            ))
            .unwrap();
        let database = Database::open(&path).unwrap();
        (dir, database)
    }

    #[test]
    fn the_catalog_is_read_back() {
        let (_dir, database) = sample();
        assert_eq!(database.meta(meta_key::B4_VERSION), Some("0.1.0"));
        assert_eq!(database.meta("nothing"), None);
        assert_eq!(
            database.warnings().unwrap(),
            ["second is first", "a later one"]
        );

        let names: Vec<(String, bool)> = database
            .relations()
            .unwrap()
            .into_iter()
            .map(|relation| (relation.name, relation.is_view))
            .collect();
        assert_eq!(
            names,
            [
                ("booking_days".to_owned(), true),
                ("bookings".to_owned(), false),
                ("meta".to_owned(), false),
                ("warnings".to_owned(), false)
            ]
        );
        assert_eq!(database.tables().unwrap().len(), 3);

        let bookings = database.relation("BOOKINGS").unwrap();
        assert_eq!(bookings.description.as_deref(), Some("Bookings"));
        assert_eq!(database.row_count(&bookings).unwrap(), 2);
        let columns = database.columns(&bookings).unwrap();
        assert_eq!(columns.len(), 5);
        assert_eq!(columns[1].name, "amount");
        assert_eq!(columns[1].data_type, "DECIMAL(18,4)");
        assert_eq!(columns[1].description.as_deref(), Some("Signed amount"));
        assert_eq!(columns[0].description, None);

        let error = database.relation("nope").unwrap_err().to_string();
        assert_eq!(
            error,
            "no table or view named `nope`: there are booking_days, bookings, meta, warnings"
        );
    }

    #[test]
    fn parameters_are_bound_and_never_part_of_the_statement() {
        let (_dir, database) = sample();
        // The text of a parameter is compared, not run.
        let hostile = "it's; drop table bookings";
        let table = database
            .query(
                "select id from bookings where purpose = ?",
                &[Param::from(hostile)],
            )
            .unwrap();
        assert_eq!(table.rows(), [vec![Cell::Integer(1)]]);
        let none = database
            .query(
                "select id from bookings where purpose = ?",
                &[Param::from("x' or '1'='1")],
            )
            .unwrap();
        assert!(none.is_empty());
        assert_eq!(none.columns().len(), 1);

        let table = database
            .query(
                "select id from bookings \
                 where amount >= ? and day <= ? and id <> ? and (currency = ? or ? is null) \
                   and ? and amount < ?",
                &[
                    Param::from(Decimal::new(-125, 1)),
                    Param::from(Date::new(2024, 3, 1).unwrap()),
                    Param::from(99_i64),
                    Param::from("EUR".to_owned()),
                    Param::from(None::<&str>),
                    Param::from(true),
                    Param::from(0.5),
                ],
            )
            .unwrap();
        assert_eq!(table.rows(), [vec![Cell::Integer(1)]]);

        let error = database.query("select ? + 1", &[]).unwrap_err().to_string();
        assert!(error.contains("parameters"), "{error}");
    }

    #[test]
    fn columns_take_their_kind_from_the_type_and_from_hints() {
        let (_dir, database) = sample();
        let sql = "select id, amount, currency, day, purpose from bookings order by id";
        let plain = database.query(sql, &[]).unwrap();
        let kinds: Vec<&Kind> = plain.columns().iter().map(Column::kind).collect();
        assert_eq!(
            kinds,
            [
                &Kind::Integer,
                &Kind::Decimal { scale: 4 },
                &Kind::Text,
                &Kind::Date,
                &Kind::Text
            ]
        );
        assert_eq!(plain.columns()[1].title(), "amount");
        assert_eq!(
            plain.rows()[0],
            [
                Cell::Integer(1),
                Cell::Decimal(Decimal::new(-125_000, 4)),
                Cell::from("EUR"),
                Cell::Date(Date::new(2024, 3, 1).unwrap()),
                Cell::from("it's; drop table bookings"),
            ]
        );
        assert_eq!(plain.rows()[1][4], Cell::Null);

        let hinted = database
            .query_with(
                sql,
                &[],
                &[
                    Hint::new("amount").title("Amount").money("currency"),
                    Hint::new("purpose").flexible(),
                    Hint::new("id").kind(Kind::Text),
                    Hint::new("not in the result").title("Ignored"),
                ],
            )
            .unwrap();
        assert_eq!(
            hinted.columns()[1],
            Column::money("amount", "Amount", "currency")
        );
        assert_eq!(
            hinted.columns()[4],
            Column::text("purpose", "purpose").flexible()
        );
        assert_eq!(hinted.columns()[0].kind(), &Kind::Text);
        // Hints change the presentation, not the values.
        assert_eq!(hinted.rows(), plain.rows());
    }

    #[test]
    fn a_result_arrives_in_chunks() {
        let (_dir, database) = sample();
        let sizes = |sql: &str, chunk_rows: usize| {
            let mut sizes = Vec::new();
            database
                .for_each_chunk(sql, &[], &[], chunk_rows, |chunk| {
                    assert_eq!(chunk.columns().len(), 1);
                    sizes.push(chunk.len());
                    Ok(())
                })
                .unwrap();
            sizes
        };
        assert_eq!(
            sizes("select i from range(2500) t(i)", 1000),
            [1000, 1000, 500]
        );
        assert_eq!(sizes("select i from range(2000) t(i)", 1000), [1000, 1000]);
        assert_eq!(sizes("select i from range(3) t(i)", 0), [1, 1, 1]);
        // An empty result still delivers its columns, once.
        assert_eq!(sizes("select i from range(0) t(i)", 1000), [0]);

        // The consumer can stop the query.
        let mut calls = 0;
        let error = database
            .for_each_chunk("select i from range(10) t(i)", &[], &[], 2, |_| {
                calls += 1;
                Err(Error::new("disk full"))
            })
            .unwrap_err();
        assert_eq!((calls, error.to_string().as_str()), (1, "disk full"));
    }

    #[test]
    fn the_connection_cannot_write_or_reach_out() {
        let (dir, database) = sample();
        for sql in [
            "insert into bookings (id) values (3)",
            "create table t(i integer)",
            "set threads = 1",
            "select * from read_csv('/etc/passwd')",
            "attach 'other.duckdb' as other",
        ] {
            let error = database.query(sql, &[]).unwrap_err().to_string();
            assert!(is_refusal(&error), "{sql}: {error}");
        }
        assert_eq!(
            database.query("  ; ", &[]).unwrap_err().to_string(),
            "the query is empty"
        );
        drop(database);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["b4.duckdb"]);
    }

    #[test]
    fn the_refused_switches_are_the_ones_duckdb_has() {
        // Every pragma and table function that switches something: if a new
        // DuckDB adds one, it must be added to the list (or this rule
        // refined), not slip through.
        let (_dir, database) = sample();
        let table = database
            .query(
                "select distinct function_name from duckdb_functions() \
                 where function_type in ('pragma', 'table') \
                   and (regexp_matches(function_name, '^(enable|disable|verify)_') \
                        or function_name ilike '%profil%' and function_type = 'pragma' \
                        or function_name = ?) \
                   and function_name <> 'all_profiling_output' \
                 order by 1",
                // Not in the text: the check would refuse this very query.
                &[Param::from("truncate_duckdb_logs")],
            )
            .unwrap();
        let mut found: Vec<String> = table.rows().iter().filter_map(|r| text_at(r, 0)).collect();
        found.sort();
        let mut listed: Vec<String> = REFUSED_SWITCHES.iter().map(|s| (*s).to_owned()).collect();
        listed.sort();
        assert_eq!(found, listed);
        // The dynamic SQL function exists under the name that is refused.
        let table = database
            .query(
                "select count(*) from duckdb_functions() \
                 where function_type = 'table' and function_name = ?",
                &[Param::from(DYNAMIC_SQL_FUNCTION)],
            )
            .unwrap();
        assert!(matches!(table.rows()[0][0], Cell::Integer(n) if n > 0));
    }

    #[test]
    fn a_statement_that_names_a_switch_is_refused_by_its_text() {
        let refused = |sql: &str| check_switches(sql).unwrap_err().to_string();
        for (sql, name) in [
            ("pragma enable_profiling", "enable_profiling"),
            ("PRAGMA Enable_Profiling = json", "enable_profiling"),
            ("call enable_profiling()", "enable_profiling"),
            (
                "select * from enable_profiling(format='json')",
                "enable_profiling",
            ),
            ("select 1; pragma \"ENABLE_PROFILE\"", "enable_profile"),
            ("pragma disable_profiling;select 1", "disable_profiling"),
            (
                "select * from enable_logging(storage='stdout')",
                "enable_logging",
            ),
            ("call main.disable_logging()", "disable_logging"),
            ("from truncate_duckdb_logs()", "truncate_duckdb_logs"),
            ("pragma\tenable_progress_bar", "enable_progress_bar"),
            (
                "pragma enable_print_progress_bar",
                "enable_print_progress_bar",
            ),
            ("pragma enable_verification", "enable_verification"),
            ("pragma verify_parallelism", "verify_parallelism"),
            ("pragma disable_optimizer", "disable_optimizer"),
            // Conservative: a string or a comment is enough.
            ("select 'enable_logging'", "enable_logging"),
            ("select 1 -- enable_logging", "enable_logging"),
        ] {
            let message = refused(sql);
            assert!(
                message.starts_with(&format!("`{name}` cannot be used in a query")),
                "{sql}: {message}"
            );
        }
        for sql in [
            "select * from query('select 1')",
            "select * from QUERY ('select 1')",
            "from \"query\"('select 1')",
            "from query /* now */ -- then\n ('select * from enable_' || 'profiling()')",
            "from system.main.query\n('select 1')",
        ] {
            let message = refused(sql);
            assert!(
                message.starts_with("the table function `query` cannot be used"),
                "{sql}: {message}"
            );
        }
        // A longer or another name is not a switch, and `query` is only
        // refused as a call.
        for sql in [
            "select 1",
            "select enable_profiling_x, my_enable_logging from t",
            "select query from log",
            "select query, (1) from log",
            "select * from query_table('accounts')",
            "select * from query_table(['accounts', 'payees'])",
            "select subquery(1)",
            "pragma database_size",
            "pragma table_info('accounts')",
        ] {
            assert!(check_switches(sql).is_ok(), "{sql}");
        }
    }

    /// What DuckDB accepts between the name of a function and its
    /// parenthesis, and what must therefore not hide a call.
    const BETWEEN_NAME_AND_PARENTHESIS: &[&str] = &[
        "",
        " ",
        "\t",
        "\n",
        "\r",
        "\r\n",
        "\u{b}\u{c}",
        // A zero-width space, a byte order mark and a word joiner: DuckDB
        // reads all three as blanks, and no rule of Unicode calls them white
        // space.
        "\u{200b}",
        "\u{feff}",
        "\u{2060}",
        // A no-break space, an ideographic space, a next line, a line
        // separator, and characters that are no blanks at all.
        "\u{a0}",
        "\u{3000}",
        "\u{85}",
        "\u{2028}",
        "\u{200d}",
        "\u{ad}",
        "\u{1}\u{7f}",
        "\u{e9}\u{1f600}",
        // Comments: to the end of a line, whichever character ends it, and
        // in blocks, which nest.
        "/* now */",
        "/**/",
        "/* ( */",
        "/* a /* b */ c */",
        "/*/* /* */ */*/",
        "-- then\n",
        "-- then\r",
        "--\n",
        "-- /* \n",
        "/* -- */",
        // And all of it together.
        " \u{200b}/* a */\u{feff}-- b\n\u{2060}\t",
        "\u{200b}\u{200b} \u{feff}/* /* */ */\u{3000}-- (\r\u{a0}",
    ];

    #[test]
    fn nothing_between_a_name_and_its_parenthesis_hides_a_call() {
        for between in BETWEEN_NAME_AND_PARENTHESIS {
            assert!(is_called(&format!("{between}(")), "{between:?}");
            assert!(is_called(&format!("\"{between}('x')")), "{between:?}");
            for (sql, message) in [
                (
                    format!("select * from query{between}('select 1')"),
                    "the table function `query` cannot be used in a query: ",
                ),
                (
                    format!("from \"Query\"{between}('select * from enable_' || 'logging()')"),
                    "the table function `query` cannot be used in a query: ",
                ),
                (
                    format!("from main.{between}query{between}('select 1')"),
                    "the table function `query` cannot be used in a query: ",
                ),
                (
                    format!("select 'select 1'.query{between}()"),
                    "the table function `query` cannot be used in a query: ",
                ),
                (
                    format!("select WRITE_LOG{between}(chr(27) || '[2J')"),
                    "the function `write_log` cannot be used in a query: ",
                ),
            ] {
                let error = check_switches(&sql).unwrap_err().to_string();
                assert!(error.starts_with(message), "{sql:?}: {error}");
            }

            // The same characters do not turn a name into a call.
            for after in [
                ",", ".", ")", ";", "=", "from", "::", "'", "[", "|", "1", "_",
            ] {
                assert!(!is_called(&format!("{between}{after}")), "{between:?}");
                assert!(!is_called(&format!("{between}{after}(")), "{between:?}");
            }
            assert!(!is_called(between), "{between:?}");
        }

        // A comment that does not end has nothing after it.
        assert!(!is_called("/* ("));
        assert!(!is_called("/* /* */ ("));
        assert!(!is_called("-- ("));
        assert_eq!(after_block_comment(" a */ b"), " b");
        assert_eq!(after_block_comment("/* */ */("), "(");
        assert_eq!(after_block_comment("/"), "");
        assert_eq!(after_block_comment("*/"), "");
        assert_eq!(after_block_comment("/*/"), "");
    }

    #[test]
    fn a_column_or_an_alias_named_like_a_refused_function_is_allowed() {
        for sql in [
            "select query from log",
            "select query, (1) from log",
            "select query,(1) from log",
            "select log.query, write_log from log",
            "select \"query\", \"write_log\" from log",
            "select \"query\" , (1) from log",
            "select 1 as query",
            "select 1 as query, (2) as write_log",
            "select 1 query;",
            "select 1 as \"query\"; select (2)",
            "select (query) from log",
            "select (select query) + (1) from log",
            "select query + (1), query - (1), query * (2), query / (2), query % (2) from log",
            "select query || ('x'), query = ('x'), query <> ('x'), query < (1) from log",
            "select query::varchar, query[1], query.field from log",
            "select query is null, query in ('a', 'b'), query like ('a%') from log",
            "select count(query) from log group by query having (count(*) > 1)",
            "select * from log where query = ? and (id > 1)",
            "select * from log order by query, (id)",
            "select * from log as query where (query.id > 1)",
            "select * from log query join (select 1) t on true",
            "with query as (select 1) select * from query",
            "select query /* the text */ , (1) from log",
            "select query -- the text\n , (1) from log",
            "select query\u{200b}, (1) from log",
            "select query\u{a0}from log where (1 = 1)",
            "select query_id(1), my_query(2), queryx(3), subquery(4), write_logs(5)",
            "select * from query_table('accounts')",
        ] {
            assert!(check_switches(sql).is_ok(), "{sql}");
        }
    }

    #[test]
    fn the_refused_calls_exist_and_only_one_function_runs_sql_text() {
        let (_dir, database) = sample();
        // Asked with parameters: the check would refuse the names in a
        // statement that calls anything after them.
        for (name, ..) in REFUSED_CALLS {
            let table = database
                .query(
                    "select count(*) from duckdb_functions() where function_name = ?",
                    &[Param::from(*name)],
                )
                .unwrap();
            assert!(
                matches!(table.rows()[0][0], Cell::Integer(n) if n > 0),
                "{name}"
            );
        }

        // What a new DuckDB might bring: a function that has SQL, a query
        // or a statement in its name or among its parameters. Each one here
        // was looked at; a new one fails this test and must be looked at.
        let table = database
            .query(
                "select distinct function_name from duckdb_functions() \
                 where function_type <> 'pragma' \
                   and (regexp_matches(function_name, ?) \
                        or regexp_matches(array_to_string(parameters, ' '), ?)) \
                 order by 1",
                &[
                    Param::from("sql|query|execute|statement|write_log"),
                    Param::from("(^| )(sql|query|statement|stmt)( |$)"),
                ],
            )
            .unwrap();
        let found: Vec<String> = table.rows().iter().filter_map(|r| text_at(r, 0)).collect();
        assert_eq!(
            found,
            [
                // The text and the number of the running statement.
                "current_query",
                "current_query_id",
                // Lists what `PREPARE` prepared: its text was checked.
                "duckdb_prepared_statements",
                // Refused.
                "query",
                // Tables and views by name, see `REFUSED_CALLS`.
                "query_table",
                // Refused.
                "write_log",
            ]
        );
    }

    #[test]
    fn switches_and_dynamic_sql_do_not_reach_the_database() {
        let (_dir, database) = sample();
        for sql in [
            "pragma enable_profiling",
            "select * from query('select 1')",
            "select * from query\u{200b}('select 1')",
            "select * from query /* /* */ */ ('select 1')",
            "select * from query -- \r('select 1')",
            "select write_log('x')",
        ] {
            let error = database.query(sql, &[]).unwrap_err().to_string();
            assert!(
                error.contains("cannot be used in a query"),
                "{sql}: {error}"
            );
            assert!(!is_refusal(&error), "{sql}");
        }
        assert_eq!(
            database.query("select 1 as query", &[]).unwrap().rows(),
            [vec![Cell::Integer(1)]]
        );
    }

    #[test]
    fn memory_sizes_are_read_as_duckdb_prints_them() {
        assert_eq!(parse_memory_size("0 bytes"), Some(0));
        assert_eq!(parse_memory_size("512 bytes"), Some(512));
        assert_eq!(parse_memory_size("1.0 KiB"), Some(1024));
        assert_eq!(parse_memory_size("244.1 MiB"), Some(255_957_401));
        assert_eq!(parse_memory_size("1.0 GiB"), Some(MEMORY_LIMIT_BYTES));
        assert_eq!(parse_memory_size(" 98.7 GiB "), Some(105_978_318_028));
        assert_eq!(parse_memory_size("2.0 TiB"), Some(2 << 40));
        for text in ["", "GiB", "1.0", "1.0 GB", "-1.0 GiB", "NaN GiB", "x MiB"] {
            assert_eq!(parse_memory_size(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_sandbox_is_sealed_when_the_database_is_open() {
        let (dir, database) = sample();
        let setting = |name: &str| {
            let table = database
                .query("select current_setting(?)", &[Param::from(name)])
                .unwrap();
            text_at(&table.rows()[0], 0).unwrap_or_else(|| format!("{:?}", table.rows()[0][0]))
        };
        let limit = parse_memory_size(&setting("memory_limit")).unwrap();
        assert!(limit <= MEMORY_LIMIT_BYTES, "{limit}");
        // (DuckDB does not report `disabled_filesystems` back; the refusals
        // below show it.)
        assert_eq!(setting("lock_configuration"), "Boolean(true)");
        assert_eq!(setting("enable_external_access"), "Boolean(false)");
        assert_eq!(setting("access_mode"), "read_only");

        let db = dir.path().join("b4.duckdb");
        let db = db.to_str().unwrap();
        for sql in [
            "set disabled_filesystems = ''".to_owned(),
            "reset disabled_filesystems".to_owned(),
            "set memory_limit = '64GB'".to_owned(),
            "pragma memory_limit = '64GB'".to_owned(),
            "set lock_configuration = false".to_owned(),
            format!("select content from read_text('{db}.tmp/secret.txt')"),
            format!("select octet_length(content) from read_blob('{db}')"),
            format!("select * from glob('{db}*')"),
            format!("copy (select 'PWNED') to '{db}.wal' (header false)"),
            format!("copy (select 'PWNED') to '{db}.wal.checkpoint' (header false)"),
            format!("copy (select 'x') to '{db}.tmp/big.txt'"),
            format!("attach '{db}.tmp/other.duckdb' as other"),
        ] {
            let error = database.query(&sql, &[]).unwrap_err().to_string();
            assert!(is_refusal(&error), "{sql}: {error}");
        }
        drop(database);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["b4.duckdb"]);
    }

    #[test]
    fn a_message_of_the_database_cannot_carry_an_escape_sequence() {
        let (_dir, database) = sample();
        let error = database
            .query(
                "select cast(chr(27) || '[2J' || chr(155) || chr(8238) as integer)",
                &[],
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Could not convert string '\u{fffd}[2J\u{fffd}\u{fffd}' to INT32"),
            "{error:?}"
        );
        assert!(!error.contains(['\x1b', '\u{9b}', '\u{202e}']));
        // The lines of a message are kept.
        let error = database
            .query("select * from nowhere", &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("\n\nLINE 1: "), "{error:?}");

        let error = database.relation("no\x1b[2Jpe").unwrap_err().to_string();
        assert!(
            error.starts_with("no table or view named `no\u{fffd}[2Jpe`"),
            "{error:?}"
        );
    }

    #[test]
    fn a_statement_without_a_result_delivers_no_columns() {
        let (_dir, database) = sample();
        let mut chunks = Vec::new();
        database
            .for_each_chunk(
                "create temp macro twice(x) as x * 2",
                &[],
                &[],
                CHUNK_ROWS,
                |chunk| {
                    chunks.push((chunk.columns().len(), chunk.len()));
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(chunks.len(), 1);
        // The macro is there for the next statement of the same connection.
        let table = database.query("select twice(21) as n", &[]).unwrap();
        assert_eq!(table.rows(), [vec![Cell::Integer(42)]]);

        // A failure in the middle of a result arrives after the chunks
        // before it, and nothing is delivered for a statement that is
        // refused.
        let mut delivered = 0;
        let error = database
            .for_each_chunk(
                "select case when i = 5000 then cast('x' as integer) else i end \
                 from range(10000) t(i)",
                &[],
                &[],
                100,
                |chunk| {
                    delivered += chunk.len();
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("Could not convert"), "{error}");
        assert!(delivered < 10_000);
        let mut called = false;
        let _ = database.for_each_chunk("copy bookings to 'x.csv'", &[], &[], 100, |_| {
            called = true;
            Ok(())
        });
        assert!(!called);
    }

    #[test]
    fn params_become_duckdb_values() {
        assert_eq!(Param::from(None::<i64>), Param::Null);
        assert_eq!(Param::from(Some("x")), Param::Text("x".to_owned()));
        let decimal = Param::from(Decimal::new(-12_345, 2)).to_value().unwrap();
        match decimal {
            Value::Decimal(d) => {
                assert_eq!(d.to_string(), "-123.45");
            }
            other => panic!("not a decimal: {other:?}"),
        }
        let date = Param::from(Date::new(1970, 1, 2).unwrap())
            .to_value()
            .unwrap();
        assert_eq!(date, Value::Date32(1));
    }
}
