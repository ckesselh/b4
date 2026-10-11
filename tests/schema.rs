//! The database schema: its definition, and the database `b4 decrypt` writes.
//!
//! The definition is `src/db/schema.rs`, compiled into this test as a module
//! of its own, because the library keeps its database layer private. The
//! tests check the definition, the catalog of a database created from it, the
//! catalog of a database written by the real binary, and the Markdown
//! rendering for the README.
//!
//! To write the rendering to a file:
//!
//! ```console
//! $ B4_SCHEMA_MARKDOWN=/tmp/schema.md cargo test --test schema markdown
//! ```

// Everything in this file is test code; the allowance for `unwrap` in tests
// only reaches the test functions themselves, not their helpers.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Write as _;
use std::process::{Command, Stdio};

use duckdb::Connection;

/// The two names `schema.rs` takes from its parent module (`src/db/mod.rs`).
const META_TABLE: &str = "meta";
const WARNINGS_TABLE: &str = "warnings";

#[path = "../src/db/schema.rs"]
mod schema;

use schema::{create, find_table, find_view, table, tables, view, views};

const DEMO_VAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault/demo.sub");

/// The bank access fields of the format: none may be a column.
const ACCESS_FIELDS: [&str; 10] = [
    "fints_customer_id",
    "last_download_token",
    "contact_guid",
    "order_contact_guid",
    "contact_name",
    "order_contact_name",
    "fints_account_sub_id",
    "used_tan",
    "order_reference_tan",
    "acknowledge_token",
];

fn is_snake_case(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && !name.ends_with('_')
        && !name.contains("__")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Name, type and comment of every column of `relation` as the database
/// reports them.
fn catalog_columns(connection: &Connection, relation: &str) -> Vec<(String, String, String)> {
    let mut statement = connection
        .prepare(
            "select column_name, data_type, coalesce(comment, '') from duckdb_columns() \
             where table_name = ? order by column_index",
        )
        .unwrap();
    let rows = statement
        .query_map([relation], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
}

#[test]
fn the_table_list_is_the_agreed_one() {
    let names: Vec<&str> = tables().iter().map(|table| table.name).collect();
    assert_eq!(
        names,
        [
            "accounts",
            "account_groups",
            "bookings",
            "payments",
            "standing_orders",
            "payees",
            "payers",
            "categories",
            "mandates",
            "exchange_rates",
            "stock_instruments",
            "stock_prices",
            "portfolio_items",
            "portfolio_transactions",
            "tax_exemptions",
            "assets",
            "meta",
            "warnings",
        ]
    );
    let views: Vec<&str> = views().iter().map(|view| view.name).collect();
    assert_eq!(
        views,
        ["bookings_view", "account_balances", "monthly_category_sums"]
    );
}

/// The writer fits a value to `MONEY` and `DECIMAL`, the column is declared
/// with the type name of its kind: the two must say the same.
#[test]
fn the_decimal_type_names_match_the_digits_the_writer_fits_values_to() {
    let name = |(width, scale): (u8, u8)| format!("DECIMAL({width},{scale})");
    assert_eq!(schema::Kind::Money.sql_type(), name(schema::MONEY));
    assert_eq!(schema::Kind::Decimal.sql_type(), name(schema::DECIMAL));
}

#[test]
fn every_table_view_and_column_is_described_and_well_named() {
    let mut relations = BTreeSet::new();
    let columns_of = |name: &'static str, description: &str, columns: Vec<(&str, &str)>| {
        assert!(is_snake_case(name), "{name}");
        assert!(description.len() > 20, "{name} needs a description");
        assert!(!columns.is_empty(), "{name}");
        let mut seen = BTreeSet::new();
        for (column, description) in columns {
            assert!(is_snake_case(column), "{name}.{column}");
            assert!(seen.insert(column.to_owned()), "{name}.{column} repeats");
            assert!(
                description.len() > 10 && description.ends_with('.'),
                "{name}.{column} needs a description that is a sentence"
            );
            assert!(
                !ACCESS_FIELDS.contains(&column),
                "{name}.{column} is a bank access field"
            );
        }
    };
    for table in tables() {
        assert!(relations.insert(table.name), "{} repeats", table.name);
        let columns = table
            .columns
            .iter()
            .map(|column| (column.name, column.description))
            .collect();
        columns_of(table.name, table.description, columns);
    }
    for view in views() {
        assert!(relations.insert(view.name), "{} repeats", view.name);
        let columns = view
            .columns
            .iter()
            .map(|column| (column.name, column.description))
            .collect();
        columns_of(view.name, view.description, columns);
    }
}

#[test]
fn no_column_name_resembles_bank_access_data() {
    // Beyond the exact field names: nothing that smells like them.
    for table in tables() {
        for column in &table.columns {
            for word in ["tan", "token", "guid", "fints", "password", "pin", "secret"] {
                assert!(
                    !column.name.split('_').any(|part| part == word),
                    "{}.{}",
                    table.name,
                    column.name
                );
            }
        }
    }
}

#[test]
fn per_account_tables_start_with_account_id_then_id() {
    for name in [
        table::BOOKINGS,
        table::PAYMENTS,
        table::STANDING_ORDERS,
        table::PORTFOLIO_ITEMS,
        table::PORTFOLIO_TRANSACTIONS,
        table::TAX_EXEMPTIONS,
        table::ASSETS,
    ] {
        let table = find_table(name).unwrap();
        let head: Vec<&str> = table.column_names().take(4).collect();
        assert_eq!(
            head,
            ["account_id", "id", "created_at", "updated_at"],
            "{name}"
        );
    }
    for table in tables() {
        if table.name == META_TABLE || table.name == WARNINGS_TABLE {
            continue;
        }
        assert!(
            table.column_names().any(|column| column == "id"),
            "{}",
            table.name
        );
    }
}

#[test]
fn the_payment_family_shares_its_column_names() {
    let names = |name| -> BTreeSet<&str> { find_table(name).unwrap().column_names().collect() };
    let bookings = names(table::BOOKINGS);
    let payments = names(table::PAYMENTS);
    let standing_orders = names(table::STANDING_ORDERS);
    for column in [
        "account_id",
        "id",
        "amount",
        "debit",
        "stored_amount",
        "currency",
        "stored_currency",
        "payee_name",
        "payee_iban",
        "payee_bic",
        "purpose",
        "category",
        "category_key",
        "end_to_end_id",
        "mandate_id",
        "creditor_id",
    ] {
        assert!(bookings.contains(column), "bookings.{column}");
        assert!(payments.contains(column), "payments.{column}");
        assert!(standing_orders.contains(column), "standing_orders.{column}");
    }
}

#[test]
fn the_database_has_exactly_the_defined_tables_columns_and_comments() {
    let connection = Connection::open_in_memory().unwrap();
    create(&connection).unwrap();

    for table in tables() {
        let expected: Vec<(String, String, String)> = table
            .columns
            .iter()
            .map(|column| {
                (
                    column.name.to_owned(),
                    column.kind.sql_type().to_owned(),
                    column.description.to_owned(),
                )
            })
            .collect();
        assert_eq!(
            catalog_columns(&connection, table.name),
            expected,
            "{}",
            table.name
        );
        let comment: String = connection
            .query_row(
                "select comment from duckdb_tables() where table_name = ?",
                [table.name],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(comment, table.description, "{}", table.name);
    }
    for view in views() {
        let expected: Vec<(String, String, String)> = view
            .columns
            .iter()
            .map(|column| {
                (
                    column.name.to_owned(),
                    column.sql_type.to_owned(),
                    column.description.to_owned(),
                )
            })
            .collect();
        assert_eq!(
            catalog_columns(&connection, view.name),
            expected,
            "{}",
            view.name
        );
        let comment: String = connection
            .query_row(
                "select comment from duckdb_views() where view_name = ?",
                [view.name],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(comment, view.description, "{}", view.name);
    }

    let relations: i64 = connection
        .query_row(
            "select (select count(*) from duckdb_tables() where not internal) \
                  + (select count(*) from duckdb_views() where not internal)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(relations as usize, tables().len() + views().len());

    // S1: no key and no other constraint anywhere.
    let constraints: i64 = connection
        .query_row("select count(*) from duckdb_constraints()", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(constraints, 0);
    let indexes: i64 = connection
        .query_row("select count(*) from duckdb_indexes()", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(indexes, 0);
}

#[test]
fn the_bookings_view_has_every_bookings_column_first() {
    let bookings: Vec<&str> = find_table(table::BOOKINGS)
        .unwrap()
        .column_names()
        .collect();
    let view: Vec<&str> = find_view(view::BOOKINGS)
        .unwrap()
        .columns
        .iter()
        .map(|column| column.name)
        .collect();
    assert_eq!(&view[..bookings.len()], &bookings[..]);
}

#[test]
fn the_category_levels_of_the_view_are_split_at_colons() {
    let connection = Connection::open_in_memory().unwrap();
    create(&connection).unwrap();
    connection
        .execute_batch(
            "insert into bookings (account_id, id, category) values \
               (1, 1, 'Wohnen:Nebenkosten:Strom'), (1, 2, 'Wohnen:Miete'), \
               (1, 3, 'Auto'), (1, 4, NULL)",
        )
        .unwrap();
    let mut statement = connection
        .prepare(
            "select id, category_name, category_parent, category_top from bookings_view \
             order by id",
        )
        .unwrap();
    type Row = (i64, Option<String>, Option<String>, Option<String>);
    let rows: Vec<Row> = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let text = |s: &str| Some(s.to_owned());
    assert_eq!(
        rows,
        [
            (1, text("Strom"), text("Wohnen:Nebenkosten"), text("Wohnen")),
            (2, text("Miete"), text("Wohnen"), text("Wohnen")),
            (3, text("Auto"), None, text("Auto")),
            (4, None, None, None),
        ]
    );
}

#[test]
fn repeated_account_ids_and_category_keys_do_not_repeat_bookings() {
    let connection = Connection::open_in_memory().unwrap();
    create(&connection).unwrap();
    connection
        .execute_batch(
            "insert into accounts (id, name) values (1, 'first'), (1, 'second');
             insert into categories (id, category_key, special) values
               (1, 'AUTO', 'none'), (2, 'AUTO', 'excluded'), (3, NULL, 'none');
             insert into bookings (account_id, id, category, category_key, amount, counted, pending)
               values (1, 1, 'Auto', 'AUTO', -5, true, false), (1, 2, NULL, NULL, 7, true, false)",
        )
        .unwrap();
    let (count, names, special): (i64, String, String) = connection
        .query_row(
            "select count(*), string_agg(distinct account_name, ','), \
                    string_agg(category_special, ',') from bookings_view",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (count, names.as_str(), special.as_str()),
        (2, "first", "none")
    );
}

/// The schema as Markdown, for `docs/schema.md`: one section per table and
/// view with a table of its columns. The output depends on nothing but the
/// schema definition.
fn markdown() -> String {
    let mut text = String::new();
    let mut section =
        |name: &str, suffix: &str, description: &str, rows: Vec<(&str, &str, &str)>| {
            let _ = writeln!(text, "### `{name}`{suffix}\n\n{description}\n");
            let _ = writeln!(text, "| Column | Type | Description |\n|---|---|---|");
            for (column, sql_type, description) in rows {
                let description = description.replace('|', "\\|");
                let _ = writeln!(text, "| `{column}` | `{sql_type}` | {description} |");
            }
            text.push('\n');
        };
    for table in tables() {
        let rows = table
            .columns
            .iter()
            .map(|column| (column.name, column.kind.sql_type(), column.description))
            .collect();
        section(table.name, "", table.description, rows);
    }
    for view in views() {
        let rows = view
            .columns
            .iter()
            .map(|column| (column.name, column.sql_type, column.description))
            .collect();
        section(view.name, " (view)", view.description, rows);
    }
    // One newline at the end, not two.
    text.truncate(text.trim_end().len());
    text.push('\n');
    text
}

#[test]
fn the_markdown_rendering_is_stable() {
    let text = markdown();
    assert_eq!(text, markdown());
    assert!(text.starts_with("### `accounts`\n\n"));
    assert!(text.ends_with(" |\n"));
    assert!(!text.contains("\n\n\n"));
    assert!(text.contains("\n### `bookings_view` (view)\n"));
    // One row per column, one section per table and view.
    let rows = text.lines().filter(|line| line.starts_with("| `")).count();
    let columns: usize = tables()
        .iter()
        .map(|table| table.columns.len())
        .sum::<usize>()
        + views().iter().map(|view| view.columns.len()).sum::<usize>();
    assert_eq!(rows, columns);
    assert_eq!(
        text.lines().filter(|line| line.starts_with("### ")).count(),
        tables().len() + views().len()
    );
    // A line of the bookings table, as a reader of the README sees it.
    assert!(text.contains(
        "| `counted` | `BOOLEAN` | False for the master row of a split or batch that has \
         parts: its parts carry the money. Sum `amount` over rows where counted is true, \
         or money is counted twice. |"
    ));

    // B4_SCHEMA_MARKDOWN=<file> writes the rendering there.
    if let Some(path) = std::env::var_os("B4_SCHEMA_MARKDOWN") {
        std::fs::write(path, &text).unwrap();
    }
}

/// Everything in `docs/schema.md` before the generated part.
const SCHEMA_DOC_INTRO: &str = "\
# The database

This is the layout of the DuckDB database that `b4 decrypt` writes: every
table, view and column with its type and description. The same texts are
stored in the database and shown by `b4 schema`.

This file is generated from `src/db/schema.rs`; do not edit it. Regenerate it
with `task docs`.

Conventions, the same in every table:

- `id` is the record's object id as stored in the vault. Rows of per-account
  tables carry `account_id` as well. Nothing is declared unique: the format
  guarantees no uniqueness, so no table has a key or a constraint.
- Money is `DECIMAL(18,4)`; quantities, prices and rates are `DECIMAL(28,10)`.
  Sums are exact.
- Amounts are signed: negative is money leaving the account.
- A coded value is its lower-case name in a `VARCHAR`; a code this version does
  not know is its number as text. A flag set is the names of the set flags
  joined by commas.
- An empty text is NULL.
- Timestamps have no time zone unless a description says UTC.

## Tables and views

";

/// `docs/schema.md` is the committed rendering of the schema. It must match
/// the definition, like the man pages match the command line.
/// `B4_UPDATE_DOCS=1 cargo test --test schema schema_doc` rewrites it.
#[test]
fn the_schema_doc_is_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/schema.md");
    let expected = format!("{SCHEMA_DOC_INTRO}{}", markdown());
    if std::env::var_os("B4_UPDATE_DOCS").is_some() {
        std::fs::write(&path, &expected).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == expected,
        "docs/schema.md is out of date: run `task docs`"
    );
}

/// Decrypt the demo vault with the real binary and return the directory and
/// the database.
fn decrypted_demo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("b4.duckdb");
    let mut child = Command::new(env!("CARGO_BIN_EXE_b4"))
        .args(["decrypt", DEMO_VAULT, database.to_str().unwrap()])
        .args(["--password-stdin", "--quiet"])
        .env_remove("B4_DB")
        .env_remove("B4_PASSWORD")
        .env_remove("B4_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("cannot run b4");
    child.stdin.take().unwrap().write_all(b"demo\n").unwrap();
    assert!(child.wait().unwrap().success(), "b4 decrypt failed");
    (dir, database)
}

#[test]
fn a_decrypted_database_has_exactly_the_defined_schema() {
    let (_dir, database) = decrypted_demo();
    let connection = Connection::open(&database).unwrap();

    let mut statement = connection
        .prepare(
            "select table_name, comment from duckdb_tables() where not internal \
             order by table_name",
        )
        .unwrap();
    let found: Vec<(String, Option<String>)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    // A reopened database does not list tables in the order of creation.
    let mut defined: Vec<(String, Option<String>)> = tables()
        .iter()
        .map(|table| (table.name.to_owned(), Some(table.description.to_owned())))
        .collect();
    defined.sort();
    assert_eq!(found, defined);

    let mut statement = connection
        .prepare(
            "select view_name, comment from duckdb_views() where not internal order by view_name",
        )
        .unwrap();
    let found: Vec<(String, Option<String>)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut defined: Vec<(String, Option<String>)> = views()
        .iter()
        .map(|view| (view.name.to_owned(), Some(view.description.to_owned())))
        .collect();
    defined.sort();
    assert_eq!(found, defined);

    for table in tables() {
        let defined: Vec<(String, String, String)> = table
            .columns
            .iter()
            .map(|column| {
                (
                    column.name.to_owned(),
                    column.kind.sql_type().to_owned(),
                    column.description.to_owned(),
                )
            })
            .collect();
        assert_eq!(
            catalog_columns(&connection, table.name),
            defined,
            "{}",
            table.name
        );
    }
    for view in views() {
        let defined: Vec<(String, String, String)> = view
            .columns
            .iter()
            .map(|column| {
                (
                    column.name.to_owned(),
                    column.sql_type.to_owned(),
                    column.description.to_owned(),
                )
            })
            .collect();
        assert_eq!(
            catalog_columns(&connection, view.name),
            defined,
            "{}",
            view.name
        );
        // Every view can be read.
        let rows: i64 = connection
            .query_row(&format!("select count(*) from {}", view.name), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(rows > 0, "{}", view.name);
    }

    // S1: nothing is unique, so nothing is constrained.
    let constraints: i64 = connection
        .query_row(
            "select (select count(*) from duckdb_constraints()) \
                  + (select count(*) from duckdb_indexes())",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(constraints, 0);
}

#[test]
fn no_column_of_a_decrypted_database_is_a_bank_access_field() {
    let (_dir, database) = decrypted_demo();
    let connection = Connection::open(&database).unwrap();
    let mut statement = connection
        .prepare("select table_name, column_name from duckdb_columns() where not internal")
        .unwrap();
    let columns: Vec<(String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(columns.len() > 400, "{} columns", columns.len());
    for (table, column) in &columns {
        assert!(
            !ACCESS_FIELDS.contains(&column.as_str()),
            "{table}.{column} is a bank access field"
        );
        assert!(is_snake_case(column), "{table}.{column}");
    }
    // The two tables the rule names have an explicit column list.
    for name in [table::ACCOUNTS, table::PAYMENTS] {
        let defined: Vec<&str> = find_table(name).unwrap().column_names().collect();
        let found: Vec<&str> = columns
            .iter()
            .filter(|(table, _)| table == name)
            .map(|(_, column)| column.as_str())
            .collect();
        assert_eq!(found, defined, "{name}");
    }
}
