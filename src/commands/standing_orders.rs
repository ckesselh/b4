//! `b4 standing-orders list`: the standing orders in the database.

use crate::cli::{Format, Globals, StandingOrdersCommand, StandingOrdersListArgs};
use crate::commands::account_ref;
use crate::db::read::{Database, Hint, Param};
use crate::db::schema::table;
use crate::error::Context;
use crate::output::{self, Table};
use crate::Error;

/// The columns of `standing-orders list` in the machine formats (json, jsonl,
/// csv), in this order. `account_name` is the name of the account; the others
/// are columns of the `standing_orders` table.
const LIST_COLUMNS: [&str; 21] = [
    "account_id",
    "id",
    "account_name",
    "scheme",
    "amount",
    "currency",
    "debit",
    "payee_name",
    "payee_iban",
    "payee_bic",
    "purpose",
    "category",
    "state",
    "is_local",
    "period_unit",
    "period_length",
    "execution_day",
    "first_execution_date",
    "last_execution_date",
    "next_execution_date",
    "stored_next_execution_date",
];

/// The interval of a standing order in words, from its unit and length.
const SCHEDULE: &str = "\
    case when s.period_unit = 'month' and s.period_length = 1 then 'monthly' \
         when s.period_unit = 'month' and s.period_length = 3 then 'quarterly' \
         when s.period_unit = 'month' and s.period_length = 6 then 'half-yearly' \
         when s.period_unit = 'month' and s.period_length = 12 then 'yearly' \
         when s.period_unit = 'week' and s.period_length = 1 then 'weekly' \
         when s.period_length is null then s.period_unit \
         else 'every ' || s.period_length || ' ' || coalesce(s.period_unit, 'period') || 's' \
    end";

/// Run a verb of `b4 standing-orders`.
pub fn run(command: &StandingOrdersCommand, globals: &Globals) -> Result<(), Error> {
    match command {
        StandingOrdersCommand::List(args) => list(args, globals),
    }
}

/// `b4 standing-orders list [--account <ACCOUNT>]`
///
/// Prints one row per standing order, ordered by account (as Banking4 orders
/// the accounts) and id. With `--account`, only those of that account,
/// resolved by the rule in the module documentation of `commands`. Every
/// stored order is listed, also one whose state says it was deleted.
///
/// # Output
///
/// * `table`, for people: id, account, payee, purpose (cut to fit the
///   terminal), the signed amount with its currency, the interval in words,
///   the day it runs on, the next execution day and the state. The next day
///   is only known for orders kept in Banking4: for an order held at the
///   bank Banking4 computes it and b4 does not, so the cell is empty.
/// * `json`, `jsonl`, `csv`, for programs: the columns of [`LIST_COLUMNS`].
fn list(args: &StandingOrdersListArgs, globals: &Globals) -> Result<(), Error> {
    let database = Database::open(&globals.db)?;
    let table = list_table(&database, args, globals.format() == Format::Table)?;
    output::print_table(&table, globals)
}

/// The result of `standing-orders list`: the columns for people with
/// `for_people`, else the columns for programs.
fn list_table(
    database: &Database,
    args: &StandingOrdersListArgs,
    for_people: bool,
) -> Result<Table, Error> {
    let account = args
        .account
        .as_deref()
        .map(|reference| account_ref::resolve(database, reference))
        .transpose()?;
    let (filter, params) = match &account {
        Some(account) => (" where s.account_id = ?", vec![Param::from(account.id)]),
        None => ("", Vec::new()),
    };

    let (columns, hints) = if for_people {
        (
            format!(
                "s.id, coalesce(a.name, cast(s.account_id as varchar)) as account, \
                 s.payee_name, s.purpose, s.amount, s.currency, {SCHEDULE} as schedule, \
                 s.execution_day, s.next_execution_date, s.state"
            ),
            vec![
                Hint::new("id").title("ID"),
                Hint::new("account").title("Account"),
                Hint::new("payee_name").title("Payee").flexible(),
                Hint::new("purpose").title("Purpose").flexible(),
                Hint::new("amount").title("Amount").money("currency"),
                Hint::new("schedule").title("Every"),
                Hint::new("execution_day").title("Day"),
                Hint::new("next_execution_date").title("Next"),
                Hint::new("state").title("State"),
            ],
        )
    } else {
        let columns: Vec<String> = LIST_COLUMNS
            .iter()
            .map(|name| match *name {
                "account_name" => "a.name as account_name".to_owned(),
                name => format!("s.{name}"),
            })
            .collect();
        (
            columns.join(", "),
            vec![Hint::new("amount").money("currency")],
        )
    };

    // An account id that the table holds twice must not double the orders.
    let sql = format!(
        "select {columns} from {orders} s \
         left join (select * from {accounts} \
                    qualify row_number() over (partition by id order by rowid) = 1) a \
           on a.id = s.account_id{filter} \
         order by a.position nulls last, s.account_id, s.id, s.rowid",
        orders = table::STANDING_ORDERS,
        accounts = table::ACCOUNTS,
    );
    database
        .query_with(&sql, &params, &hints)
        .context("cannot list the standing orders")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testdb::{self, lines, rendered};
    use crate::db::schema;

    fn listed(database: &Database, account: Option<&str>, for_people: bool) -> Table {
        let args = StandingOrdersListArgs {
            account: account.map(str::to_owned),
        };
        list_table(database, &args, for_people).unwrap()
    }

    #[test]
    fn the_list_for_people_says_the_interval_in_words() {
        let (_dir, database) = testdb::open();
        let table = listed(&database, None, true);
        let expected = r" ID  ACCOUNT          PAYEE                  PURPOSE                 AMOUNT  EVERY          DAY  NEXT        STATE
501  Girokonto        Hausverwaltung Muster  Miete ↵ Wohnung 3  -850.00 EUR  monthly          1              at_bank
502  Girokonto        Sportverein Beispiel   Beitrag             -30.00 EUR  quarterly       15  2024-04-15  local
504  Girokonto
503  Altes Sparkonto  Zeitung                Abo                 -10.00 EUR  every 2 weeks    1              at_bank
";
        assert_eq!(lines(&rendered(&table, Format::Table)), lines(expected));
    }

    #[test]
    fn every_interval_has_a_wording() {
        let (_dir, database) = testdb::open_with(
            "delete from standing_orders;
             insert into standing_orders (account_id, id, period_unit, period_length) values
               (1, 1, 'month', 1), (1, 2, 'month', 2), (1, 3, 'month', 3), (1, 4, 'month', 6),
               (1, 5, 'month', 12), (1, 6, 'week', 1), (1, 7, 'week', 4), (1, 8, 'month', null),
               (1, 9, null, 2), (1, 10, '7', 1);",
        );
        let table = listed(&database, None, true);
        let index = table
            .columns()
            .iter()
            .position(|column| column.name() == "schedule")
            .unwrap();
        let words: Vec<String> = table
            .rows()
            .iter()
            .map(|row| match &row[index] {
                crate::output::Cell::Text(text) => text.clone(),
                other => panic!("unexpected schedule {other:?}"),
            })
            .collect();
        assert_eq!(
            words,
            [
                "monthly",
                "every 2 months",
                "quarterly",
                "half-yearly",
                "yearly",
                "weekly",
                "every 4 weeks",
                "month",
                "every 2 periods",
                "every 1 7s"
            ]
        );
    }

    #[test]
    fn the_machine_formats_carry_the_documented_columns() {
        let (_dir, database) = testdb::open();
        let table = listed(&database, None, false);
        let columns: Vec<&str> = table.columns().iter().map(|c| c.name()).collect();
        assert_eq!(columns, LIST_COLUMNS);
        let stored = schema::find_table(table::STANDING_ORDERS).unwrap();
        for name in LIST_COLUMNS {
            assert!(
                name == "account_name" || stored.column_names().any(|column| column == name),
                "{name}"
            );
        }

        let jsonl = rendered(&table, Format::Jsonl);
        let rows = lines(&jsonl);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows[0],
            "{\"account_id\":1,\"id\":501,\"account_name\":\"Girokonto\",\
             \"scheme\":\"sepa_credit_transfer\",\"amount\":-850.00,\"currency\":\"EUR\",\
             \"debit\":true,\"payee_name\":\"Hausverwaltung Muster\",\
             \"payee_iban\":\"DE02999900050001234567\",\"payee_bic\":null,\
             \"purpose\":\"Miete\\nWohnung 3\",\"category\":\"Wohnen:Miete\",\
             \"state\":\"at_bank\",\"is_local\":false,\"period_unit\":\"month\",\
             \"period_length\":1,\"execution_day\":1,\"first_execution_date\":\"2020-01-01\",\
             \"last_execution_date\":null,\"next_execution_date\":null,\
             \"stored_next_execution_date\":\"2024-04-01\"}"
        );
        // An order with nothing set.
        assert!(
            rows[2].starts_with(
                "{\"account_id\":1,\"id\":504,\"account_name\":\"Girokonto\",\"scheme\":null,\
                 \"amount\":null,\"currency\":null,"
            ),
            "{}",
            rows[2]
        );
    }

    #[test]
    fn an_account_narrows_the_list() {
        let (_dir, database) = testdb::open();
        assert_eq!(listed(&database, Some("giro"), false).len(), 3);
        // A hidden account can be named.
        assert_eq!(listed(&database, Some("altes"), false).len(), 1);
        let none = listed(&database, Some("depot"), false);
        assert!(none.is_empty());
        assert_eq!(rendered(&none, Format::Json), "[]\n");
        assert_eq!(
            rendered(&listed(&database, Some("depot"), true), Format::Table),
            ""
        );

        let args = StandingOrdersListArgs {
            account: Some("konto".to_owned()),
        };
        assert!(list_table(&database, &args, true)
            .unwrap_err()
            .to_string()
            .starts_with("`konto` matches several accounts:"));
    }
}
