//! Black-box tests of the commands that work on any database: `sql`,
//! `schema`, `status` and `export`. Each test writes its own small DuckDB
//! file; none needs a decrypted vault.

// The helpers below are not `#[test]` functions, so clippy.toml's exemption
// for tests does not reach them.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use duckdb::Connection;
use tempfile::TempDir;

/// The schema version this build of b4 reads (`db::SCHEMA_VERSION`).
const SCHEMA_VERSION: &str = "1";

/// Where `docs/format/01-container.md` (3.1) puts the vault id and the
/// modification stamp in the header.
const FILE_GUID_OFFSET: usize = 0x070;
const MODIFIED_AT_OFFSET: usize = 0x0D4;

const DEMO_VAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault/demo.sub");

const READ_ONLY_NOTE: &str =
    "b4 sql is read-only: it cannot change the database or reach files or the network";

/// What DuckDB says without a file system, and the two lines b4 adds: a
/// query that runs out of memory can fail with the same words.
const NO_FILE_SYSTEM: &str =
    "Permission Error: File system LocalFileSystem has been disabled by configuration";
const MEMORY_NOTE: &str = "b4 sql gives a query 1 GiB of memory and no disk to spill to, and \
     these limits cannot be changed: reduce the query with a WHERE or a LIMIT";

/// The checksum `b4 decrypt` stores as `vault_checksum`
/// (`db::content_checksum`): 64-bit FNV-1a as 16 hex digits.
fn content_checksum(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{hash:016x}")
}

#[test]
fn the_checksum_of_the_tests_is_the_one_b4_stores() {
    assert_eq!(content_checksum(b""), "cbf29ce484222325");
    assert_eq!(content_checksum(b"a"), "af63dc4c8601ec8c");
    assert_eq!(content_checksum(b"foobar"), "85944171f73967e8");
}

// --- fixtures ---

/// A directory with a database `b4.duckdb` in it.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    /// The sample database, with a vault path that does not exist.
    fn new() -> Fixture {
        let fixture = Fixture {
            dir: tempfile::tempdir().unwrap(),
        };
        fixture.write(&[
            ("schema_version", SCHEMA_VERSION),
            ("b4_version", "0.1.0"),
            ("vault_path", "/nonexistent/b4-test/Household.sub"),
            ("vault_id", "01234567-89ab-cdef-0123-456789abcdef"),
            ("vault_created", "2024-01-01T00:00:00Z"),
            ("vault_modified", "2025-06-30T12:00:00Z"),
            ("vault_modified_stamp", "804600000"),
            ("vault_size", "1024"),
            ("decrypted_at", "2025-07-01T08:15:00Z"),
            ("entries_never_read", "9"),
        ]);
        fixture
    }

    /// The sample database whose `meta` table describes a copy of the demo
    /// vault, which is put next to it as `vault.sub`.
    fn with_vault() -> Fixture {
        let fixture = Fixture {
            dir: tempfile::tempdir().unwrap(),
        };
        let vault = fixture.vault();
        fs::copy(DEMO_VAULT, &vault).unwrap();
        let header = b4::vault::Header::read(&vault).unwrap();
        fixture.write(&[
            ("schema_version", SCHEMA_VERSION),
            ("b4_version", "0.1.0"),
            ("vault_path", vault.to_str().unwrap()),
            ("vault_id", &header.file_guid().to_string()),
            ("vault_created", ""),
            ("vault_modified", "2025-06-30T12:00:00Z"),
            (
                "vault_modified_stamp",
                &header.modified_at().raw().to_string(),
            ),
            (
                "vault_size",
                &fs::metadata(&vault).unwrap().len().to_string(),
            ),
            (
                "vault_checksum",
                &content_checksum(&fs::read(&vault).unwrap()),
            ),
            ("decrypted_at", "2025-07-01T08:15:00Z"),
            ("entries_never_read", "9"),
        ]);
        fixture
    }

    fn db(&self) -> PathBuf {
        self.dir.path().join("b4.duckdb")
    }

    fn vault(&self) -> PathBuf {
        self.dir.path().join("vault.sub")
    }

    fn write(&self, meta: &[(&str, &str)]) {
        let connection = Connection::open(self.db()).unwrap();
        connection
            .execute_batch(
                "create table meta(key varchar primary key, value varchar);
                 create table warnings(message varchar);
                 insert into warnings values
                   ('skipped unknown entry Foo'), ('rounded holdings.quantity of record 7');

                 create table accounts(
                   id bigint, name varchar, currency varchar, balance decimal(18,4),
                   hidden boolean, opened date, updated timestamp, rate double);
                 comment on table accounts is 'One row per account';
                 comment on column accounts.id is 'Record id';
                 comment on column accounts.balance is 'Balance in the account''s currency';
                 insert into accounts values
                   (2, 'Savings, \"high\" yield', 'EUR', 10000.5000, false, date '2020-02-29',
                    timestamp '2025-06-30 23:59:59.5', 2.25),
                   (1, 'Giro', 'EUR', -12.3400, false, date '2019-01-15',
                    timestamp '2025-06-30 12:00:00', 0.1),
                   (3, null, null, null, null, null, null, null);

                 create table holdings(
                   id bigint, account_id bigint, quantity decimal(28,10), note varchar);
                 comment on table holdings is 'Securities held';
                 insert into holdings values
                   (7, 2, 123456789012345.1234567891, 'two' || chr(10) || 'lines'),
                   (7, 2, 1.5, ''),
                   (5, 1, 0.0000000001, null);

                 create table empty_table(id bigint, label varchar);

                 create view accounts_view as
                   select id, name from accounts where not hidden;
                 comment on view accounts_view is 'Visible accounts';",
            )
            .unwrap();
        for (key, value) in meta {
            connection
                .execute("insert into meta values (?, ?)", [key, value])
                .unwrap();
        }
    }

    /// Change the `meta` table with `sql`.
    fn change_meta(&self, sql: &str) {
        Connection::open(self.db())
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }

    /// `b4 <args>` on this database.
    fn b4(&self, args: &[&str]) -> Command {
        let mut command = b4(args);
        command.arg("--db").arg(self.db());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.b4(args).output().expect("cannot run b4")
    }

    /// `b4 sql <query> --output <format>`, which must succeed; its stdout.
    fn sql(&self, query: &str, format: &str) -> String {
        let output = self.run(&["sql", query, "--output", format]);
        assert_eq!(exit_code(&output), 0, "{query}:\n{}", stderr(&output));
        stdout(&output)
    }

    /// The names in the directory of the database, sorted.
    fn files(&self) -> Vec<String> {
        file_names(self.dir.path())
    }
}

/// One invocation of every command and verb that reads the database. They
/// all open it the same way, so they all fail the same way when they cannot.
const DATABASE_COMMANDS: [&[&str]; 15] = [
    &["sql", "select 1"],
    &["schema"],
    &["schema", "accounts"],
    &["status"],
    &["status", "--check"],
    &["accounts", "list"],
    &["accounts", "show", "1"],
    &["bookings", "list"],
    &["bookings", "show", "1"],
    &["bookings", "summary", "--by", "month"],
    &["payees", "list"],
    &["categories", "list"],
    &["standing-orders", "list"],
    &["portfolio", "list"],
    &["export", "out"],
];

/// A `b4` invocation that does not see the environment of the test run and
/// has no terminal on any stream.
fn b4(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_b4"));
    command
        .args(args)
        .env_remove("B4_DB")
        .env_remove("B4_PASSWORD")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .stdin(Stdio::null());
    command
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is not UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is not UTF-8")
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("b4 was killed by a signal")
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("not JSON ({error}):\n{text}"))
}

/// One value of every kind of type b4 maps, under the column names used in
/// the expectations below.
const EVERY_TYPE: &str = "select
    true as b, 1::tinyint as ti, 2::smallint as si, 3::integer as i, 4::bigint as bi,
    5::hugeint as hi, 9223372036854775808::hugeint as h65,
    170141183460469231731687303715884105727::hugeint as h127,
    18446744073709551615::ubigint as ubi, 6::uhugeint as uhi,
    1.5::float as f, 1.1::float as f2, 2.5::double as d,
    1.5::decimal(4,1) as dec, 12345678901234567890123456789012345.123::decimal(38,3) as dec38,
    date '2024-02-29' as dt, time '12:34:56.789' as tm,
    timestamp '2024-02-29 12:34:56.123456' as ts,
    '2024-02-29 12:34:56'::timestamp_s as tss,
    '2024-02-29 12:34:56.123456789'::timestamp_ns as tsns,
    timestamptz '2024-02-29 12:34:56+02' as tstz,
    interval '14 months 2 days 3 seconds' as iv,
    'text' as s, '\\xAA\\x00'::blob as bl, '101'::bit as bt,
    uuid '0189f3a4-0000-7000-8000-000000000001' as u,
    [1, 2, null] as l, {'a': 1.25::decimal(5,2), 'b': 'x \"y\"'} as st, map {'k': date '2024-01-01'} as m";

// --- the sandbox ---

/// Statements that would write the database, reach a file or the network,
/// load code or loosen the sandbox. `{dir}` is a directory the test owns,
/// `{db}` the path of the database.
const ESCAPES: &[&str] = &[
    "copy accounts to '{dir}/out.csv'",
    "copy (select 1) to '{dir}/out.parquet' (format parquet)",
    "copy accounts from '/etc/passwd'",
    "select * from read_csv('/etc/passwd')",
    "select * from read_text('/etc/passwd')",
    "select * from read_blob('/etc/passwd')",
    "select * from glob('/etc/*')",
    "select * from sniff_csv('/etc/passwd')",
    "select * from '/etc/passwd'",
    "attach '{dir}/other.duckdb' as other",
    "attach ':memory:' as other",
    "install httpfs",
    "load httpfs",
    "export database '{dir}/dump'",
    "import database '{dir}/dump'",
    "set threads = 1",
    "set enable_external_access = true",
    "set lock_configuration = false",
    "set disabled_filesystems = ''",
    "reset disabled_filesystems",
    "set memory_limit = '64GB'",
    "set temp_directory = '{dir}'",
    "set max_temp_directory_size = '10GB'",
    "reset threads",
    "insert into accounts (id) values (99)",
    "update accounts set name = 'x'",
    "delete from accounts",
    "create table escape(i integer)",
    "create view escape as select 1",
    "drop table accounts",
    "alter table accounts add column escape integer",
    "comment on table accounts is 'x'",
    "pragma threads = 2",
    "pragma memory_limit = '1GB'",
    "pragma profiling_output = '{dir}/profile.json'",
    "select 1; drop table accounts",
    // What DuckDB allows itself even without external access: the database
    // file, its write-ahead log and everything under `<database>.tmp`.
    // These got out before the file system was disabled.
    "select content from read_text('{db}.tmp/secret.txt')",
    "select content from read_text('{db}.tmp/preexisting.txt')",
    "select * from read_csv('{db}.tmp/secret.txt')",
    "select * from glob('{db}.tmp/*')",
    "select octet_length(content) from read_blob('{db}')",
    "select * from read_blob('{db}.tmp/secret.txt')",
    "copy (select 'PWNED') to '{db}.wal' (header false)",
    "copy (select 'PWNED') to '{db}.wal.checkpoint' (header false)",
    "copy (select 'PWNED') to '{db}.wal.recovery' (header false)",
    "copy (select repeat('A', 1000000) from range(8)) to '{db}.tmp/big.txt' (header false)",
    "copy (select 'PWNED') to '{db}.tmp/out.csv'",
    "copy (select 'PWNED') to '{db}'",
    "attach '{db}.tmp/other.duckdb' as other",
    "export database '{db}.tmp/dump'",
    "select current_setting('temp_directory'); copy (select 1) to '{db}.tmp/x.csv'",
];

/// The [`ESCAPES`] that fail with an ordinary error.
const FAIL_OTHERWISE: &[&str] = &[
    "select * from '/etc/passwd'",
    "copy (select 1) to '{dir}/out.parquet' (format parquet)",
];

/// Switches of DuckDB that are functions and not settings, so that the
/// locked configuration does not stop them, and `query`, which could hide
/// one. b4 refuses each by the name in the second place, before the
/// statement reaches the database.
const SWITCHES: &[(&str, &str)] = &[
    ("pragma enable_profiling", "enable_profiling"),
    ("PRAGMA Enable_Profiling; select * from accounts", "enable_profiling"),
    ("pragma enable_profiling = json", "enable_profiling"),
    ("call enable_profiling()", "enable_profiling"),
    ("select * from enable_profiling()", "enable_profiling"),
    (
        "select * from enable_profiling(format='json', save_location=>'{db}.tmp/prof.json')",
        "enable_profiling",
    ),
    (
        "select * from enable_profiling(save_location=>'{dir}/prof.json'); select 1",
        "enable_profiling",
    ),
    ("pragma \"enable_profile\"; select 1", "enable_profile"),
    ("pragma disable_profiling", "disable_profiling"),
    (
        "select * from enable_logging(storage='file', storage_path=>'{dir}/log.csv', level=>'info'); select 1",
        "enable_logging",
    ),
    (
        "select * from enable_logging(storage='file', storage_path=>'{db}.tmp/log.csv')",
        "enable_logging",
    ),
    ("call enable_logging(storage='stdout'); select 1", "enable_logging"),
    ("call enable_logging()", "enable_logging"),
    ("call disable_logging()", "disable_logging"),
    ("from truncate_duckdb_logs()", "truncate_duckdb_logs"),
    ("pragma enable_progress_bar; select 1", "enable_progress_bar"),
    (
        "pragma enable_print_progress_bar",
        "enable_print_progress_bar",
    ),
    ("pragma enable_verification; select 1", "enable_verification"),
    ("pragma disable_optimizer", "disable_optimizer"),
    (
        "select * from query('select * from enable_' || 'profiling()'); select 1",
        "query",
    ),
    ("from QUERY ('select 1')", "query"),
];

/// Everything in `dir` and below it, with the content of each file: what
/// "the directory is unchanged" compares.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            // A symbolic link is listed, not followed.
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_dir() {
                pending.push(path.clone());
                found.push((path, None));
            } else if kind.is_file() {
                let content = fs::read(&path).unwrap();
                found.push((path, Some(content)));
            } else {
                found.push((path, None));
            }
        }
    }
    found.sort();
    found
}

/// Run every query of `queries` with `b4 sql` on `fixture` and check that
/// it is refused: exit status 1 and not a signal, nothing on standard
/// output, an error on standard error. `judge` sees the statement as
/// written, the query that ran and the message.
fn assert_refused(
    fixture: &Fixture,
    outside: &Path,
    queries: &[&str],
    judge: impl Fn(&str, &str, &str),
) {
    for escape in queries {
        let query = escape
            .replace("{dir}", outside.to_str().unwrap())
            .replace("{db}", fixture.db().to_str().unwrap());
        for format in ["table", "json", "csv"] {
            let output = fixture.run(&["sql", &query, "--output", format]);
            let message = stderr(&output);
            assert_eq!(
                output.status.code(),
                Some(1),
                "{query}: {:?}\n{message}",
                output.status
            );
            assert_eq!(stdout(&output), "", "{query}");
            assert!(message.starts_with("error: "), "{query}:\n{message}");
            for secret in ["root:", "SECRET-BYTES", "PREEXISTING"] {
                assert!(!message.contains(secret), "{query} read a file:\n{message}");
            }
            judge(escape, &query, &message);
        }
    }
}

/// Whether `message` is a refusal by the sandbox with b4's explanation.
fn is_explained_refusal(message: &str) -> bool {
    message.ends_with(&format!("\n{READ_ONLY_NOTE}\n"))
        || (message.starts_with(&format!("error: {NO_FILE_SYSTEM}"))
            && message.ends_with(&format!("\n{READ_ONLY_NOTE}\n{MEMORY_NOTE}\n")))
}

#[test]
fn sql_refuses_every_way_out_of_the_sandbox() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let before = snapshot(fixture.dir.path());

    assert_refused(
        &fixture,
        outside.path(),
        ESCAPES,
        |escape, query, message| {
            // A few fail before the sandbox is asked: DuckDB looks for a table
            // of that name, for a format that only an extension has, or refuses
            // for a reason of its own.
            assert_eq!(
                is_explained_refusal(message),
                !FAIL_OTHERWISE.contains(&escape),
                "{query}:\n{message}"
            );
        },
    );

    assert_eq!(
        snapshot(fixture.dir.path()),
        before,
        "the directory of the database changed"
    );
    assert_eq!(fixture.files(), ["b4.duckdb"]);
    assert_eq!(file_names(outside.path()), Vec::<String>::new());
    // The data is all still there.
    assert_eq!(
        fixture.sql("select count(*) as n from accounts", "csv"),
        "n\n3\n"
    );
}

/// The directory DuckDB treats as its own, `<database>.tmp`, exists and
/// holds a file: before the file system was disabled a query could read and
/// write there.
#[test]
fn sql_cannot_reach_the_temporary_directory_of_the_database() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let temp = fixture.dir.path().join("b4.duckdb.tmp");
    fs::create_dir(&temp).unwrap();
    fs::write(temp.join("preexisting.txt"), "PREEXISTING\n").unwrap();
    fs::write(temp.join("secret.txt"), "SECRET-BYTES\n").unwrap();
    let before = snapshot(fixture.dir.path());

    assert_refused(
        &fixture,
        outside.path(),
        ESCAPES,
        |escape, query, message| {
            if !FAIL_OTHERWISE.contains(&escape) {
                assert!(is_explained_refusal(message), "{query}:\n{message}");
            }
        },
    );
    assert_refused(&fixture, outside.path(), &switch_queries(), |_, _, _| {});

    assert_eq!(snapshot(fixture.dir.path()), before);
    assert_eq!(file_names(outside.path()), Vec::<String>::new());
    // Ordinary work goes on next to it, and leaves it alone.
    assert_eq!(
        fixture.sql(
            "create temp table t as select * from accounts; select count(*) as n from t",
            "csv"
        ),
        "n\n3\n"
    );
    assert_eq!(snapshot(fixture.dir.path()), before);
}

/// Someone else planted `<database>.tmp` as a symbolic link to a directory
/// with a secret, as another user can in `/tmp`, or a repository can carry.
#[cfg(unix)]
#[test]
fn sql_does_not_follow_a_link_planted_as_the_temporary_directory() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    fs::write(target.path().join("secret.txt"), "SECRET-BYTES\n").unwrap();
    fs::write(target.path().join("preexisting.txt"), "PREEXISTING\n").unwrap();
    let link = fixture.dir.path().join("b4.duckdb.tmp");
    std::os::unix::fs::symlink(target.path(), &link).unwrap();
    let before = snapshot(fixture.dir.path());
    let target_before = snapshot(target.path());

    assert_refused(
        &fixture,
        outside.path(),
        ESCAPES,
        |escape, query, message| {
            if !FAIL_OTHERWISE.contains(&escape) {
                assert!(is_explained_refusal(message), "{query}:\n{message}");
            }
        },
    );
    assert_refused(&fixture, outside.path(), &switch_queries(), |_, _, _| {});
    // Queries that are allowed do not touch it either.
    assert_eq!(
        fixture.sql("select count(*) as n from accounts", "csv"),
        "n\n3\n"
    );

    // The link is still a link to the same place, and the directory behind
    // it holds what it held: nothing was read into a message (checked for
    // every statement), written or removed.
    assert_eq!(snapshot(fixture.dir.path()), before);
    assert_eq!(fs::read_link(&link).unwrap(), target.path());
    assert_eq!(snapshot(target.path()), target_before);
    assert_eq!(
        fs::read_to_string(target.path().join("secret.txt")).unwrap(),
        "SECRET-BYTES\n"
    );
    assert_eq!(file_names(outside.path()), Vec::<String>::new());
}

fn switch_queries() -> Vec<&'static str> {
    SWITCHES.iter().map(|(query, _)| *query).collect()
}

/// Calls of refused functions with something between the name and the
/// parenthesis that DuckDB reads as a blank or a comment, each with the
/// name it must be refused under.
const HIDDEN_CALLS: &[(&str, &str)] = &[
    // A zero-width space: with it, the statement turned logging on, and
    // `write_log` then wrote raw escape sequences to the terminal.
    (
        "select * from query\u{200b}('select * from enable_' || 'logging(storage=''stdout'')')",
        "the table function `query`",
    ),
    (
        "select * from query\u{feff}('select * from enable_' || 'logging(storage=''stdout'')')",
        "the table function `query`",
    ),
    (
        "select * from \"query\"\u{2060}\u{200b} ('select 1')",
        "the table function `query`",
    ),
    // A comment within a comment, and a comment that a carriage return ends.
    (
        "select * from query /* a /* b */ c */ ('select 1')",
        "the table function `query`",
    ),
    (
        "select * from query -- no\r('select 1')",
        "the table function `query`",
    ),
    (
        "select write_log(chr(27) || '[2J')",
        "the function `write_log`",
    ),
    (
        "select write_log\u{200b}(chr(27) || '[2J', level := 'error')",
        "the function `write_log`",
    ),
];

#[test]
fn sql_refuses_a_call_that_hides_behind_what_only_duckdb_reads_as_a_blank() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let before = snapshot(fixture.dir.path());
    let queries: Vec<&str> = HIDDEN_CALLS.iter().map(|(query, _)| *query).collect();

    assert_refused(
        &fixture,
        outside.path(),
        &queries,
        |escape, query, message| {
            let name = HIDDEN_CALLS
                .iter()
                .find(|(hidden, _)| *hidden == escape)
                .map(|(_, name)| *name)
                .unwrap();
            assert!(
                message.starts_with(&format!("error: {name} cannot be used in a query: ")),
                "{query:?}:\n{message}"
            );
            assert_eq!(message.lines().count(), 1, "{query:?}:\n{message}");
            assert!(!message.contains('\u{1b}'), "{query:?}");
        },
    );
    assert_eq!(snapshot(fixture.dir.path()), before);

    // The same characters after a column of that name hide nothing and
    // refuse nothing.
    assert_eq!(
        fixture.sql("select 1 as query\u{200b}, (2) as write_log", "csv"),
        "query,write_log\n1,2\n"
    );
}

#[test]
fn sql_refuses_the_switches_that_are_not_settings() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let before = snapshot(fixture.dir.path());

    assert_refused(
        &fixture,
        outside.path(),
        &switch_queries(),
        |escape, query, message| {
            let name = SWITCHES
                .iter()
                .find(|(switch, _)| *switch == escape)
                .map(|(_, name)| *name)
                .unwrap();
            // The message says which name was refused, and is b4's own: the
            // statement never reached the database.
            let expected = if name == "query" {
                "error: the table function `query` cannot be used in a query: ".to_owned()
            } else {
                format!("error: `{name}` cannot be used in a query: ")
            };
            assert!(message.starts_with(&expected), "{query}:\n{message}");
            assert_eq!(message.lines().count(), 1, "{query}:\n{message}");
        },
    );

    assert_eq!(snapshot(fixture.dir.path()), before);
    assert_eq!(file_names(outside.path()), Vec::<String>::new());

    // What the refusal protects: the output of a query is its result and
    // nothing else, in every format.
    assert_eq!(
        fixture.sql("select 1 as query, 'enable' as profiling", "json"),
        "[\n  {\"query\":1,\"profiling\":\"enable\"}\n]\n"
    );
    // Harmless relatives still work.
    let plan = fixture.sql("explain analyze select 1", "csv");
    assert!(
        plan.starts_with("explain_key,explain_value\nanalyzed_plan,"),
        "{plan}"
    );
    assert_eq!(
        fixture.sql("select count(*) as n from query_table('accounts')", "csv"),
        "n\n3\n"
    );
}

#[test]
fn sql_fails_cleanly_when_a_query_needs_too_much_memory() {
    let fixture = Fixture::new();
    let limit = fixture.sql("select current_setting('memory_limit') as m", "csv");
    assert!(
        ["m\n1.0 GiB\n", "m\n1024.0 MiB\n"].contains(&limit.as_str()) || limit.ends_with(" MiB\n"),
        "{limit}"
    );

    // Three gigabytes of text in one value, and two hundred million distinct
    // numbers: both more than the limit, and there is no disk to spill to.
    // The process must end with an error, not be killed, and say what the
    // limit is. DuckDB words the two failures differently.
    for (query, words) in [
        (
            "select length(string_agg(repeat('x', 1000000), '')) from range(3000)",
            "error: Out of Memory Error: ",
        ),
        (
            "select count(*) from (select distinct i from range(200000000) t(i))",
            "error: Permission Error: File system LocalFileSystem has been disabled",
        ),
    ] {
        let output = fixture.run(&["sql", query]);
        let message = stderr(&output);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{query}: {:?}\n{message}",
            output.status
        );
        assert_eq!(stdout(&output), "", "{query}");
        assert!(message.starts_with(words), "{query}:\n{message}");
        assert!(
            message.ends_with(&format!("\n{MEMORY_NOTE}\n")),
            "{query}:\n{message}"
        );
        assert_eq!(fixture.files(), ["b4.duckdb"], "{query}");
    }
}

#[test]
fn sql_streams_csv_and_json_but_caps_the_table() {
    let fixture = Fixture::new();
    let query = "select i, i::varchar as t from range(250000) r(i)";

    // csv, jsonl and json are written while the result arrives.
    let csv = fixture.sql(query, "csv");
    assert_eq!(csv.lines().count(), 250_001);
    assert!(csv.starts_with("i,t\n0,0\n1,1\n"), "{:.40}", csv);
    assert!(csv.ends_with("\n249999,249999\n"));
    let jsonl = fixture.sql(query, "jsonl");
    assert_eq!(jsonl.lines().count(), 250_000);
    assert!(jsonl.ends_with("{\"i\":249999,\"t\":\"249999\"}\n"));
    let rows = json(&fixture.sql(query, "json"));
    assert_eq!(rows.as_array().unwrap().len(), 250_000);
    assert_eq!(rows[249_999]["i"], 249_999);

    // The table holds at most 100000 rows, and says what to do instead.
    let output = fixture.run(&["sql", query]);
    assert_eq!(exit_code(&output), 1);
    assert_eq!(stdout(&output), "", "nothing is printed before the limit");
    assert_eq!(
        stderr(&output),
        "error: the result has more than 100000 rows, which is all the table format holds \
         in memory: use --output csv or --output jsonl, or add a LIMIT to the query\n"
    );
    let table = fixture.sql("select i from range(100000) r(i)", "table");
    assert_eq!(table.lines().count(), 100_001);

    // An error in the middle of a result: the rows before it are out, and
    // the exit status says that the output is incomplete.
    let output = fixture.run(&[
        "sql",
        "select case when i = 200000 then cast('x' as integer) else i end as i \
         from range(250000) r(i)",
        "--output",
        "csv",
    ]);
    assert_eq!(exit_code(&output), 1);
    assert!(stdout(&output).starts_with("i\n0\n1\n"));
    assert!(stderr(&output).starts_with("error: Conversion Error: "));
}

#[test]
fn a_query_on_standard_input_has_a_size_limit() {
    let fixture = Fixture::new();
    let mut child = fixture
        .b4(&["sql", "-", "--output", "csv"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // More than the megabyte allowed; b4 may stop reading before the end.
    let line = format!("-- {}\n", "x".repeat(1020));
    for _ in 0..1100 {
        if stdin.write_all(line.as_bytes()).is_err() {
            break;
        }
    }
    let _ = stdin.write_all(b"select 1\n");
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert_eq!(exit_code(&output), 1);
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "error: the query on standard input is longer than 1 MiB\n"
    );
}

// --- sql: text from the vault ---

/// A purpose as the sender of a transfer could write it: clear the screen,
/// with ESC and with the 8-bit CSI, and reverse the line.
const HOSTILE_SQL: &str = "'pay' || chr(27) || '[2J' || chr(155) || '2J' || chr(8238) || 'me'";
const HOSTILE: &str = "pay\x1b[2J\u{9b}2J\u{202e}me";

#[test]
fn an_error_message_never_carries_an_escape_sequence() {
    let fixture = Fixture::new();
    for query in [
        format!("select cast({HOSTILE_SQL} as integer)"),
        format!("select * from \"{HOSTILE}\""),
        format!("select ({HOSTILE_SQL})::date"),
    ] {
        let output = fixture.run(&["sql", &query]);
        assert_eq!(exit_code(&output), 1, "{query:?}");
        assert_eq!(stdout(&output), "");
        let message = stderr(&output);
        assert!(message.starts_with("error: "), "{message:?}");
        assert!(
            !message
                .chars()
                .any(|c| (c.is_control() && c != '\n' && c != '\t')
                    || ('\u{202a}'..='\u{202e}').contains(&c)
                    || ('\u{2066}'..='\u{2069}').contains(&c)),
            "{message:?}"
        );
        assert!(
            message.contains("pay\u{fffd}[2J\u{fffd}2J\u{fffd}me"),
            "{message:?}"
        );
    }
    // The same for the name of a table that `schema` and `export` are asked
    // for.
    let dir = tempfile::tempdir().unwrap();
    for args in [
        &["schema", "no\x1b[2Jpe"][..],
        &[
            "export",
            dir.path().to_str().unwrap(),
            "--table",
            "no\x1b[2Jpe",
        ],
    ] {
        let output = fixture.run(args);
        assert_eq!(exit_code(&output), 1);
        let message = stderr(&output);
        assert!(message.contains("`no\u{fffd}[2Jpe`"), "{message:?}");
        assert!(!message.contains('\x1b'), "{message:?}");
    }
}

#[test]
fn the_machine_formats_are_exact_when_they_are_not_written_to_a_terminal() {
    // Standard output of these runs is a pipe: data, byte for byte. (What a
    // terminal gets is tested with the renderers, which take the
    // destination as a flag.)
    let fixture = Fixture::new();
    let query = format!("select {HOSTILE_SQL} as purpose");
    assert_eq!(fixture.sql(&query, "csv"), format!("purpose\n{HOSTILE}\n"));
    assert_eq!(
        fixture.sql(&query, "jsonl"),
        "{\"purpose\":\"pay\\u001b[2J\u{9b}2J\u{202e}me\"}\n"
    );
    let rows = json(&fixture.sql(&query, "json"));
    assert_eq!(rows[0]["purpose"], HOSTILE);
    // The table is made for people and is cleaned wherever it goes.
    assert_eq!(
        fixture.sql(&query, "table"),
        "PURPOSE\npay\u{fffd}[2J\u{fffd}2J\u{fffd}me\n"
    );

    // An export is data too.
    Connection::open(fixture.db())
        .unwrap()
        .execute_batch(&format!(
            "create table hostile as select 1 as id, {HOSTILE_SQL} as purpose"
        ))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().join("out");
    for (format, expected) in [
        ("csv", format!("id,purpose\n1,{HOSTILE}\n")),
        (
            "jsonl",
            "{\"id\":1,\"purpose\":\"pay\\u001b[2J\u{9b}2J\u{202e}me\"}\n".to_owned(),
        ),
    ] {
        let output = export(&fixture, &dir, &["--format", format, "--table", "hostile"]);
        assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
        assert_eq!(
            fs::read_to_string(dir.join(format!("hostile.{format}"))).unwrap(),
            expected
        );
    }
}

#[test]
fn an_ordinary_error_does_not_get_the_read_only_note() {
    let fixture = Fixture::new();
    for query in [
        "select 1 +",
        "select * from nowhere",
        "select nothing from accounts",
    ] {
        let output = fixture.run(&["sql", query]);
        let message = stderr(&output);
        assert_eq!(exit_code(&output), 1, "{query}");
        assert_eq!(stdout(&output), "", "{query}");
        assert!(message.starts_with("error: "), "{query}:\n{message}");
        assert!(!message.contains("read-only"), "{query}:\n{message}");
    }
    // The database's own words are passed on.
    let message = stderr(&fixture.run(&["sql", "select 1 +"]));
    assert!(
        message.starts_with("error: Parser Error: syntax error at end of input"),
        "{message}"
    );
    let message = stderr(&fixture.run(&["sql", "select * from nowhere"]));
    assert!(
        message.starts_with("error: Catalog Error: Table with name nowhere does not exist!"),
        "{message}"
    );
}

#[test]
fn read_only_commands_leave_nothing_next_to_the_database() {
    let fixture = Fixture::with_vault();
    let before = fs::read(fixture.db()).unwrap();
    let export = tempfile::tempdir().unwrap();
    let export_dir = export.path().to_str().unwrap();

    for args in [
        &["sql", "select * from accounts order by id"][..],
        &[
            "sql",
            "create temp table t as select * from accounts; select count(*) from t",
        ],
        &["schema"],
        &["schema", "accounts"],
        &["status"],
        &["status", "--check"],
        &["export", export_dir],
    ] {
        let output = fixture.run(args);
        assert_eq!(exit_code(&output), 0, "{args:?}:\n{}", stderr(&output));
        assert_eq!(fixture.files(), ["b4.duckdb", "vault.sub"], "{args:?}");
    }
    assert_eq!(
        fs::read(fixture.db()).unwrap(),
        before,
        "the database changed"
    );
}

// --- sql: values ---

#[test]
fn decimal_sums_are_exact() {
    let fixture = Fixture::new();
    let query = "select 0.10::decimal(18,4) + 0.20::decimal(18,4) as total";
    // The table shows the scale of the type; the machine formats write the
    // exact value without trailing zeros, which is the rule of the output
    // module for every command.
    assert_eq!(fixture.sql(query, "table"), " TOTAL\n0.3000\n");
    assert_eq!(fixture.sql(query, "csv"), "total\n0.3\n");
    assert_eq!(fixture.sql(query, "json"), "[\n  {\"total\":0.3}\n]\n");
    assert_eq!(fixture.sql(query, "jsonl"), "{\"total\":0.3}\n");

    // The same over stored rows: a double would give 9988.160000000002.
    let query = "select sum(balance) as total, avg(rate) as rate from accounts";
    assert_eq!(
        fixture.sql(query, "table"),
        "    TOTAL   RATE\n9988.1600  1.175\n"
    );
    assert_eq!(
        fixture.sql(query, "jsonl"),
        "{\"total\":9988.16,\"rate\":1.175}\n"
    );
}

#[test]
fn a_decimal_of_25_digits_survives_in_every_format() {
    let fixture = Fixture::new();
    let digits = "123456789012345.1234567891";
    let query = format!("select {digits}::decimal(28,10) as quantity");
    assert_eq!(
        fixture.sql(&query, "table"),
        format!("{:>26}\n{digits}\n", "QUANTITY")
    );
    assert_eq!(fixture.sql(&query, "csv"), format!("quantity\n{digits}\n"));
    // More digits than a double carries: a string, not a number.
    assert_eq!(
        fixture.sql(&query, "json"),
        format!("[\n  {{\"quantity\":\"{digits}\"}}\n]\n")
    );
    assert_eq!(
        fixture.sql(&query, "jsonl"),
        format!("{{\"quantity\":\"{digits}\"}}\n")
    );

    // And when it comes out of a table.
    let stored = "select quantity from holdings order by quantity desc limit 1";
    assert_eq!(fixture.sql(stored, "csv"), format!("quantity\n{digits}\n"));
    // The smallest step of the type is not lost either.
    let smallest = "select min(quantity) as q, sum(quantity) as total from holdings";
    assert_eq!(
        fixture.sql(smallest, "csv"),
        "q,total\n0.0000000001,123456789012346.6234567892\n"
    );
}

#[test]
fn json_writes_only_numbers_a_double_can_hold() {
    let fixture = Fixture::new();
    // 2^53 is the last integer below which every integer is a double.
    let query = "select 9007199254740992 as at, 9007199254740993 as beyond, \
                 -9007199254740992 as minus_at, -9007199254740993 as minus_beyond, \
                 9223372036854775807 as max, (-9223372036854775807 - 1)::bigint as min, \
                 18446744073709551615::ubigint as unsigned, 42 as small";
    assert_eq!(
        fixture.sql(query, "jsonl"),
        "{\"at\":9007199254740992,\"beyond\":\"9007199254740993\",\
         \"minus_at\":-9007199254740992,\"minus_beyond\":\"-9007199254740993\",\
         \"max\":\"9223372036854775807\",\"min\":\"-9223372036854775808\",\
         \"unsigned\":\"18446744073709551615\",\"small\":42}\n"
    );
    let row = &json(&fixture.sql(query, "json"))[0];
    assert_eq!(row["at"], 9_007_199_254_740_992_i64);
    assert_eq!(row["beyond"], "9007199254740993");
    assert_eq!(row["min"], "-9223372036854775808");
    // The other formats have the digits as they are.
    assert_eq!(
        fixture.sql(query, "csv").lines().nth(1),
        Some(
            "9007199254740992,9007199254740993,-9007199254740992,-9007199254740993,\
             9223372036854775807,-9223372036854775808,18446744073709551615,42"
        )
    );

    // The same inside a list, a struct and a map, whose JSON text is one
    // string of the result.
    let nested = "select [9007199254740992, 9007199254740993, 9223372036854775807] as l, \
                  {'d': 12345678901234567890.123::decimal(38,3), 'e': 1234567890123.45::decimal(18,2), \
                   'h': 170141183460469231731687303715884105727::hugeint, 'i': -5::hugeint} as s, \
                  map {'k': (-9223372036854775807 - 1)::bigint} as m";
    let row = &json(&fixture.sql(nested, "json"))[0];
    assert_eq!(
        row["l"],
        "[9007199254740992,\"9007199254740993\",\"9223372036854775807\"]"
    );
    assert_eq!(
        row["s"],
        "{\"d\":\"12345678901234567890.123\",\"e\":1234567890123.45,\
         \"h\":\"170141183460469231731687303715884105727\",\"i\":-5}"
    );
    assert_eq!(row["m"], "{\"k\":\"-9223372036854775808\"}");
    // Each of them is JSON in its own right.
    for key in ["l", "s", "m"] {
        json(row[key].as_str().unwrap());
    }
}

#[test]
fn every_type_is_mapped_exactly() {
    let fixture = Fixture::new();
    let row = json(&fixture.sql(EVERY_TYPE, "jsonl"));
    let expected = serde_json::json!({
        "b": true, "ti": 1, "si": 2, "i": 3, "bi": 4,
        "hi": 5,
        "h65": "9223372036854775808",
        "h127": "170141183460469231731687303715884105727",
        "ubi": "18446744073709551615",
        "uhi": 6,
        "f": 1.5, "f2": 1.1, "d": 2.5,
        "dec": 1.5,
        "dec38": "12345678901234567890123456789012345.123",
        "dt": "2024-02-29",
        "tm": "12:34:56.789000",
        "ts": "2024-02-29T12:34:56.123456",
        "tss": "2024-02-29T12:34:56",
        "tsns": "2024-02-29T12:34:56.123456789",
        "tstz": "2024-02-29T10:34:56Z",
        "iv": "1 year 2 months 2 days 00:00:03",
        "s": "text",
        "bl": "aa00",
        "bt": "101",
        "u": "0189f3a4-0000-7000-8000-000000000001",
        "l": "[1,2,null]",
        "st": "{\"a\":1.25,\"b\":\"x \\\"y\\\"\"}",
        "m": "{\"k\":\"2024-01-01\"}",
    });
    assert_eq!(row, expected);

    // The keys come in the order of the query.
    let line = fixture.sql(EVERY_TYPE, "jsonl");
    assert!(line.starts_with("{\"b\":true,\"ti\":1,\"si\":2,"), "{line}");

    let csv = fixture.sql(EVERY_TYPE, "csv");
    assert_eq!(
        csv,
        "b,ti,si,i,bi,hi,h65,h127,ubi,uhi,f,f2,d,dec,dec38,dt,tm,ts,tss,tsns,tstz,iv,s,bl,bt,u,l,st,m\n\
         true,1,2,3,4,5,9223372036854775808,170141183460469231731687303715884105727,\
         18446744073709551615,6,1.5,1.1,2.5,1.5,12345678901234567890123456789012345.123,\
         2024-02-29,12:34:56.789000,2024-02-29T12:34:56.123456,2024-02-29T12:34:56,\
         2024-02-29T12:34:56.123456789,2024-02-29T10:34:56Z,1 year 2 months 2 days 00:00:03,\
         text,aa00,101,0189f3a4-0000-7000-8000-000000000001,\
         \"[1,2,null]\",\"{\"\"a\"\":1.25,\"\"b\"\":\"\"x \\\"\"y\\\"\"\"\"}\",\"{\"\"k\"\":\"\"2024-01-01\"\"}\"\n"
    );

    // In a table numbers are right-aligned and a decimal has its scale.
    let table = fixture.sql(
        "select 4::bigint as bi, 1.5::decimal(6,3) as dec, 2.5::double as d, \
         date '2024-02-29' as dt, timestamp '2024-02-29 12:34:56' as ts, true as b, 'x' as s \
         union all select 1234, -20.25, 0.125, null, null, false, null order by bi",
        "table",
    );
    assert_eq!(
        table,
        "  BI      DEC      D  DT          TS                   B      S\n\
         \x20  4    1.500    2.5  2024-02-29  2024-02-29 12:34:56  true   x\n\
         1234  -20.250  0.125                                   false\n"
    );
}

#[test]
fn null_of_every_type_is_null() {
    let fixture = Fixture::new();
    let query = "select null::boolean as b, null::bigint as i, null::hugeint as h, \
                 null::double as d, null::decimal(18,4) as dec, null::date as dt, \
                 null::timestamp as ts, null::timestamptz as tstz, null::time as tm, \
                 null::interval as iv, null::varchar as s, null::blob as bl, \
                 null::integer[] as l, null::struct(a integer) as st, \
                 null::map(varchar, integer) as m, null as n";
    let row = json(&fixture.sql(query, "jsonl"));
    let object = row.as_object().unwrap();
    assert_eq!(object.len(), 16);
    for (key, value) in object {
        assert!(value.is_null(), "{key} is {value}");
    }
    assert_eq!(
        fixture.sql(query, "csv"),
        "b,i,h,d,dec,dt,ts,tstz,tm,iv,s,bl,l,st,m,n\n,,,,,,,,,,,,,,,\n"
    );
    // A row of nothing is still a row.
    assert_eq!(
        fixture.sql("select null::bigint as i, null::varchar as s", "table"),
        "I  S\n\n"
    );

    // Stored NULLs, next to an empty text.
    assert_eq!(
        fixture.sql("select id, note from holdings order by id, note", "jsonl"),
        "{\"id\":5,\"note\":null}\n{\"id\":7,\"note\":\"\"}\n{\"id\":7,\"note\":\"two\\nlines\"}\n"
    );
}

#[test]
fn repeated_column_names_get_distinct_keys() {
    let fixture = Fixture::new();
    let query = "select 1 as sum, 2 as sum, 3 as \"SUM\", 4 as sum_2, count(*), count(*) \
                 from accounts";
    assert_eq!(
        fixture.sql(query, "jsonl"),
        "{\"sum\":1,\"sum_3\":2,\"SUM\":3,\"sum_2\":4,\"count_star()\":3,\"count_star()_2\":3}\n"
    );
    assert_eq!(
        fixture.sql(query, "csv"),
        "sum,sum_3,SUM,sum_2,count_star(),count_star()_2\n1,2,3,4,3,3\n"
    );
    assert_eq!(
        fixture.sql("select 1 as sum, 2 as sum", "json"),
        "[\n  {\"sum\":1,\"sum_2\":2}\n]\n"
    );
}

#[test]
fn an_empty_result_in_every_format() {
    let fixture = Fixture::new();
    let query = "select id, name, balance from accounts where id < 0";

    let output = fixture.run(&["sql", query]);
    assert_eq!(exit_code(&output), 0);
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "no rows\n");

    let output = fixture.run(&["sql", query, "--quiet"]);
    assert_eq!(
        (stdout(&output), stderr(&output)),
        (String::new(), String::new())
    );

    for (format, expected) in [
        ("json", "[]\n"),
        ("jsonl", ""),
        ("csv", "id,name,balance\n"),
    ] {
        let output = fixture.run(&["sql", query, "--output", format]);
        assert_eq!(exit_code(&output), 0, "{format}");
        assert_eq!(stdout(&output), expected, "{format}");
        assert_eq!(stderr(&output), "", "{format}");
    }
    let output = fixture.run(&["sql", query, "--output", "csv", "--no-header"]);
    assert_eq!(stdout(&output), "");

    // A table without rows is the same case.
    assert_eq!(
        fixture.sql("select * from empty_table", "csv"),
        "id,label\n"
    );
}

// --- sql: the query text ---

#[test]
fn a_dash_reads_the_query_from_stdin() {
    let fixture = Fixture::new();
    let run = |input: &str| {
        let mut child = fixture
            .b4(&["sql", "-", "--output", "csv"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };

    let output = run("-- balances\nselect name, balance\nfrom accounts\nwhere id = 1;\n");
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(stdout(&output), "name,balance\nGiro,-12.34\n");

    for nothing in ["", "  \n", ";"] {
        let output = run(nothing);
        assert_eq!(exit_code(&output), 1, "{nothing:?}");
        assert_eq!(stdout(&output), "");
        assert_eq!(
            stderr(&output),
            "error: the query is empty\n",
            "{nothing:?}"
        );
    }
    let output = run("-- only a comment\n");
    assert_eq!(exit_code(&output), 1);
    assert!(
        stderr(&output).starts_with("error: "),
        "{}",
        stderr(&output)
    );
}

#[test]
fn of_several_statements_the_last_result_is_printed() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.sql("select 1 as first; select 2 as second", "csv"),
        "second\n2\n"
    );
    // Temporary objects live in memory and help to build a query.
    assert_eq!(
        fixture.sql(
            "create temp table big as select * from accounts where balance > 0; \
             create temp macro double(x) as x * 2; \
             select name, double(balance) as twice from big",
            "csv"
        ),
        "name,twice\n\"Savings, \"\"high\"\" yield\",20001\n"
    );
    // An error in an earlier statement stops the run.
    let output = fixture.run(&["sql", "select * from nowhere; select 1"]);
    assert_eq!(exit_code(&output), 1);
    assert_eq!(stdout(&output), "");
}

#[test]
fn text_is_data_and_never_sql() {
    let fixture = Fixture::new();
    // Quotes, line breaks and separators come out as the formats demand.
    assert_eq!(
        fixture.sql("select name from accounts where id = 2", "csv"),
        "name\n\"Savings, \"\"high\"\" yield\"\n"
    );
    assert_eq!(
        fixture.sql(
            "select note from holdings where id = 7 and note <> ''",
            "table"
        ),
        "NOTE\ntwo ↵ lines\n"
    );
}

// --- opening the database ---

#[test]
fn a_missing_database_says_how_to_get_one() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("none.duckdb");
    for args in DATABASE_COMMANDS {
        let output = b4(args)
            .arg("--db")
            .arg(&missing)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(exit_code(&output), 1, "{args:?}");
        assert_eq!(stdout(&output), "", "{args:?}");
        assert_eq!(
            stderr(&output),
            format!(
                "error: there is no database at {}: run `b4 decrypt <VAULT>` to create it, or \
                 name an existing one with --db or B4_DB\n",
                missing.display()
            ),
            "{args:?}"
        );
    }
    // Looking for it did not create it, nor anything else.
    assert_eq!(file_names(dir.path()), Vec::<String>::new());

    // B4_DB names the database as well.
    let output = b4(&["schema"]).env("B4_DB", &missing).output().unwrap();
    assert!(
        stderr(&output).contains("none.duckdb"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_file_that_is_not_a_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cases: [(&str, &[u8]); 4] = [
        ("empty.duckdb", b""),
        ("short.duckdb", b"DUCK"),
        (
            "text.duckdb",
            b"this is a text file, not a database, and long enough",
        ),
        ("vault.duckdb", &fs::read(DEMO_VAULT).unwrap()),
    ];
    for (name, content) in cases {
        let path = dir.path().join(name);
        fs::write(&path, content).unwrap();
        for args in DATABASE_COMMANDS {
            let output = b4(args)
                .arg("--db")
                .arg(&path)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert_eq!(exit_code(&output), 1, "{name} {args:?}");
            assert_eq!(stdout(&output), "", "{name} {args:?}");
            assert_eq!(
                stderr(&output),
                format!(
                    "error: {} is not a DuckDB database: run `b4 decrypt <VAULT>` to create \
                     one, or name another file with --db or B4_DB\n",
                    path.display()
                ),
                "{name} {args:?}"
            );
        }
        assert_eq!(fs::read(&path).unwrap(), content, "{name} changed");
    }
    // A directory is not one either.
    let output = b4(&["schema"])
        .arg("--db")
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(exit_code(&output), 1);
    assert!(
        stderr(&output).starts_with("error: "),
        "{}",
        stderr(&output)
    );
    assert_eq!(file_names(dir.path()).len(), 4);
}

#[test]
fn a_database_of_another_schema_version_is_refused() {
    let fixture = Fixture {
        dir: tempfile::tempdir().unwrap(),
    };
    fixture.write(&[("schema_version", "999"), ("b4_version", "9.9.9")]);
    for args in DATABASE_COMMANDS {
        let output = fixture
            .b4(args)
            .current_dir(fixture.dir.path())
            .output()
            .unwrap();
        assert_eq!(exit_code(&output), 1, "{args:?}");
        assert_eq!(stdout(&output), "", "{args:?}");
        assert_eq!(
            stderr(&output),
            format!(
                "error: the database {} has schema version 999 (written by b4 9.9.9), but \
                 this b4 reads schema version {SCHEMA_VERSION}: run `b4 decrypt <VAULT>` to \
                 write it again\n",
                fixture.db().display()
            ),
            "{args:?}"
        );
    }

    // No version at all.
    let fixture = Fixture {
        dir: tempfile::tempdir().unwrap(),
    };
    fixture.write(&[("b4_version", "0.0.1")]);
    let message = stderr(&fixture.run(&["schema"]));
    assert!(
        message.contains("does not say which schema version it has")
            && message.contains("run `b4 decrypt <VAULT>` to write it again"),
        "{message}"
    );

    // A DuckDB file that b4 did not write.
    let dir = tempfile::tempdir().unwrap();
    let foreign = dir.path().join("foreign.duckdb");
    Connection::open(&foreign)
        .unwrap()
        .execute_batch("create table t(i integer)")
        .unwrap();
    for args in DATABASE_COMMANDS {
        let output = b4(args)
            .arg("--db")
            .arg(&foreign)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(exit_code(&output), 1, "{args:?}");
        assert_eq!(stdout(&output), "", "{args:?}");
        assert_eq!(
            stderr(&output),
            format!(
                "error: {} is not a database written by b4 (it has no `meta` table): run \
                 `b4 decrypt <VAULT>` to write it again\n",
                foreign.display()
            ),
            "{args:?}"
        );
    }
    assert_eq!(file_names(dir.path()), ["foreign.duckdb"]);
}

#[test]
fn two_processes_can_query_at_the_same_time() {
    let fixture = Fixture::new();
    // A third reader holds the file open for the whole test, so the readers
    // below overlap with it for certain, and with each other most likely.
    let config = duckdb::Config::default()
        .access_mode(duckdb::AccessMode::ReadOnly)
        .unwrap();
    let holder = Connection::open_with_flags(fixture.db(), config).unwrap();

    let query = "select count(*) as n from accounts, range(2000000)";
    let children: Vec<_> = (0..4)
        .map(|_| {
            fixture
                .b4(&["sql", query, "--output", "csv"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
        assert_eq!(stdout(&output), "n\n6000000\n");
    }
    let count: i64 = holder
        .query_row("select count(*) from accounts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 3);
}

// --- schema ---

#[test]
fn schema_lists_tables_and_views_with_rows_and_descriptions() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture_stdout(&fixture, &["schema"]),
        "TABLE          TYPE   ROWS  DESCRIPTION\n\
         accounts       table     3  One row per account\n\
         accounts_view  view      2  Visible accounts\n\
         empty_table    table     0\n\
         holdings       table     3  Securities held\n\
         meta           table    10\n\
         warnings       table     2\n"
    );
    assert_eq!(
        fixture_stdout(&fixture, &["schema", "--output", "csv", "--no-header"]),
        "accounts,table,3,One row per account\n\
         accounts_view,view,2,Visible accounts\n\
         empty_table,table,0,\n\
         holdings,table,3,Securities held\n\
         meta,table,10,\n\
         warnings,table,2,\n"
    );
    let listed = json(&fixture_stdout(&fixture, &["schema", "--output", "json"]));
    assert_eq!(listed.as_array().unwrap().len(), 6);
    assert_eq!(
        listed[0],
        serde_json::json!({
            "name": "accounts", "type": "table", "rows": 3,
            "description": "One row per account"
        })
    );
    assert_eq!(listed[2]["description"], serde_json::Value::Null);
    let lines = fixture_stdout(&fixture, &["schema", "--output", "jsonl"]);
    assert_eq!(lines.lines().count(), 6);
    assert_eq!(
        lines.lines().nth(1).unwrap(),
        "{\"name\":\"accounts_view\",\"type\":\"view\",\"rows\":2,\"description\":\"Visible accounts\"}"
    );
}

#[test]
fn schema_of_a_table_lists_its_columns() {
    let fixture = Fixture::new();
    let expected = "COLUMN    TYPE           DESCRIPTION\n\
                    id        BIGINT         Record id\n\
                    name      VARCHAR\n\
                    currency  VARCHAR\n\
                    balance   DECIMAL(18,4)  Balance in the account's currency\n\
                    hidden    BOOLEAN\n\
                    opened    DATE\n\
                    updated   TIMESTAMP\n\
                    rate      DOUBLE\n";
    assert_eq!(fixture_stdout(&fixture, &["schema", "accounts"]), expected);
    // The case of letters does not matter, as in SQL.
    assert_eq!(fixture_stdout(&fixture, &["schema", "Accounts"]), expected);

    assert_eq!(
        fixture_stdout(&fixture, &["schema", "accounts_view", "--output", "csv"]),
        "name,type,description\nid,BIGINT,\nname,VARCHAR,\n"
    );
    assert_eq!(
        fixture_stdout(&fixture, &["schema", "holdings", "--output", "jsonl"]),
        "{\"name\":\"id\",\"type\":\"BIGINT\",\"description\":null}\n\
         {\"name\":\"account_id\",\"type\":\"BIGINT\",\"description\":null}\n\
         {\"name\":\"quantity\",\"type\":\"DECIMAL(28,10)\",\"description\":null}\n\
         {\"name\":\"note\",\"type\":\"VARCHAR\",\"description\":null}\n"
    );
    let columns = json(&fixture_stdout(
        &fixture,
        &["schema", "meta", "--output", "json"],
    ));
    assert_eq!(columns.as_array().unwrap().len(), 2);
}

#[test]
fn schema_of_an_unknown_table_lists_the_tables() {
    let fixture = Fixture::new();
    for name in ["bookings", "accounts; drop table accounts", "accounts\" --"] {
        let output = fixture.run(&["schema", name]);
        assert_eq!(exit_code(&output), 1, "{name}");
        assert_eq!(stdout(&output), "", "{name}");
        assert_eq!(
            stderr(&output),
            format!(
                "error: no table or view named `{name}`: there are accounts, accounts_view, \
                 empty_table, holdings, meta, warnings\n"
            )
        );
    }
}

fn fixture_stdout(fixture: &Fixture, args: &[&str]) -> String {
    let output = fixture.run(args);
    assert_eq!(exit_code(&output), 0, "{args:?}:\n{}", stderr(&output));
    assert_eq!(stderr(&output), "", "{args:?}");
    stdout(&output)
}

// --- status ---

/// Overwrite `bytes.len()` bytes of the vault at `offset`.
fn patch_vault(fixture: &Fixture, offset: usize, change: impl Fn(&mut [u8])) {
    let mut bytes = fs::read(fixture.vault()).unwrap();
    change(&mut bytes[offset..]);
    fs::write(fixture.vault(), bytes).unwrap();
}

fn status_json(fixture: &Fixture) -> serde_json::Value {
    let output = fixture.run(&["status", "--output", "json"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    json(&stdout(&output))
}

/// Exit status, stdout and stderr of `b4 status --check [more]`.
fn check(fixture: &Fixture, more: &[&str]) -> (i32, String, String) {
    let mut args = vec!["status", "--check"];
    args.extend_from_slice(more);
    let output = fixture.run(&args);
    (exit_code(&output), stdout(&output), stderr(&output))
}

#[test]
fn status_describes_an_up_to_date_database() {
    let fixture = Fixture::with_vault();
    let db = fixture.db();
    let vault = fixture.vault();
    let header = b4::vault::Header::read(&vault).unwrap();
    let size = fs::metadata(&db).unwrap().len();

    let output = fixture.run(&["status"]);
    assert_eq!(exit_code(&output), 0);
    assert_eq!(stderr(&output), "");
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    let expected = [
        format!("Database:        {}", db.display()),
        "Size:".to_owned(),
        "Written by:      b4 0.1.0, schema version 1".to_owned(),
        "Decrypted:       2025-07-01T08:15:00Z".to_owned(),
        format!("Vault:           {}", vault.display()),
        format!("Vault ID:        {}", header.file_guid()),
        "Vault modified:  2025-06-30T12:00:00Z".to_owned(),
        "Never read:      9 entries with bank access data".to_owned(),
        "Rows:            accounts     3".to_owned(),
        "                 empty_table  0".to_owned(),
        "                 holdings     3".to_owned(),
        "Warnings:        2 (listed by `b4 status --output json`)".to_owned(),
        "Compared:        vault id, modification stamp, size, content checksum".to_owned(),
        "Status:          up to date".to_owned(),
    ];
    assert_eq!(lines.len(), expected.len(), "{text}");
    for (line, expected) in lines.iter().zip(&expected) {
        assert!(
            line.starts_with(expected.as_str()),
            "{line:?} / {expected:?}"
        );
    }
    // No stored warning text in the table: it can be long.
    assert!(!text.contains("skipped unknown entry"), "{text}");

    let report = status_json(&fixture);
    assert_eq!(
        report,
        serde_json::json!({
            "database": db.to_str().unwrap(),
            "size_bytes": size,
            "b4_version": "0.1.0",
            "schema_version": "1",
            "decrypted_at": "2025-07-01T08:15:00Z",
            "vault_path": vault.to_str().unwrap(),
            "vault_id": header.file_guid().to_string(),
            "vault_created": null,
            "vault_modified": "2025-06-30T12:00:00Z",
            "entries_never_read": 9,
            "state": "up_to_date",
            "up_to_date": true,
            "state_detail": null,
            "checks": ["vault_id", "modified_stamp", "size", "checksum"],
            "rows": 6,
            "tables": {"accounts": 3, "empty_table": 0, "holdings": 3},
            "warnings": [
                "skipped unknown entry Foo",
                "rounded holdings.quantity of record 7"
            ],
        })
    );
    // The keys are in a fixed order, not sorted.
    let raw = stdout(&fixture.run(&["status", "--output", "json"]));
    assert!(raw.starts_with("{\n  \"database\": "), "{raw}");
    assert!(raw.find("\"size_bytes\"").unwrap() < raw.find("\"b4_version\"").unwrap());

    let line = stdout(&fixture.run(&["status", "--output", "jsonl"]));
    assert_eq!(line.lines().count(), 1);
    assert_eq!(json(&line), report);

    let csv = stdout(&fixture.run(&["status", "--output", "csv"]));
    assert_eq!(
        csv,
        format!(
            "database,size_bytes,b4_version,schema_version,decrypted_at,vault_path,vault_id,\
             vault_created,vault_modified,entries_never_read,state,up_to_date,state_detail,\
             checks,rows,tables,warnings\n\
             {},{size},0.1.0,1,2025-07-01T08:15:00Z,{},{},,2025-06-30T12:00:00Z,9,up_to_date,\
             true,,vault_id modified_stamp size checksum,6,3,2\n",
            db.display(),
            vault.display(),
            header.file_guid()
        )
    );

    assert_eq!(check(&fixture, &[]).0, 0);
    assert_eq!(check(&fixture, &[]).1, text);
    // Quiet and up to date: nothing at all.
    assert_eq!(
        check(&fixture, &["--quiet"]),
        (0, String::new(), String::new())
    );
    assert_eq!(
        check(&fixture, &["-q", "-o", "json"]),
        (0, String::new(), String::new())
    );
}

#[test]
fn status_sees_that_the_vault_changed() {
    let fixture = Fixture::with_vault();
    // Banking4 writes the time into the header when it closes the vault.
    patch_vault(&fixture, MODIFIED_AT_OFFSET, |stamp| {
        stamp[..4].copy_from_slice(&0x3100_0000_i32.to_le_bytes());
    });

    let output = fixture.run(&["status"]);
    assert_eq!(
        exit_code(&output),
        0,
        "without --check the status is not an error"
    );
    assert!(
        stdout(&output).ends_with("Status:          vault changed since decrypt\n"),
        "{}",
        stdout(&output)
    );
    let report = status_json(&fixture);
    assert_eq!(report["state"], "vault_changed");
    assert_eq!(report["up_to_date"], false);
    assert_eq!(report["state_detail"], serde_json::Value::Null);
    // The stamp decided: the file was not read for its checksum.
    assert_eq!(
        report["checks"],
        serde_json::json!(["vault_id", "modified_stamp", "size"])
    );
    assert!(
        stdout(&output).contains("Compared:        vault id, modification stamp, size\n"),
        "{}",
        stdout(&output)
    );

    let (code, out, err) = check(&fixture, &[]);
    assert_eq!(code, 1);
    assert!(
        out.ends_with("Status:          vault changed since decrypt\n"),
        "{out}"
    );
    assert_eq!(err, "", "a stale database is a result, not an error");
    assert_eq!(
        check(&fixture, &["--quiet"]),
        (
            1,
            String::new(),
            "warning: vault changed since decrypt\n".to_owned()
        )
    );
    let (code, out, _) = check(&fixture, &["--output", "json"]);
    assert_eq!(
        (code, &json(&out)["state"]),
        (1, &serde_json::json!("vault_changed"))
    );
}

#[test]
fn status_sees_a_vault_that_grew_with_the_same_stamp() {
    let fixture = Fixture::with_vault();
    let mut bytes = fs::read(fixture.vault()).unwrap();
    bytes.extend_from_slice(&[0u8; 512]);
    fs::write(fixture.vault(), bytes).unwrap();
    assert_eq!(status_json(&fixture)["state"], "vault_changed");
    assert_eq!(check(&fixture, &["-q"]).0, 1);
}

/// The header stamp has a resolution of one second and is only rewritten
/// when Banking4 closes the vault: a change can leave stamp and size as
/// they were. Only the content tells.
#[test]
fn status_sees_one_changed_byte_with_the_same_stamp_and_size() {
    let fixture = Fixture::with_vault();
    let before = fs::metadata(fixture.vault()).unwrap().len();
    assert_eq!(status_json(&fixture)["state"], "up_to_date");

    // One byte in the last cluster, far from the header.
    let last = usize::try_from(before).unwrap() - 1;
    patch_vault(&fixture, last, |bytes| bytes[0] ^= 0x01);
    assert_eq!(fs::metadata(fixture.vault()).unwrap().len(), before);

    let report = status_json(&fixture);
    assert_eq!(report["state"], "vault_changed");
    assert_eq!(report["up_to_date"], false);
    assert_eq!(
        report["checks"],
        serde_json::json!(["vault_id", "modified_stamp", "size", "checksum"])
    );
    let (code, out, err) = check(&fixture, &[]);
    assert_eq!((code, err.as_str()), (1, ""));
    assert!(
        out.ends_with(
            "Compared:        vault id, modification stamp, size, content checksum\n\
             Status:          vault changed since decrypt\n"
        ),
        "{out}"
    );
    assert_eq!(
        check(&fixture, &["--quiet"]),
        (
            1,
            String::new(),
            "warning: vault changed since decrypt\n".to_owned()
        )
    );

    // Changed back, it is the same vault again.
    patch_vault(&fixture, last, |bytes| bytes[0] ^= 0x01);
    assert_eq!(check(&fixture, &["-q"]).0, 0);
}

/// A recorded value that is missing or unreadable must never count as a
/// match: the database is then not "up to date" but incomplete.
#[test]
fn status_never_takes_a_missing_recorded_value_for_a_match() {
    for (change, keys, checks) in [
        (
            "delete from meta where key = 'vault_size'",
            "vault_size",
            serde_json::json!(["vault_id", "modified_stamp"]),
        ),
        (
            "update meta set value = 'nope' where key = 'vault_size'",
            "vault_size",
            serde_json::json!(["vault_id", "modified_stamp"]),
        ),
        (
            "update meta set value = '' where key = 'vault_id'",
            "vault_id",
            serde_json::json!(["modified_stamp", "size"]),
        ),
        (
            "delete from meta where key = 'vault_id'",
            "vault_id",
            serde_json::json!(["modified_stamp", "size"]),
        ),
        (
            "update meta set value = 'soon' where key = 'vault_modified_stamp'",
            "vault_modified_stamp",
            serde_json::json!(["vault_id", "size"]),
        ),
        (
            "delete from meta where key = 'vault_checksum'",
            "vault_checksum",
            serde_json::json!(["vault_id", "modified_stamp", "size"]),
        ),
        (
            "update meta set value = 'xyz' where key = 'vault_checksum'",
            "vault_checksum",
            serde_json::json!(["vault_id", "modified_stamp", "size"]),
        ),
        (
            "delete from meta where key in ('vault_size', 'vault_id', 'vault_checksum')",
            "vault_id, vault_size, vault_checksum",
            serde_json::json!(["modified_stamp"]),
        ),
    ] {
        let fixture = Fixture::with_vault();
        fixture.change_meta(change);
        let detail = format!(
            "the database does not record {keys}: run `b4 decrypt <VAULT>` to write it again"
        );

        let report = status_json(&fixture);
        assert_eq!(report["state"], "metadata_incomplete", "{change}");
        assert_eq!(report["up_to_date"], false, "{change}");
        assert_eq!(report["state_detail"], detail.as_str(), "{change}");
        assert_eq!(report["checks"], checks, "{change}");

        let output = fixture.run(&["status"]);
        assert_eq!(exit_code(&output), 0, "{change}");
        assert!(
            stdout(&output).ends_with(&format!(
                "Status:          database metadata incomplete: {detail}\n"
            )),
            "{change}:\n{}",
            stdout(&output)
        );
        assert_eq!(
            check(&fixture, &["--quiet"]),
            (
                1,
                String::new(),
                format!("warning: database metadata incomplete: {detail}\n")
            ),
            "{change}"
        );
    }

    // The case of the review: the size was not recorded, the stamp is the
    // same, and the vault has grown. That was "up to date".
    let fixture = Fixture::with_vault();
    fixture.change_meta("update meta set value = 'nope' where key = 'vault_size'");
    let mut bytes = fs::read(fixture.vault()).unwrap();
    bytes.extend_from_slice(&[0u8; 512]);
    fs::write(fixture.vault(), bytes).unwrap();
    assert_eq!(status_json(&fixture)["state"], "metadata_incomplete");
    assert_eq!(check(&fixture, &["-q"]).0, 1);

    // Without a recorded id, another file with the same stamp and size is
    // not "up to date" either.
    let fixture = Fixture::with_vault();
    fixture.change_meta("delete from meta where key = 'vault_id'");
    patch_vault(&fixture, FILE_GUID_OFFSET, |guid| guid[0] ^= 0xff);
    assert_eq!(status_json(&fixture)["state"], "metadata_incomplete");
    assert_eq!(check(&fixture, &["-q"]).0, 1);

    // A difference that can still be seen is reported as what it is.
    let fixture = Fixture::with_vault();
    fixture.change_meta("delete from meta where key = 'vault_checksum'");
    patch_vault(&fixture, MODIFIED_AT_OFFSET, |stamp| stamp[0] ^= 0xff);
    assert_eq!(status_json(&fixture)["state"], "vault_changed");
}

#[test]
fn status_sees_another_vault_at_the_path() {
    let fixture = Fixture::with_vault();
    patch_vault(&fixture, FILE_GUID_OFFSET, |guid| guid[0] ^= 0xff);

    let output = fixture.run(&["status"]);
    assert_eq!(exit_code(&output), 0);
    assert!(
        stdout(&output).ends_with("Status:          another vault is at that path now\n"),
        "{}",
        stdout(&output)
    );
    let report = status_json(&fixture);
    assert_eq!(report["state"], "other_vault");
    assert_eq!(report["up_to_date"], false);
    assert_eq!(report["checks"], serde_json::json!(["vault_id"]));
    assert_eq!(
        check(&fixture, &["--quiet"]),
        (
            1,
            String::new(),
            "warning: another vault is at that path now\n".to_owned()
        )
    );

    // The id decides before the stamp does.
    patch_vault(&fixture, MODIFIED_AT_OFFSET, |stamp| stamp[0] ^= 0xff);
    assert_eq!(status_json(&fixture)["state"], "other_vault");
}

#[test]
fn status_copes_with_a_vault_that_is_gone() {
    let fixture = Fixture::with_vault();
    fs::remove_file(fixture.vault()).unwrap();

    let output = fixture.run(&["status"]);
    assert_eq!(exit_code(&output), 0);
    assert_eq!(stderr(&output), "");
    let text = stdout(&output);
    let last = text.lines().last().unwrap();
    assert!(
        last.starts_with("Status:          vault not reachable: cannot read vault file "),
        "{last}"
    );
    // Everything about the database itself is still shown.
    assert!(text.contains("Rows:            accounts     3\n"), "{text}");

    let report = status_json(&fixture);
    assert_eq!(report["state"], "vault_unreachable");
    assert_eq!(report["up_to_date"], false);
    assert!(
        report["state_detail"]
            .as_str()
            .unwrap()
            .contains("vault.sub"),
        "{report}"
    );
    assert_eq!(report["rows"], 6);
    assert_eq!(report["checks"], serde_json::json!([]));
    assert!(text.contains("Compared:        nothing\n"), "{text}");

    let (code, out, err) = check(&fixture, &["--quiet"]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.starts_with("warning: vault not reachable: "), "{err}");
    assert_eq!(check(&fixture, &[]).0, 1);

    // A file that is no vault is not reachable as a vault either.
    fs::write(fixture.vault(), b"not a vault").unwrap();
    assert_eq!(status_json(&fixture)["state"], "vault_unreachable");
    assert_eq!(check(&fixture, &["-q"]).0, 1);
}

#[test]
fn status_without_warnings_or_vault_facts() {
    let fixture = Fixture {
        dir: tempfile::tempdir().unwrap(),
    };
    fixture.write(&[("schema_version", SCHEMA_VERSION)]);
    Connection::open(fixture.db())
        .unwrap()
        .execute_batch("delete from warnings")
        .unwrap();

    let text = stdout(&fixture.run(&["status"]));
    assert!(
        text.contains("Written by:      schema version 1\n"),
        "{text}"
    );
    assert!(text.contains("Warnings:        none\n"), "{text}");
    assert!(
        text.ends_with(
            "Status:          vault not reachable: the database does not say where its vault is\n"
        ),
        "{text}"
    );
    let report = status_json(&fixture);
    assert_eq!(report["warnings"], serde_json::json!([]));
    assert_eq!(report["vault_path"], serde_json::Value::Null);
    assert_eq!(report["entries_never_read"], serde_json::Value::Null);
    assert_eq!(check(&fixture, &["-q"]).0, 1);
}

// --- export ---

const ACCOUNTS_CSV: &str = "id,name,currency,balance,hidden,opened,updated,rate\n\
     1,Giro,EUR,-12.34,false,2019-01-15,2025-06-30T12:00:00,0.1\n\
     2,\"Savings, \"\"high\"\" yield\",EUR,10000.5,false,2020-02-29,2025-06-30T23:59:59.500000,2.25\n\
     3,,,,,,,\n";

const HOLDINGS_JSONL: &str = "{\"id\":5,\"account_id\":1,\"quantity\":0.0000000001,\"note\":null}\n\
     {\"id\":7,\"account_id\":2,\"quantity\":1.5,\"note\":\"\"}\n\
     {\"id\":7,\"account_id\":2,\"quantity\":\"123456789012345.1234567891\",\"note\":\"two\\nlines\"}\n";

fn export(fixture: &Fixture, dir: &Path, more: &[&str]) -> Output {
    let mut args = vec!["export", dir.to_str().unwrap()];
    args.extend_from_slice(more);
    fixture.run(&args)
}

#[test]
fn export_writes_every_table_as_csv() {
    let fixture = Fixture::new();
    let parent = tempfile::tempdir().unwrap();
    // The directory, and the one above it, are created.
    let dir = parent.path().join("a").join("export");

    let output = export(&fixture, &dir, &[]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        format!(
            "{0}/accounts.csv: 3 rows\n{0}/empty_table.csv: 0 rows\n{0}/holdings.csv: 3 rows\n\
             {0}/meta.csv: 10 rows\n{0}/warnings.csv: 2 rows\n",
            dir.display()
        )
    );
    // Tables only: the view would repeat rows of accounts. And nothing
    // temporary is left.
    assert_eq!(
        file_names(&dir),
        [
            "accounts.csv",
            "empty_table.csv",
            "holdings.csv",
            "meta.csv",
            "warnings.csv"
        ]
    );

    let read = |name: &str| fs::read_to_string(dir.join(name)).unwrap();
    assert_eq!(read("accounts.csv"), ACCOUNTS_CSV);
    assert_eq!(read("empty_table.csv"), "id,label\n");
    assert_eq!(
        read("holdings.csv"),
        "id,account_id,quantity,note\n5,1,0.0000000001,\n7,2,1.5,\"\"\n\
         7,2,123456789012345.1234567891,\"two\nlines\"\n"
    );
    assert_eq!(
        read("warnings.csv"),
        "message\nrounded holdings.quantity of record 7\nskipped unknown entry Foo\n"
    );
    assert!(read("meta.csv").starts_with("key,value\nb4_version,0.1.0\n"));

    #[cfg(unix)]
    {
        for name in file_names(&dir) {
            let mode = fs::metadata(dir.join(&name)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
        let mode = fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "the directory");
    }
}

#[test]
fn export_writes_jsonl() {
    let fixture = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let output = export(&fixture, dir.path(), &["--format", "jsonl", "--quiet"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        (stdout(&output), stderr(&output)),
        (String::new(), String::new())
    );
    assert_eq!(
        file_names(dir.path()),
        [
            "accounts.jsonl",
            "empty_table.jsonl",
            "holdings.jsonl",
            "meta.jsonl",
            "warnings.jsonl"
        ]
    );
    let read = |name: &str| fs::read_to_string(dir.path().join(name)).unwrap();
    assert_eq!(read("holdings.jsonl"), HOLDINGS_JSONL);
    assert_eq!(read("empty_table.jsonl"), "");
    let accounts = read("accounts.jsonl");
    assert_eq!(accounts.lines().count(), 3);
    assert_eq!(
        json(accounts.lines().next().unwrap()),
        serde_json::json!({
            "id": 1, "name": "Giro", "currency": "EUR", "balance": -12.34, "hidden": false,
            "opened": "2019-01-15", "updated": "2025-06-30T12:00:00", "rate": 0.1
        })
    );
    assert_eq!(
        accounts.lines().nth(2).unwrap(),
        "{\"id\":3,\"name\":null,\"currency\":null,\"balance\":null,\"hidden\":null,\
         \"opened\":null,\"updated\":null,\"rate\":null}"
    );
}

/// `--output` is a global option, so `b4 export` accepts it. It selects the
/// format where it can and is refused where it cannot: never ignored.
#[test]
fn export_honours_the_global_output_option_or_refuses_it() {
    let fixture = Fixture::new();
    let parent = tempfile::tempdir().unwrap();
    let mut count = 0;
    let mut fresh = || {
        count += 1;
        parent.path().join(format!("out{count}"))
    };

    // csv and jsonl select the format, wherever the option stands.
    for args in [
        &["--output", "jsonl"][..],
        &["-o", "jsonl"],
        &["--output=jsonl"],
        &["--format", "jsonl", "--output", "jsonl"],
    ] {
        let dir = fresh();
        let output = export(&fixture, &dir, &[args, &["--table", "holdings"]].concat());
        assert_eq!(exit_code(&output), 0, "{args:?}:\n{}", stderr(&output));
        assert_eq!(file_names(&dir), ["holdings.jsonl"], "{args:?}");
        assert_eq!(
            fs::read_to_string(dir.join("holdings.jsonl")).unwrap(),
            HOLDINGS_JSONL
        );
    }
    let dir = fresh();
    let output = fixture.run(&[
        "--output",
        "jsonl",
        "export",
        dir.to_str().unwrap(),
        "--table",
        "holdings",
    ]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(file_names(&dir), ["holdings.jsonl"]);
    for args in [
        &["--output", "csv"][..],
        &["-o", "csv", "--format", "csv"],
        &[],
    ] {
        let dir = fresh();
        let output = export(&fixture, &dir, &[args, &["--table", "accounts"]].concat());
        assert_eq!(exit_code(&output), 0, "{args:?}:\n{}", stderr(&output));
        assert_eq!(file_names(&dir), ["accounts.csv"], "{args:?}");
        assert_eq!(
            fs::read_to_string(dir.join("accounts.csv")).unwrap(),
            ACCOUNTS_CSV
        );
    }

    // What export cannot write is a usage error that names --format, before
    // anything is created.
    for (args, name) in [
        (&["--output", "json"][..], "json"),
        (&["-o", "json", "--format", "csv"], "json"),
        (&["--output", "table"], "table"),
        (&["-otable"], "table"),
        (&["--output=table", "--format", "jsonl"], "table"),
    ] {
        let dir = fresh();
        let output = export(&fixture, &dir, args);
        assert_eq!(exit_code(&output), 2, "{args:?}");
        assert_eq!(stdout(&output), "");
        assert_eq!(
            stderr(&output),
            format!(
                "error: b4 export cannot write `--output {name}`: choose the file format \
                 with `--format csv` or `--format jsonl`\n"
            ),
            "{args:?}"
        );
        assert!(!dir.exists(), "{args:?}");
    }
    let dir = fresh();
    let output = fixture.run(&["-o", "table", "export", dir.to_str().unwrap()]);
    assert_eq!(exit_code(&output), 2);
    assert!(!dir.exists());

    // And so is an --output that contradicts --format.
    let dir = fresh();
    let output = export(&fixture, &dir, &["--format", "csv", "--output", "jsonl"]);
    assert_eq!(exit_code(&output), 2);
    assert_eq!(
        stderr(&output),
        "error: `--format csv` and `--output jsonl` contradict each other: choose the file \
         format with `--format` alone\n"
    );
    assert!(!dir.exists());
}

/// A table name becomes a file name. One that could leave the directory on
/// some system, or that is a device there, is refused before anything is
/// written.
#[test]
fn export_refuses_table_names_that_are_not_file_names() {
    for name in [
        "C:notes",
        "NUL",
        "com1",
        "a/b",
        "..",
        "trailing.",
        "trailing ",
    ] {
        let fixture = Fixture::new();
        Connection::open(fixture.db())
            .unwrap()
            .execute_batch(&format!("create table \"{name}\" as select 1 as id"))
            .unwrap();
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("out");

        for args in [&[][..], &["--table", name]] {
            let output = export(&fixture, &dir, args);
            assert_eq!(exit_code(&output), 1, "{name:?} {args:?}");
            assert_eq!(
                stderr(&output),
                format!(
                    "error: cannot export the table `{name}`: its name cannot be used as a \
                     file name\n"
                ),
                "{name:?} {args:?}"
            );
            assert!(!dir.exists(), "{name:?} {args:?}");
        }
        // The other tables can still be exported by name.
        let output = export(&fixture, &dir, &["--table", "accounts"]);
        assert_eq!(exit_code(&output), 0, "{name:?}:\n{}", stderr(&output));
        assert_eq!(file_names(&dir), ["accounts.csv"], "{name:?}");
        assert_eq!(file_names(parent.path()), ["out"], "{name:?}");
    }
}

#[test]
fn export_of_selected_tables() {
    let fixture = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let output = export(
        &fixture,
        dir.path(),
        &[
            "--table", "holdings", "--table", "accounts", "--table", "HOLDINGS",
        ],
    );
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    // In the order asked for, each once; meta and warnings only on request.
    assert_eq!(
        stderr(&output),
        format!(
            "{0}/holdings.csv: 3 rows\n{0}/accounts.csv: 3 rows\n",
            dir.path().display()
        )
    );
    assert_eq!(file_names(dir.path()), ["accounts.csv", "holdings.csv"]);

    // A view can be named.
    let output = export(
        &fixture,
        dir.path(),
        &["--table", "accounts_view", "--no-header"],
    );
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        fs::read_to_string(dir.path().join("accounts_view.csv")).unwrap(),
        "1,Giro\n2,\"Savings, \"\"high\"\" yield\"\n"
    );
}

#[test]
fn export_of_an_unknown_table_writes_nothing() {
    let fixture = Fixture::new();
    let parent = tempfile::tempdir().unwrap();
    let dir = parent.path().join("export");
    let output = export(
        &fixture,
        &dir,
        &["--table", "accounts", "--table", "bookings"],
    );
    assert_eq!(exit_code(&output), 1);
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "error: no table or view named `bookings`: there are accounts, accounts_view, \
         empty_table, holdings, meta, warnings\n"
    );
    assert!(!dir.exists(), "the directory was created");
    assert_eq!(file_names(parent.path()), Vec::<String>::new());
}

#[test]
fn two_exports_are_the_same_bytes() {
    let fixture = Fixture::new();
    // Rows that only differ in a late column, stored in an order that is
    // not the sorted one.
    Connection::open(fixture.db())
        .unwrap()
        .execute_batch(
            "create table shuffled as
               select (i * 7919) % 13 as id, (i * 31) % 5 as account_id, i % 3 as part,
                      'row ' || ((i * 104729) % 5003) as label
               from range(5000) t(i);
             create table no_key as
               select ((i * 7919) % 5003)::varchar as label, i % 2 = 0 as even
               from range(3000) t(i);",
        )
        .unwrap();

    for format in ["csv", "jsonl"] {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for dir in [&first, &second] {
            let output = export(&fixture, dir.path(), &["--format", format, "-q"]);
            assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
        }
        let names = file_names(first.path());
        assert_eq!(names.len(), 7);
        assert_eq!(names, file_names(second.path()));
        for name in names {
            assert_eq!(
                fs::read(first.path().join(&name)).unwrap(),
                fs::read(second.path().join(&name)).unwrap(),
                "{name}"
            );
        }
    }

    // More rows than one chunk, in order, with one header.
    let dir = tempfile::tempdir().unwrap();
    export(
        &fixture,
        dir.path(),
        &["--table", "shuffled", "--table", "no_key"],
    );
    let shuffled = fs::read_to_string(dir.path().join("shuffled.csv")).unwrap();
    let lines: Vec<&str> = shuffled.lines().collect();
    assert_eq!(lines.len(), 5001);
    assert_eq!(lines[0], "id,account_id,part,label");
    assert_eq!(lines.iter().filter(|line| **line == lines[0]).count(), 1);
    let keys: Vec<(u32, u32)> = lines[1..]
        .iter()
        .map(|line| {
            let mut fields = line.split(',');
            (
                fields.next().unwrap().parse().unwrap(),
                fields.next().unwrap().parse().unwrap(),
            )
        })
        .collect();
    assert!(
        keys.windows(2).all(|pair| pair[0] <= pair[1]),
        "not ordered by id, account_id"
    );

    // Without key columns: by every column.
    let no_key = fs::read_to_string(dir.path().join("no_key.csv")).unwrap();
    let labels: Vec<&str> = no_key.lines().skip(1).collect();
    assert_eq!(labels.len(), 3000);
    assert!(
        labels.windows(2).all(|pair| pair[0] <= pair[1]),
        "not ordered"
    );
}

#[test]
fn export_replaces_existing_files() {
    let fixture = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let accounts = dir.path().join("accounts.csv");
    fs::write(
        &accounts,
        "an old export that is much longer than the new one\n".repeat(50),
    )
    .unwrap();
    fs::write(dir.path().join("notes.txt"), "mine").unwrap();
    #[cfg(unix)]
    fs::set_permissions(&accounts, fs::Permissions::from_mode(0o644)).unwrap();

    let output = export(&fixture, dir.path(), &["--table", "accounts"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(fs::read_to_string(&accounts).unwrap(), ACCOUNTS_CSV);
    // The new file replaced the old one; it did not inherit its mode.
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&accounts).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Other files are left alone, and no temporary file stays behind.
    assert_eq!(file_names(dir.path()), ["accounts.csv", "notes.txt"]);
    assert_eq!(
        fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "mine"
    );
}

#[test]
fn export_fails_cleanly_when_the_directory_cannot_be_used() {
    let fixture = Fixture::new();
    let parent = tempfile::tempdir().unwrap();
    let file = parent.path().join("file");
    fs::write(&file, "in the way").unwrap();

    let output = export(&fixture, &file, &[]);
    assert_eq!(exit_code(&output), 1);
    assert!(
        stderr(&output).starts_with(&format!(
            "error: cannot create the directory {}: ",
            file.display()
        )),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "in the way");
    assert_eq!(file_names(parent.path()), ["file"]);
}
