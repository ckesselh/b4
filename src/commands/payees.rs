//! `b4 payees list`: the payees saved in the vault.

use crate::cli::{Format, Globals, PayeesCommand, PayeesListArgs};
use crate::db::read::{quote_identifier, Database, Hint, Param};
use crate::db::schema;
use crate::error::Context;
use crate::output::{self, Table};
use crate::Error;

/// The text columns of `payees` that `--filter` searches. Coded values
/// (`kind`, `flags`, `vop_status`) are left out: nobody searches for them by
/// typing a part of a name.
const SEARCHED: &[&str] = &[
    "name",
    "iban",
    "bic",
    "account_number",
    "bank_code",
    "street",
    "town",
    "country",
    "bank_name",
    "bank_street",
    "bank_town",
    "purpose",
    "category",
    "email",
    "customer_number",
    "notes",
];

/// The columns a person sees in table output, in order.
const SHOWN: &[&str] = &["name", "iban", "bic", "bank_name", "town"];

/// Key of the column with the number of bookings. Not a column of `payees`.
const BOOKINGS: &str = "booking_count";

/// Run a verb of `b4 payees`.
pub fn run(command: &PayeesCommand, globals: &Globals) -> Result<(), Error> {
    match command {
        PayeesCommand::List(args) => list(args, globals),
    }
}

/// `b4 payees list [--filter <TEXT>]`: print the address book.
fn list(args: &PayeesListArgs, globals: &Globals) -> Result<(), Error> {
    let database = Database::open(&globals.db)?;
    let table = table(&database, args.filter.as_deref(), globals.format())?;
    output::print_table(&table, globals)
}

/// One row per saved payee, ordered by name.
///
/// * Order: by the lower-cased name in code point order (so `Zoo` sorts
///   before `Ärztin`, whatever the locale), then by the name as written, then
///   by id. Payees without a name come last.
/// * Filter: with `filter`, only payees that have the text in one of the
///   [`SEARCHED`] columns, as a substring and without regard to case. An IBAN
///   also matches when the text has blanks the stored IBAN has not. The text
///   is bound as a parameter and compared with `contains`, not `LIKE`, so `%`
///   and `_` stand for themselves.
/// * `booking_count`: the number of counted bookings (`bookings.counted`) whose
///   `payee_name` is the payee's name. Bookings do not refer to payees by id,
///   so the name is all there is to match on; both names are trimmed and
///   upper-cased first. Two payees of the same name get the same number.
/// * Columns: [`SHOWN`] and `booking_count` for [`Format::Table`]; every column of
///   the `payees` table under its own name, then `booking_count`, for the
///   other
///   formats.
fn table(database: &Database, filter: Option<&str>, format: Format) -> Result<Table, Error> {
    let columns: Vec<&str> = if format == Format::Table {
        SHOWN.to_vec()
    } else {
        schema::find_table(schema::table::PAYEES)
            .map(|table| table.column_names().collect())
            .unwrap_or_default()
    };
    let select: Vec<String> = columns
        .iter()
        .map(|name| format!("p.{}", quote_identifier(name)))
        .collect();

    let mut params = Vec::new();
    let mut conditions = Vec::new();
    if let Some(text) = filter {
        params.push(Param::from(text));
        conditions.extend(
            SEARCHED
                .iter()
                .map(|name| format!("contains(lower(p.{}), lower($1))", quote_identifier(name))),
        );
        // `DE31 9999 0001` finds the IBAN stored as `DE3199990001...`.
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if !compact.is_empty() && compact != text {
            params.push(Param::from(compact));
            conditions.push("contains(lower(p.iban), lower($2))".to_owned());
        }
    }
    let filter_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("where {}", conditions.join(" or "))
    };

    let sql = format!(
        "with counts as (
           select upper(trim(payee_name)) as name_key, count(*) as bookings
           from bookings
           where counted and payee_name is not null
           group by all)
         select {select}, coalesce(c.bookings, 0) as {bookings}
         from payees p
         left join counts c on c.name_key = upper(trim(p.name))
         {filter_clause}
         order by lower(p.name) nulls last, p.name, p.id",
        select = select.join(", "),
        bookings = quote_identifier(BOOKINGS),
    );
    let hints = [
        Hint::new("name").title("Name"),
        Hint::new("iban").title("IBAN"),
        Hint::new("bic").title("BIC"),
        Hint::new("bank_name").title("Bank").flexible(),
        Hint::new("town").title("Town").flexible(),
        Hint::new("amount").money("currency"),
        Hint::new(BOOKINGS).title("Bookings"),
    ];
    database
        .query_with(&sql, &params, &hints)
        .context("cannot list the payees")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SCHEMA_VERSION;
    use crate::output::{Cell, Options};

    /// A database of the real schema with a handful of payees and bookings.
    fn sample() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b4.duckdb");
        let connection = duckdb::Connection::open(&path).unwrap();
        schema::create(&connection).unwrap();
        connection
            .execute_batch(&format!(
                "insert into meta values ('schema_version', '{SCHEMA_VERSION}');
                 insert into payees (id, name, iban, bic, bank_name, town, notes, amount, currency) values
                   (1, 'Stadtwerke Musterstadt', 'DE31999900011000471100', 'MUBADEM0XXX', 'Musterbank', '12345 Musterstadt', null, 0, 'EUR'),
                   (2, 'Ärztin Dr. Öztürk', null, null, null, null, 'Praxis am Markt', 80.5, 'EUR'),
                   (3, '100% Bio GmbH', 'DE02999900022000471122', null, null, null, null, 0, null),
                   (4, 'snake_case AG', null, null, null, 'Zürich', null, 0, null),
                   (5, 'zoo Berlin', null, null, null, null, null, 0, null),
                   (6, 'Nobody Pays', null, null, null, null, null, 0, null),
                   (7, null, 'DE99999900033000471144', null, null, null, null, 0, null);
                 insert into bookings (account_id, id, payee_name, amount, currency, counted, split_role) values
                   (1, 1, 'Stadtwerke Musterstadt', -50, 'EUR', true, null),
                   (1, 2, '  STADTWERKE musterstadt ', -51, 'EUR', true, null),
                   (2, 3, 'Stadtwerke Musterstadt', -52, 'EUR', true, null),
                   (1, 4, 'Stadtwerke Musterstadt Netz', -1, 'EUR', true, null),
                   (1, 10, 'Ärztin Dr. Öztürk', -90, 'EUR', false, 'split_master'),
                   (1, 11, 'Ärztin Dr. Öztürk', -40, 'EUR', true, 'split_slave'),
                   (1, 12, 'Ärztin Dr. Öztürk', -50, 'EUR', true, 'split_slave'),
                   (1, 20, '100% Bio GmbH', -9, 'USD', true, null),
                   (1, 21, '100 Bio GmbH', -9, 'USD', true, null),
                   (1, 22, null, -9, 'EUR', true, null);"
            ))
            .unwrap();
        drop(connection);
        let database = Database::open(&path).unwrap();
        (dir, database)
    }

    fn index(table: &Table, name: &str) -> usize {
        table
            .columns()
            .iter()
            .position(|column| column.name() == name)
            .unwrap_or_else(|| panic!("no column {name}"))
    }

    fn texts(table: &Table, name: &str) -> Vec<Option<String>> {
        let index = index(table, name);
        table
            .rows()
            .iter()
            .map(|row| match &row[index] {
                Cell::Text(text) => Some(text.clone()),
                Cell::Null => None,
                other => panic!("{name} is {other:?}"),
            })
            .collect()
    }

    fn names(database: &Database, filter: &str) -> Vec<String> {
        let table = table(database, Some(filter), Format::Json).unwrap();
        texts(&table, "name").into_iter().flatten().collect()
    }

    fn counts(table: &Table) -> Vec<i64> {
        let index = index(table, BOOKINGS);
        table
            .rows()
            .iter()
            .map(|row| match &row[index] {
                Cell::Integer(count) => *count,
                other => panic!("bookings is {other:?}"),
            })
            .collect()
    }

    #[test]
    fn payees_are_ordered_by_name_without_regard_to_case_and_nameless_last() {
        let (_dir, database) = sample();
        let table = table(&database, None, Format::Table).unwrap();
        assert_eq!(
            texts(&table, "name"),
            [
                Some("100% Bio GmbH".to_owned()),
                Some("Nobody Pays".to_owned()),
                Some("snake_case AG".to_owned()),
                Some("Stadtwerke Musterstadt".to_owned()),
                Some("zoo Berlin".to_owned()),
                Some("Ärztin Dr. Öztürk".to_owned()),
                None,
            ]
        );
    }

    #[test]
    fn bookings_are_counted_by_name_and_only_when_counted() {
        let (_dir, database) = sample();
        let table = table(&database, None, Format::Table).unwrap();
        // 100% Bio: one, not the booking of `100 Bio GmbH`. Stadtwerke: three
        // over two accounts and two spellings, not `... Netz`. The doctor:
        // the two parts of the split, not its master.
        assert_eq!(counts(&table), [1, 0, 0, 3, 0, 2, 0]);
    }

    #[test]
    fn the_table_shows_the_columns_for_people() {
        let (_dir, database) = sample();
        let table = table(&database, Some("stadtwerke"), Format::Table).unwrap();
        let options = Options {
            format: Format::Table,
            header: true,
            width: None,
        };
        let mut out = Vec::new();
        output::render_table(&mut out, &table, &options).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME                    IBAN                    BIC          BANK        TOWN               BOOKINGS\n\
             Stadtwerke Musterstadt  DE31999900011000471100  MUBADEM0XXX  Musterbank  12345 Musterstadt         3\n"
        );
    }

    #[test]
    fn the_other_formats_carry_every_column_of_the_table_under_its_name() {
        let (_dir, database) = sample();
        for format in [Format::Json, Format::Jsonl, Format::Csv] {
            let table = table(&database, None, format).unwrap();
            let keys: Vec<&str> = table.columns().iter().map(|c| c.name()).collect();
            let mut expected: Vec<&str> = schema::find_table(schema::table::PAYEES)
                .unwrap()
                .column_names()
                .collect();
            expected.push(BOOKINGS);
            assert_eq!(keys, expected);
            assert_eq!(table.len(), 7);
        }
        let table = table(&database, Some("öztürk"), Format::Jsonl).unwrap();
        let options = Options {
            format: Format::Jsonl,
            header: true,
            width: None,
        };
        let mut out = Vec::new();
        output::render_table(&mut out, &table, &options).unwrap();
        let line = String::from_utf8(out).unwrap();
        assert!(line.starts_with("{\"id\":2,"), "{line}");
        assert!(line.contains("\"name\":\"Ärztin Dr. Öztürk\""), "{line}");
        assert!(line.contains("\"iban\":null"), "{line}");
        assert!(
            line.contains("\"amount\":80.50,\"currency\":\"EUR\""),
            "{line}"
        );
        assert!(line.ends_with("\"booking_count\":2}\n"), "{line}");
    }

    #[test]
    fn the_filter_is_a_substring_of_any_text_column_without_regard_to_case() {
        let (_dir, database) = sample();
        assert_eq!(names(&database, "MUSTERSTADT"), ["Stadtwerke Musterstadt"]);
        assert_eq!(names(&database, "ärztin"), ["Ärztin Dr. Öztürk"]);
        assert_eq!(names(&database, "ÖZTÜRK"), ["Ärztin Dr. Öztürk"]);
        // IBAN, BIC, bank, town and notes.
        assert_eq!(names(&database, "de0299"), ["100% Bio GmbH"]);
        assert_eq!(names(&database, "mubadem0"), ["Stadtwerke Musterstadt"]);
        assert_eq!(names(&database, "musterbank"), ["Stadtwerke Musterstadt"]);
        assert_eq!(names(&database, "zürich"), ["snake_case AG"]);
        assert_eq!(names(&database, "am markt"), ["Ärztin Dr. Öztürk"]);
        assert!(names(&database, "nothing has this").is_empty());
        // A payee without a name is found by its IBAN.
        let table = table(&database, Some("DE9999"), Format::Table).unwrap();
        assert_eq!(texts(&table, "name"), [None]);
    }

    #[test]
    fn an_iban_typed_with_blanks_is_found() {
        let (_dir, database) = sample();
        assert_eq!(
            names(&database, "DE31 9999 0001 1000"),
            ["Stadtwerke Musterstadt"]
        );
        // Blanks alone do not match every payee that has an IBAN.
        assert_eq!(
            names(&database, " "),
            [
                "100% Bio GmbH",
                "Nobody Pays",
                "snake_case AG",
                "Stadtwerke Musterstadt",
                "zoo Berlin",
                "Ärztin Dr. Öztürk"
            ]
        );
    }

    #[test]
    fn percent_underscore_and_quotes_in_the_filter_stand_for_themselves() {
        let (_dir, database) = sample();
        assert_eq!(names(&database, "%"), ["100% Bio GmbH"]);
        assert_eq!(names(&database, "0% b"), ["100% Bio GmbH"]);
        assert_eq!(names(&database, "_"), ["snake_case AG"]);
        assert_eq!(names(&database, "e_c"), ["snake_case AG"]);
        assert!(names(&database, "s%e").is_empty());
        assert!(names(&database, "sn_ke").is_empty());
        assert!(names(&database, "' or 1=1 --").is_empty());
        assert!(names(&database, "\\").is_empty());
    }

    #[test]
    fn no_match_is_an_empty_table_with_its_columns() {
        let (_dir, database) = sample();
        let table = table(&database, Some("nothing has this"), Format::Csv).unwrap();
        assert!(table.is_empty());
        assert_eq!(table.columns().last().unwrap().name(), BOOKINGS);
    }
}
