//! `b4 bookings list|show|summary`: the bookings in the database.

use crate::cli::{
    BookingSort, BookingsCommand, BookingsListArgs, BookingsShowArgs, BookingsSummaryArgs, Format,
    Globals, SummaryBy,
};
use crate::commands::account_ref;
use crate::commands::booking_filter::Selection;
use crate::commands::record;
use crate::db::read::{quote_identifier, Database, Hint, Param};
use crate::db::schema::{self, table, view};
use crate::error::Context;
use crate::output::{self, Table};
use crate::Error;

/// The columns of `bookings list` in the machine formats (json, jsonl, csv),
/// in this order. All of them are columns of the bookings view under the
/// names the schema gives them, so `b4 schema bookings_view` describes each.
const LIST_COLUMNS: [&str; 24] = [
    "account_id",
    "id",
    "account_name",
    "account_iban",
    "booking_date",
    "value_date",
    "status",
    "pending",
    "amount",
    "currency",
    "debit",
    "payee_name",
    "payee_iban",
    "payee_bic",
    "purpose",
    "category",
    "booking_text",
    "notes",
    "counted",
    "split_role",
    "master_id",
    "end_to_end_id",
    "mandate_id",
    "creditor_id",
];

/// The amounts of a booking and the column that holds the currency of each.
const MONEY_COLUMNS: [(&str, &str); 4] = [
    ("amount", "currency"),
    ("vat_amount", "currency"),
    ("instructed_amount", "instructed_currency"),
    ("charges_amount", "charges_currency"),
];

/// Run a verb of `b4 bookings`.
pub fn run(command: &BookingsCommand, globals: &Globals) -> Result<(), Error> {
    match command {
        BookingsCommand::List(args) => list(args, globals),
        BookingsCommand::Show(args) => show(args, globals),
        BookingsCommand::Summary(args) => summary(args, globals),
    }
}

/// `b4 bookings list [filters] [--sort <KEY>] [--reverse] [--limit <N>]`
///
/// Prints the bookings that pass every given filter, read from the view that
/// joins the account onto the bookings. The filters are those of
/// [`Selection::new`]; an `--account` is resolved by the rule in the module
/// documentation of `commands`, and a hidden account can be named.
///
/// Every stored row is listed: the master row of a split or a batch and its
/// parts. (`bookings summary` counts the parts only.)
///
/// Order: `--sort`, date by default with the oldest first, so the newest
/// booking is the last line. A booking without a booking date, which is a
/// pending one, comes after the dated ones. `amount` is the signed amount,
/// smallest first; `payee` is the payee name without regard to case. Ties are
/// broken by date, then id, then account, so the order is total, and
/// `--reverse` turns the whole of it round. `--limit` keeps the first N rows
/// of that order.
///
/// # Output
///
/// * `table`, for people: id, date, account, payee, purpose, category and
///   the signed amount with its currency. Payee, purpose and category are
///   cut to fit the terminal. A pending booking has ` *` after its date, or
///   `pending` in its place when it has no date yet. No colour, not even for
///   negative amounts.
/// * `json`, `jsonl`, `csv`, for programs: the columns of [`LIST_COLUMNS`]
///   under their schema names, complete and unformatted.
fn list(args: &BookingsListArgs, globals: &Globals) -> Result<(), Error> {
    let database = Database::open(&globals.db)?;
    let table = list_table(&database, args, globals.format() == Format::Table)?;
    output::print_table(&table, globals)
}

/// The result of `bookings list`: the columns for people with `for_people`,
/// else the columns for programs.
fn list_table(
    database: &Database,
    args: &BookingsListArgs,
    for_people: bool,
) -> Result<Table, Error> {
    let accounts = account_ref::resolve_ids(database, &args.filters.accounts)?;
    let selection = Selection::new(&args.filters, &accounts);

    let (columns, hints) = if for_people {
        (
            "id, \
             case when booking_date is null and pending then 'pending' \
                  when pending then strftime(booking_date, '%Y-%m-%d') || ' *' \
                  else strftime(booking_date, '%Y-%m-%d') end as date, \
             coalesce(account_name, cast(account_id as varchar)) as account, \
             payee_name, purpose, category, amount, currency"
                .to_owned(),
            vec![
                Hint::new("id").title("ID"),
                Hint::new("date").title("Date"),
                Hint::new("account").title("Account"),
                Hint::new("payee_name").title("Payee").flexible(),
                Hint::new("purpose").title("Purpose").flexible(),
                Hint::new("category").title("Category").flexible(),
                Hint::new("amount").title("Amount").money("currency"),
            ],
        )
    } else {
        (
            LIST_COLUMNS.join(", "),
            vec![Hint::new("amount").money("currency")],
        )
    };

    let mut sql = format!(
        "select {columns} from {}{} order by {}",
        view::BOOKINGS,
        selection.where_clause(),
        order_by(args.sort, args.reverse)
    );
    if let Some(limit) = args.limit {
        // A number that clap parsed, not text.
        sql.push_str(&format!(" limit {limit}"));
    }
    database
        .query_with(&sql, selection.params(), &hints)
        .context("cannot list the bookings")
}

/// The `order by` list for `sort`. Every key has the same direction and a
/// missing value counts as the largest, so `reverse` gives the exact mirror
/// image of the order.
fn order_by(sort: BookingSort, reverse: bool) -> String {
    let leading: &[&str] = match sort {
        BookingSort::Date => &[],
        BookingSort::Amount => &["amount"],
        BookingSort::Payee => &["lower(payee_name)"],
    };
    let direction = if reverse {
        "desc nulls first"
    } else {
        "asc nulls last"
    };
    leading
        .iter()
        .chain(&["booking_date", "id", "account_id"])
        .map(|key| format!("{key} {direction}"))
        .collect::<Vec<String>>()
        .join(", ")
}

/// `b4 bookings show <ID> [--account <ACCOUNT>]`
///
/// Prints every stored field of the booking with the id `args.id`, in the
/// order of the table's columns, with the name and the IBAN of its account
/// after the account id.
///
/// An id is only unique within an account. Without `--account` every booking
/// with the id is printed; with it, the one of that account. No booking with
/// the id is an error.
///
/// # Output
///
/// * `table`: one `Title: value` line per field that has a value; several
///   bookings are separated by an empty line.
/// * `json`: one object with every field, also the empty ones (`null`). If
///   several accounts have a booking with the id, an array of such objects.
/// * `jsonl`: one object per booking and line; `csv`: a header and one row
///   per booking.
fn show(args: &BookingsShowArgs, globals: &Globals) -> Result<(), Error> {
    let database = Database::open(&globals.db)?;
    let table = show_table(&database, args)?;
    if globals.format() == Format::Table {
        for index in 0..table.len() {
            if index > 0 {
                output::print_bytes(b"\n")?;
            }
            output::print_record(&record::of_row(&table, index, true), globals)?;
        }
        Ok(())
    } else if table.len() == 1 {
        output::print_record(&record::of_row(&table, 0, false), globals)
    } else {
        output::print_table(&table, globals)
    }
}

/// The bookings `bookings show` prints, one row each with every field. At
/// least one row, or an error.
fn show_table(database: &Database, args: &BookingsShowArgs) -> Result<Table, Error> {
    let account = args
        .account
        .as_deref()
        .map(|reference| account_ref::resolve(database, reference))
        .transpose()?;

    let stored = schema::find_table(table::BOOKINGS)
        .ok_or_else(|| Error::new("the schema has no table of bookings"))?;
    let mut names: Vec<&str> = Vec::new();
    for name in stored.column_names() {
        names.push(name);
        if name == "account_id" {
            names.extend(["account_name", "account_iban"]);
        }
    }
    let columns: Vec<String> = names.iter().map(|name| quote_identifier(name)).collect();

    let mut sql = format!(
        "select {} from {} where id = ?",
        columns.join(", "),
        view::BOOKINGS
    );
    let mut params = vec![Param::from(args.id)];
    if let Some(account) = &account {
        sql.push_str(" and account_id = ?");
        params.push(Param::from(account.id));
    }
    sql.push_str(" order by account_id, booking_date");

    let hints = record::hints(names.iter().copied(), &MONEY_COLUMNS);
    let found = database
        .query_with(&sql, &params, &hints)
        .context(format!("cannot read the booking {}", args.id))?;
    if found.is_empty() {
        let id = args.id;
        return Err(Error::new(match &account {
            Some(account) => format!(
                "the account {} ({}) has no booking with the id {id}",
                account.name.as_deref().unwrap_or("without a name"),
                account.id
            ),
            None => format!(
                "no booking has the id {id}: `b4 bookings list` lists the bookings with their ids"
            ),
        }));
    }
    Ok(found)
}

/// `b4 bookings summary --by <month|year|category|payee|account> [filters]`
///
/// Groups the bookings that pass the filters (the same as for `bookings
/// list`, built by the same code) and prints one row per group and currency
/// with the number of bookings, the sum of the debits (negative), the sum of
/// the credits and the net amount, which is their sum.
///
/// Only rows whose `counted` column is true take part: the master row of a
/// split or a batch stands for its parts and would count the money twice.
/// Sums are exact decimals computed by the database, and amounts of
/// different currencies are never added: the currency is part of the group.
/// Pending bookings are included unless `--booked` is given.
///
/// Groups: `month` and `year` are those of the booking date as `YYYY-MM` and
/// `YYYY`; bookings without a booking date form one group of their own, the
/// last. `category` is the full category name, each level on its own (a
/// parent's row does not include its children); `payee` the payee name;
/// `account` the account. Month and year are in ascending order, the others
/// ordered by name without regard to case, a group without a name last.
///
/// The group without a value is `null` in the machine formats, and `(no
/// date)` or `(none)` in table output.
fn summary(args: &BookingsSummaryArgs, globals: &Globals) -> Result<(), Error> {
    let database = Database::open(&globals.db)?;
    let table = summary_table(&database, args, globals.format() == Format::Table)?;
    output::print_table(&table, globals)
}

/// The result of `bookings summary`; with `for_people` a group without a
/// value gets a placeholder as its name.
fn summary_table(
    database: &Database,
    args: &BookingsSummaryArgs,
    for_people: bool,
) -> Result<Table, Error> {
    let accounts = account_ref::resolve_ids(database, &args.filters.accounts)?;
    let mut selection = Selection::new(&args.filters, &accounts);
    selection.require("counted");

    // What is selected, what is grouped by, what is ordered by, and the
    // title of the group column. `label` wraps the expression of the group
    // for people.
    let label = |expression: &str, placeholder: &str| {
        if for_people {
            format!("coalesce({expression}, '{placeholder}')")
        } else {
            expression.to_owned()
        }
    };
    let (select, group, order, hint) = match args.by {
        SummaryBy::Month => (
            format!(
                "{} as month",
                label("strftime(booking_date, '%Y-%m')", "(no date)")
            ),
            "strftime(booking_date, '%Y-%m')",
            "strftime(booking_date, '%Y-%m') nulls last",
            Hint::new("month").title("Month"),
        ),
        SummaryBy::Year => (
            format!(
                "{} as year",
                label("strftime(booking_date, '%Y')", "(no date)")
            ),
            "strftime(booking_date, '%Y')",
            "strftime(booking_date, '%Y') nulls last",
            Hint::new("year").title("Year"),
        ),
        // Names that differ only in case are one category.
        SummaryBy::Category => (
            format!("{} as category", label("min(category)", "(none)")),
            "category_key",
            "lower(min(category)) nulls last",
            Hint::new("category").title("Category").flexible(),
        ),
        SummaryBy::Payee => (
            format!("{} as payee_name", label("payee_name", "(none)")),
            "payee_name",
            "lower(payee_name) nulls last, payee_name",
            Hint::new("payee_name").title("Payee").flexible(),
        ),
        SummaryBy::Account => (
            format!(
                "account_id, {} as account_name",
                label("min(account_name)", "(none)")
            ),
            "account_id",
            "lower(min(account_name)) nulls last, account_id",
            Hint::new("account_name").title("Account").flexible(),
        ),
    };

    let sql = format!(
        "select {select}, currency, count(*) as booking_count, \
           cast(coalesce(sum(amount) filter (where debit), 0) as decimal(38,4)) as debits, \
           cast(coalesce(sum(amount) filter (where not debit), 0) as decimal(38,4)) as credits, \
           cast(coalesce(sum(amount), 0) as decimal(38,4)) as net \
         from {}{} group by {group}, currency order by {order}, currency nulls last",
        view::BOOKINGS,
        selection.where_clause()
    );
    let hints = [
        hint,
        Hint::new("account_id").title("ID"),
        Hint::new("booking_count").title("Bookings"),
        Hint::new("debits").title("Debits").money("currency"),
        Hint::new("credits").title("Credits").money("currency"),
        Hint::new("net").title("Net").money("currency"),
    ];
    database
        .query_with(&sql, selection.params(), &hints)
        .context("cannot sum up the bookings")
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;
    use crate::cli::BookingFilters;
    use crate::commands::testdb::{self, lines, rendered, rendered_record, CARD, GIRO, OLD};
    use crate::output::Cell;

    fn list_args(filters: BookingFilters) -> BookingsListArgs {
        BookingsListArgs {
            filters,
            sort: BookingSort::Date,
            reverse: false,
            limit: None,
        }
    }

    /// The value of the column `name` in every row.
    fn column(table: &Table, name: &str) -> Vec<Cell> {
        let index = table
            .columns()
            .iter()
            .position(|column| column.name() == name)
            .unwrap_or_else(|| panic!("no column {name}"));
        table.rows().iter().map(|row| row[index].clone()).collect()
    }

    /// The `(account, id)` of every row of a machine-format list.
    fn keys(table: &Table) -> Vec<(i64, i64)> {
        column(table, "account_id")
            .into_iter()
            .zip(column(table, "id"))
            .map(|pair| match pair {
                (Cell::Integer(account), Cell::Integer(id)) => (account, id),
                other => panic!("unexpected key {other:?}"),
            })
            .collect()
    }

    fn sorted(database: &Database, sort: BookingSort, reverse: bool) -> Vec<(i64, i64)> {
        let args = BookingsListArgs {
            sort,
            reverse,
            ..list_args(BookingFilters::default())
        };
        keys(&list_table(database, &args, false).unwrap())
    }

    fn money(text: &str) -> Cell {
        Cell::Decimal(text.parse::<Decimal>().unwrap())
    }

    #[test]
    fn the_list_for_people_shows_what_a_person_reads() {
        let (_dir, database) = testdb::open();
        let table = list_table(&database, &list_args(BookingFilters::default()), true).unwrap();
        let text = rendered(&table, Format::Table);
        let expected = r" ID  DATE          ACCOUNT          PAYEE                      PURPOSE                               CATEGORY                       AMOUNT
300  2024-01-01    Altes Sparkonto  Übertrag                   Anfangsbestand                        (nicht auswerten)          500.00 EUR
100  2024-01-15    Girokonto        Beispielwerke AG           Lohn/Gehalt 01/2024                   Einkommen:Gehalt          3000.00 EUR
101  2024-01-31    Girokonto        Hausverwaltung Muster      Miete Januar                          Wohnen:Miete              -850.00 EUR
100  2024-02-01    Kreditkarte      Example Software Inc.      Subscription                          Sonstiges                  -49.00 USD
102  2024-02-10    Girokonto        Stadtwerke Musterstadt     Abschlag 100% Ökostrom ↵ Vertrag a_b  Wohnen:Nebenkosten:Strom   -45.50 EUR
103  2024-02-14    Girokonto        Drogerie Beispiel          Kartenzahlung                                                    -84.37 EUR
104  2024-02-14    Girokonto        Drogerie Beispiel          Kartenzahlung                         Haushalt                   -52.17 EUR
105  2024-02-14    Girokonto        Drogerie Beispiel          Kartenzahlung                         Gesundheit                 -32.20 EUR
202  2024-02-20    Kreditkarte      Example Software Inc.      Refund                                Sonstiges                   49.00 USD
106  2024-03-18    Girokonto        Sammelüberweisung          Sammelüberweisung 2 Posten                                      -244.88 EUR
107  2024-03-18    Girokonto        Schornsteinfeger Beispiel  Rechnung 2024/118                     Wohnen:Nebenkosten        -144.88 EUR
108  2024-03-18    Girokonto        Sportverein Beispiel       Beitrag                                                         -100.00 EUR
110  2024-03-20    Girokonto        Verkehrsbetriebe           Monatskarte                           Mobilität:ÖPNV             -12.00 EUR
111  2024-03-25    Girokonto                                                                                                      0.00 EUR
201  2024-03-30 *  Kreditkarte      Cafe Example                                                                                -20.00 USD
109  pending       Girokonto        Supermarkt Beispiel        Kartenzahlung                                                    -67.12 EUR
";
        assert_eq!(lines(&text), lines(expected));
        // No colour, not even for negative amounts.
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn a_narrow_terminal_cuts_payee_purpose_and_category() {
        let (_dir, database) = testdb::open();
        let table = list_table(&database, &list_args(BookingFilters::default()), true).unwrap();
        let options = output::Options {
            format: Format::Table,
            header: true,
            width: Some(100),
        };
        let mut out = Vec::new();
        output::render_table(&mut out, &table, &options).unwrap();
        let text = String::from_utf8(out).unwrap();
        for line in text.lines() {
            assert!(line.chars().count() <= 100, "{line}");
        }
        assert!(text.contains('…'), "{text}");
        // Id, date, account and amount are never cut.
        assert!(
            text.contains("102  2024-02-10    Girokonto        "),
            "{text}"
        );
        assert!(text.contains("  -144.88 EUR\n"), "{text}");
    }

    #[test]
    fn the_machine_formats_carry_the_documented_columns_under_schema_names() {
        let (_dir, database) = testdb::open();
        let table = list_table(&database, &list_args(BookingFilters::default()), false).unwrap();
        let names: Vec<&str> = table.columns().iter().map(|c| c.name()).collect();
        assert_eq!(names, LIST_COLUMNS);

        // Every one of them is a column the schema describes.
        let stored = schema::find_table(table::BOOKINGS).unwrap();
        let view = schema::find_view(view::BOOKINGS).unwrap();
        for name in LIST_COLUMNS {
            assert!(
                stored.column_names().any(|column| column == name)
                    || view.columns.iter().any(|column| column.name == name),
                "{name} is not a column of {}",
                view.name
            );
        }

        let csv = rendered(&table, Format::Csv);
        assert_eq!(
            csv.lines().next().unwrap(),
            "account_id,id,account_name,account_iban,booking_date,value_date,status,pending,\
             amount,currency,debit,payee_name,payee_iban,payee_bic,purpose,category,\
             booking_text,notes,counted,split_role,master_id,end_to_end_id,mandate_id,\
             creditor_id"
        );
        // One line more than rows: a purpose of two lines is one quoted field.
        assert_eq!(csv.lines().count(), 1 + 16 + 1);
        assert!(
            csv.contains(",\"Abschlag 100% Ökostrom\nVertrag a_b\","),
            "{csv}"
        );
        // A booking with nothing set: empty fields, and nulls in JSON.
        assert!(
            csv.contains("\n1,111,Girokonto,DE31999900011000471100,2024-03-25,,booked,false,0.00,EUR,true,,,,,,,,true,,,,,\n"),
            "{csv}"
        );
        let jsonl = rendered(&table, Format::Jsonl);
        assert!(
            jsonl.contains(
                "{\"account_id\":1,\"id\":109,\"account_name\":\"Girokonto\",\
                 \"account_iban\":\"DE31999900011000471100\",\"booking_date\":null,\
                 \"value_date\":\"2024-04-01\",\"status\":\"pending\",\"pending\":true,\
                 \"amount\":-67.12,\"currency\":\"EUR\",\"debit\":true,\
                 \"payee_name\":\"Supermarkt Beispiel\",\"payee_iban\":null,\"payee_bic\":null,\
                 \"purpose\":\"Kartenzahlung\",\"category\":null,\"booking_text\":null,\
                 \"notes\":null,\"counted\":true,\"split_role\":null,\"master_id\":null,\
                 \"end_to_end_id\":null,\"mandate_id\":null,\"creditor_id\":null}\n"
            ),
            "{jsonl}"
        );
        // The master of a split is listed and says that it does not count.
        assert!(jsonl.contains("\"id\":103,"), "{jsonl}");
        assert!(
            jsonl.contains("\"counted\":false,\"split_role\":\"split_master\",\"master_id\":103,"),
            "{jsonl}"
        );
        assert!(
            jsonl.contains("\"counted\":true,\"split_role\":\"batch_slave\",\"master_id\":106,"),
            "{jsonl}"
        );
    }

    #[test]
    fn the_default_order_is_by_date_with_undated_bookings_last() {
        let (_dir, database) = testdb::open();
        let by_date = sorted(&database, BookingSort::Date, false);
        assert_eq!(
            by_date,
            [
                (OLD, 300),
                (GIRO, 100),
                (GIRO, 101),
                (CARD, 100),
                (GIRO, 102),
                (GIRO, 103),
                (GIRO, 104),
                (GIRO, 105),
                (CARD, 202),
                (GIRO, 106),
                (GIRO, 107),
                (GIRO, 108),
                (GIRO, 110),
                (GIRO, 111),
                (CARD, 201),
                (GIRO, 109),
            ]
        );
    }

    #[test]
    fn reverse_is_the_mirror_image_of_every_order() {
        let (_dir, database) = testdb::open();
        for sort in [BookingSort::Date, BookingSort::Amount, BookingSort::Payee] {
            let mut forward = sorted(&database, sort, false);
            forward.reverse();
            assert_eq!(sorted(&database, sort, true), forward, "{sort:?}");
        }
        // The undated pending booking is the newest: first when reversed.
        assert_eq!(sorted(&database, BookingSort::Date, true)[0], (GIRO, 109));
    }

    #[test]
    fn amount_sorts_by_the_signed_amount_and_payee_by_name() {
        let (_dir, database) = testdb::open();
        let by_amount = sorted(&database, BookingSort::Amount, false);
        assert_eq!(by_amount[..3], [(GIRO, 101), (GIRO, 106), (GIRO, 107)]);
        assert_eq!(by_amount[13..], [(CARD, 202), (OLD, 300), (GIRO, 100)]);
        // Equal amounts: by date.
        let ties: Vec<(i64, i64)> = by_amount
            .iter()
            .copied()
            .filter(|key| [(CARD, 100), (CARD, 202)].contains(key))
            .collect();
        assert_eq!(ties, [(CARD, 100), (CARD, 202)]);

        let by_payee = sorted(&database, BookingSort::Payee, false);
        assert_eq!(
            by_payee[..6],
            [
                (GIRO, 100),
                (CARD, 201),
                (GIRO, 103),
                (GIRO, 104),
                (GIRO, 105),
                (CARD, 100),
            ]
        );
        // No payee: last.
        assert_eq!(by_payee[15], (GIRO, 111));
    }

    #[test]
    fn limit_keeps_the_first_rows_of_the_order() {
        let (_dir, database) = testdb::open();
        let limited = |sort, reverse, limit| {
            let args = BookingsListArgs {
                sort,
                reverse,
                limit: Some(limit),
                filters: BookingFilters::default(),
            };
            keys(&list_table(&database, &args, false).unwrap())
        };
        assert_eq!(
            limited(BookingSort::Date, false, 2),
            [(OLD, 300), (GIRO, 100)]
        );
        // The most recent ones, newest first.
        assert_eq!(
            limited(BookingSort::Date, true, 3),
            [(GIRO, 109), (CARD, 201), (GIRO, 111)]
        );
        assert_eq!(limited(BookingSort::Amount, true, 1), [(GIRO, 100)]);
        assert_eq!(limited(BookingSort::Date, false, 1000).len(), 16);

        // The limit applies after the filters.
        let args = BookingsListArgs {
            limit: Some(1),
            reverse: true,
            ..list_args(BookingFilters {
                category: Some("wohnen".to_owned()),
                ..BookingFilters::default()
            })
        };
        assert_eq!(
            keys(&list_table(&database, &args, false).unwrap()),
            [(GIRO, 107)]
        );
    }

    #[test]
    fn accounts_are_resolved_and_a_hidden_one_can_be_named() {
        let (_dir, database) = testdb::open();
        let of = |references: &[&str]| {
            let filters = BookingFilters {
                accounts: references.iter().map(|text| (*text).to_owned()).collect(),
                ..BookingFilters::default()
            };
            list_table(&database, &list_args(filters), false).map(|table| keys(&table))
        };
        assert_eq!(of(&["altes"]).unwrap(), [(OLD, 300)]);
        assert_eq!(
            of(&["4711", "DE84 9999 0003 3000 4711 44"]).unwrap(),
            [(OLD, 300), (CARD, 100), (CARD, 202), (CARD, 201)]
        );
        assert_eq!(of(&["1"]).unwrap().len(), 12);
        // Bookings of the hidden account are listed without naming it, too.
        assert!(of(&[]).unwrap().contains(&(OLD, 300)));

        let error = of(&["giro", "konto"]).unwrap_err().to_string();
        assert!(error.starts_with("`konto` matches several accounts:\n  1  Girokonto\n"));
        let error = of(&["nope"]).unwrap_err().to_string();
        assert!(error.starts_with("no account matches `nope`"), "{error}");
    }

    #[test]
    fn an_empty_result_is_a_table_without_rows() {
        let (_dir, database) = testdb::open();
        let filters = BookingFilters {
            payee: Some("nobody".to_owned()),
            ..BookingFilters::default()
        };
        for for_people in [true, false] {
            let table = list_table(&database, &list_args(filters.clone()), for_people).unwrap();
            assert!(table.is_empty());
            assert_eq!(rendered(&table, Format::Table), "");
            assert_eq!(rendered(&table, Format::Json), "[]\n");
            assert_eq!(rendered(&table, Format::Jsonl), "");
        }
        let table = list_table(&database, &list_args(filters.clone()), false).unwrap();
        assert!(rendered(&table, Format::Csv).starts_with("account_id,id,account_name,"));
        assert_eq!(rendered(&table, Format::Csv).lines().count(), 1);

        let args = BookingsSummaryArgs {
            by: SummaryBy::Month,
            filters,
        };
        let table = summary_table(&database, &args, false).unwrap();
        assert!(table.is_empty());
        assert_eq!(
            rendered(&table, Format::Csv),
            "month,currency,booking_count,debits,credits,net\n"
        );
    }

    fn summary_of(database: &Database, by: SummaryBy, filters: BookingFilters) -> Table {
        summary_table(database, &BookingsSummaryArgs { by, filters }, false).unwrap()
    }

    #[test]
    fn a_summary_counts_a_split_and_a_batch_once() {
        let (_dir, database) = testdb::open();
        let table = summary_of(&database, SummaryBy::Account, BookingFilters::default());
        assert_eq!(
            rendered(&table, Format::Jsonl),
            "\
{\"account_id\":3,\"account_name\":\"Altes Sparkonto\",\"currency\":\"EUR\",\"booking_count\":1,\"debits\":0.00,\"credits\":500.00,\"net\":500.00}
{\"account_id\":1,\"account_name\":\"Girokonto\",\"currency\":\"EUR\",\"booking_count\":10,\"debits\":-1303.87,\"credits\":3000.00,\"net\":1696.13}
{\"account_id\":2,\"account_name\":\"Kreditkarte\",\"currency\":\"USD\",\"booking_count\":3,\"debits\":-69.00,\"credits\":49.00,\"net\":-20.00}
"
        );
        // The list has the two masters as well: 12 rows, 10 of them counted.
        let giro = BookingFilters {
            accounts: vec!["giro".to_owned()],
            ..BookingFilters::default()
        };
        let listed = list_table(&database, &list_args(giro.clone()), false).unwrap();
        assert_eq!(listed.len(), 12);
        let counted = column(&listed, "counted")
            .iter()
            .filter(|cell| **cell == Cell::Boolean(true))
            .count();
        assert_eq!(counted, 10);

        // The split of 84.37 is the sum of its two parts under one payee, and
        // the parts keep their own categories.
        let drogerie = BookingFilters {
            payee: Some("drogerie".to_owned()),
            ..BookingFilters::default()
        };
        let table = summary_of(&database, SummaryBy::Payee, drogerie.clone());
        assert_eq!(column(&table, "booking_count"), [Cell::Integer(2)]);
        assert_eq!(column(&table, "net"), [money("-84.37")]);
        let table = summary_of(&database, SummaryBy::Category, drogerie);
        assert_eq!(
            column(&table, "category"),
            [Cell::from("Gesundheit"), Cell::from("Haushalt")]
        );

        // The batch of 244.88 counts as its two transfers, in March.
        let march = BookingFilters {
            from: Some("2024-03-18".parse().unwrap()),
            to: Some("2024-03-18".parse().unwrap()),
            ..BookingFilters::default()
        };
        let table = summary_of(&database, SummaryBy::Month, march);
        assert_eq!(column(&table, "month"), [Cell::from("2024-03")]);
        assert_eq!(column(&table, "booking_count"), [Cell::Integer(2)]);
        assert_eq!(column(&table, "debits"), [money("-244.88")]);
    }

    #[test]
    fn a_summary_never_adds_different_currencies() {
        let (_dir, database) = testdb::open();
        let table = summary_table(
            &database,
            &BookingsSummaryArgs {
                by: SummaryBy::Month,
                filters: BookingFilters::default(),
            },
            true,
        )
        .unwrap();
        let expected = "\
MONTH      BOOKINGS       DEBITS      CREDITS          NET
2024-01           3  -850.00 EUR  3500.00 EUR  2650.00 EUR
2024-02           3  -129.87 EUR     0.00 EUR  -129.87 EUR
2024-02           2   -49.00 USD    49.00 USD     0.00 USD
2024-03           4  -256.88 EUR     0.00 EUR  -256.88 EUR
2024-03           1   -20.00 USD     0.00 USD   -20.00 USD
(no date)         1   -67.12 EUR     0.00 EUR   -67.12 EUR
";
        assert_eq!(lines(&rendered(&table, Format::Table)), lines(expected));

        let by_year = summary_of(&database, SummaryBy::Year, BookingFilters::default());
        assert_eq!(
            rendered(&by_year, Format::Csv),
            "year,currency,booking_count,debits,credits,net\n\
             2024,EUR,10,-1236.75,3500.00,2263.25\n\
             2024,USD,3,-69.00,49.00,-20.00\n\
             ,EUR,1,-67.12,0.00,-67.12\n"
        );
    }

    #[test]
    fn a_summary_by_category_or_payee_is_ordered_by_name() {
        let (_dir, database) = testdb::open();
        let table = summary_table(
            &database,
            &BookingsSummaryArgs {
                by: SummaryBy::Category,
                filters: BookingFilters::default(),
            },
            true,
        )
        .unwrap();
        let expected = "\
CATEGORY                  BOOKINGS       DEBITS      CREDITS          NET
(nicht auswerten)                1     0.00 EUR   500.00 EUR   500.00 EUR
Einkommen:Gehalt                 1     0.00 EUR  3000.00 EUR  3000.00 EUR
Gesundheit                       1   -32.20 EUR     0.00 EUR   -32.20 EUR
Haushalt                         1   -52.17 EUR     0.00 EUR   -52.17 EUR
Mobilität:ÖPNV                   1   -12.00 EUR     0.00 EUR   -12.00 EUR
Sonstiges                        2   -49.00 USD    49.00 USD     0.00 USD
Wohnen:Miete                     1  -850.00 EUR     0.00 EUR  -850.00 EUR
Wohnen:Nebenkosten               1  -144.88 EUR     0.00 EUR  -144.88 EUR
Wohnen:Nebenkosten:Strom         1   -45.50 EUR     0.00 EUR   -45.50 EUR
(none)                           3  -167.12 EUR     0.00 EUR  -167.12 EUR
(none)                           1   -20.00 USD     0.00 USD   -20.00 USD
";
        assert_eq!(lines(&rendered(&table, Format::Table)), lines(expected));

        // For a program the group without a name is null, not a placeholder.
        let table = summary_of(&database, SummaryBy::Category, BookingFilters::default());
        assert_eq!(column(&table, "category")[9..], [Cell::Null, Cell::Null]);

        let table = summary_of(&database, SummaryBy::Payee, BookingFilters::default());
        let payees = column(&table, "payee_name");
        assert_eq!(payees.len(), 12);
        assert_eq!(payees[0], Cell::from("Beispielwerke AG"));
        assert_eq!(payees[3], Cell::from("Example Software Inc."));
        assert_eq!(payees[11], Cell::Null);
    }

    #[test]
    fn a_summary_applies_the_filters_of_the_list() {
        let (_dir, database) = testdb::open();
        let filters = BookingFilters {
            accounts: vec!["giro".to_owned()],
            category: Some("wohnen".to_owned()),
            ..BookingFilters::default()
        };
        let table = summary_of(&database, SummaryBy::Year, filters.clone());
        assert_eq!(column(&table, "booking_count"), [Cell::Integer(3)]);
        assert_eq!(column(&table, "net"), [money("-1040.38")]);
        // The same bookings the list shows.
        let listed = list_table(&database, &list_args(filters), false).unwrap();
        assert_eq!(listed.len(), 3);

        // --booked leaves the pending bookings out, --debit the credits.
        let booked = BookingFilters {
            booked: true,
            ..BookingFilters::default()
        };
        let table = summary_of(&database, SummaryBy::Account, booked);
        assert_eq!(
            column(&table, "booking_count"),
            [Cell::Integer(1), Cell::Integer(9), Cell::Integer(2)]
        );
        assert_eq!(
            column(&table, "net"),
            [money("500"), money("1763.25"), money("0")]
        );
        let debit = BookingFilters {
            debit: true,
            ..BookingFilters::default()
        };
        let table = summary_of(&database, SummaryBy::Account, debit);
        assert_eq!(
            column(&table, "credits"),
            [money("0"), money("0")],
            "an account without debits has no row"
        );
        assert_eq!(column(&table, "debits"), [money("-1303.87"), money("-69")]);

        let unknown = BookingFilters {
            accounts: vec!["nope".to_owned()],
            ..BookingFilters::default()
        };
        let args = BookingsSummaryArgs {
            by: SummaryBy::Month,
            filters: unknown,
        };
        assert!(summary_table(&database, &args, false).is_err());
    }

    #[test]
    fn show_prints_every_booking_with_the_id_or_the_one_of_an_account() {
        let (_dir, database) = testdb::open();
        let show = |id, account: Option<&str>| {
            show_table(
                &database,
                &BookingsShowArgs {
                    id,
                    account: account.map(str::to_owned),
                },
            )
        };
        let both = show(100, None).unwrap();
        assert_eq!(
            column(&both, "account_name"),
            [Cell::from("Girokonto"), Cell::from("Kreditkarte")]
        );
        let one = show(100, Some("kredit")).unwrap();
        assert_eq!(column(&one, "account_id"), [Cell::Integer(CARD)]);
        assert_eq!(column(&one, "amount"), [money("-49")]);
        assert_eq!(show(300, Some("3")).unwrap().len(), 1);

        assert_eq!(
            show(999, None).unwrap_err().to_string(),
            "no booking has the id 999: `b4 bookings list` lists the bookings with their ids"
        );
        assert_eq!(
            show(300, Some("giro")).unwrap_err().to_string(),
            "the account Girokonto (1) has no booking with the id 300"
        );
        assert!(show(100, Some("konto"))
            .unwrap_err()
            .to_string()
            .contains("matches several accounts"));
    }

    #[test]
    fn show_has_every_stored_field_and_people_see_the_ones_that_are_set() {
        let (_dir, database) = testdb::open();
        let table = show_table(
            &database,
            &BookingsShowArgs {
                id: 102,
                account: None,
            },
        )
        .unwrap();

        // Every column of the table, in its order, with the account after
        // its id.
        let stored = schema::find_table(table::BOOKINGS).unwrap();
        let names: Vec<&str> = table.columns().iter().map(|c| c.name()).collect();
        assert_eq!(names.len(), stored.columns.len() + 2);
        assert_eq!(
            names[..4],
            ["account_id", "account_name", "account_iban", "id"]
        );
        let without_account: Vec<&str> = names
            .iter()
            .copied()
            .filter(|name| !["account_name", "account_iban"].contains(name))
            .collect();
        assert_eq!(without_account, stored.column_names().collect::<Vec<_>>());

        let text = rendered_record(&record::of_row(&table, 0, true), Format::Table);
        let expected = "\
Account ID:    1
Account name:  Girokonto
Account IBAN:  DE31999900011000471100
ID:            102
Booking date:  2024-02-10
Value date:    2024-02-10
Status:        booked
Pending:       false
Amount:        -45.50 EUR
Debit:         true
Currency:      EUR
Payee name:    Stadtwerke Musterstadt
Purpose:       Abschlag 100% Ökostrom
               Vertrag a_b
Category:      Wohnen:Nebenkosten:Strom
Category key:  WOHNEN:NEBENKOSTEN:STROM
Booking text:  Lastschrift
Notes:         geprüft
Counted:       true
Batch index:   0
";
        assert_eq!(lines(&text), lines(expected));

        // A program gets every key, set or not.
        let json = rendered_record(&record::of_row(&table, 0, false), Format::Json);
        let object: serde_json::Value = serde_json::from_str(&json).unwrap();
        let object = object.as_object().unwrap();
        assert_eq!(object.len(), names.len());
        assert_eq!(object["payee_iban"], serde_json::Value::Null);
        assert_eq!(object["purpose"], "Abschlag 100% Ökostrom\nVertrag a_b");
        assert!(json.contains("  \"amount\": -45.50,\n"), "{json}");

        // A booking with nothing but its key and amount.
        let bare = show_table(
            &database,
            &BookingsShowArgs {
                id: 111,
                account: None,
            },
        )
        .unwrap();
        let text = rendered_record(&record::of_row(&bare, 0, true), Format::Table);
        assert_eq!(text.lines().count(), 11, "{text}");
    }
}
