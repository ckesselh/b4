//! `b4 accounts list|show`: the accounts in the database.

use crate::cli::{AccountsCommand, AccountsListArgs, AccountsShowArgs, Format, Globals};
use crate::commands::account_ref;
use crate::commands::record;
use crate::db::read::{quote_identifier, Database, Hint, Param};
use crate::db::schema::{self, table};
use crate::error::Context;
use crate::output::{self, Table};
use crate::Error;

/// The columns of `accounts list` in the machine formats (json, jsonl, csv),
/// in this order. The first eleven are columns of the `accounts` table; the
/// last three are computed from the bookings that count.
const LIST_COLUMNS: [&str; 14] = [
    "id",
    "kind",
    "name",
    "iban",
    "account_number",
    "bic",
    "bank_code",
    "currency",
    "balance",
    "balance_at",
    "hidden",
    "booking_count",
    "first_booking_date",
    "last_booking_date",
];

/// The amounts of an account, all in its `currency`.
const MONEY_COLUMNS: [(&str, &str); 7] = [
    ("balance", "currency"),
    ("online_balance", "currency"),
    ("available_funds", "currency"),
    ("overdraft_limit", "currency"),
    ("credits", "currency"),
    ("debits", "currency"),
    ("net", "currency"),
];

/// What `accounts show` adds to the stored columns, computed from the
/// bookings of the account whose `counted` column is true.
const SHOW_STATISTICS: [&str; 7] = [
    "booking_count",
    "pending_count",
    "first_booking_date",
    "last_booking_date",
    "credits",
    "debits",
    "net",
];

/// Run a verb of `b4 accounts`.
pub fn run(command: &AccountsCommand, globals: &Globals) -> Result<(), Error> {
    match command {
        AccountsCommand::List(args) => list(args, globals),
        AccountsCommand::Show(args) => show(args, globals),
    }
}

/// `b4 accounts list [--type <TYPE>]... [--all]`
///
/// Prints one row per account in the order Banking4 shows the accounts
/// (position, then id). Accounts hidden in Banking4 are left out unless
/// `--all` is given; `--type` keeps only accounts of these types, several
/// types being a union.
///
/// The number of bookings and the first and last booking date count the
/// bookings whose `counted` column is true (the master row of a split or a
/// batch stands for its parts), pending ones included.
///
/// # Output
///
/// * `table`, for people: id, type, name, the IBAN or else the account
///   number, the bank (BIC or else bank code; the vault's name for the bank
///   belongs to the access data and is never read), the balance with its
///   currency, the day of the balance and the number of bookings. With
///   `--all` a column says which accounts are hidden.
/// * `json`, `jsonl`, `csv`, for programs: the columns of [`LIST_COLUMNS`].
fn list(args: &AccountsListArgs, globals: &Globals) -> Result<(), Error> {
    let database = Database::open(&globals.db)?;
    let table = list_table(&database, args, globals.format() == Format::Table)?;
    output::print_table(&table, globals)
}

/// The result of `accounts list`: the columns for people with `for_people`,
/// else the columns for programs.
fn list_table(
    database: &Database,
    args: &AccountsListArgs,
    for_people: bool,
) -> Result<Table, Error> {
    let mut conditions: Vec<String> = Vec::new();
    let mut params: Vec<Param> = Vec::new();
    if !args.all {
        conditions.push("not coalesce(a.hidden, false)".to_owned());
    }
    if !args.types.is_empty() {
        let placeholders = vec!["?"; args.types.len()].join(", ");
        conditions.push(format!("a.kind in ({placeholders})"));
        params.extend(args.types.iter().map(|kind| Param::from(kind.kind_name())));
    }
    let filter = if conditions.is_empty() {
        String::new()
    } else {
        format!(" where {}", conditions.join(" and "))
    };

    let (columns, hints) = if for_people {
        let mut columns = "a.id, a.kind, a.name, \
                           coalesce(a.iban, a.account_number) as iban_or_number, \
                           coalesce(a.bic, a.bank_code) as bank, \
                           a.balance, a.currency, cast(a.balance_at as date) as balance_date, \
                           coalesce(s.booking_count, 0) as booking_count"
            .to_owned();
        if args.all {
            columns.push_str(", case when a.hidden then 'hidden' end as hidden");
        }
        let hints = vec![
            Hint::new("id").title("ID"),
            Hint::new("kind").title("Type"),
            Hint::new("name").title("Name"),
            Hint::new("iban_or_number").title("IBAN / number"),
            Hint::new("bank").title("Bank"),
            Hint::new("balance").title("Balance").money("currency"),
            Hint::new("balance_date").title("As of"),
            Hint::new("booking_count").title("Bookings"),
            Hint::new("hidden").title("Hidden"),
        ];
        (columns, hints)
    } else {
        let columns: Vec<String> = LIST_COLUMNS
            .iter()
            .map(|name| match *name {
                "booking_count" => "coalesce(s.booking_count, 0) as booking_count".to_owned(),
                "first_booking_date" | "last_booking_date" => format!("s.{name}"),
                name => format!("a.{name}"),
            })
            .collect();
        (
            columns.join(", "),
            vec![Hint::new("balance").money("currency")],
        )
    };

    let sql = format!(
        "select {columns} from {accounts} a \
         left join (select account_id, count(*) as booking_count, \
                      min(booking_date) as first_booking_date, \
                      max(booking_date) as last_booking_date \
                    from {bookings} where counted group by account_id) s \
           on s.account_id = a.id{filter} \
         order by a.position nulls last, a.id, a.rowid",
        accounts = table::ACCOUNTS,
        bookings = table::BOOKINGS,
    );
    database
        .query_with(&sql, &params, &hints)
        .context("cannot list the accounts")
}

/// `b4 accounts show <ACCOUNT>`
///
/// Prints one record with every stored column of the account `args.account`
/// names (resolved by the rule in the module documentation of `commands`; a
/// hidden account can be shown), followed by what its bookings say:
///
/// * `booking_count`: the bookings that count (`counted` is true), pending
///   ones included; `pending_count`: how many of them are pending;
/// * `first_booking_date`, `last_booking_date`;
/// * `credits`, `debits` (negative) and `net`: the sums of those bookings in
///   the account's currency, the same numbers `bookings summary --by account`
///   prints. They are empty for an account without bookings, and in the
///   case that should not occur, bookings of one account in several
///   currencies: such amounts are never added.
///
/// Table output shows the fields that have a value; the machine formats have
/// every field.
fn show(args: &AccountsShowArgs, globals: &Globals) -> Result<(), Error> {
    let database = Database::open(&globals.db)?;
    let table = show_table(&database, &args.account)?;
    let for_people = globals.format() == Format::Table;
    output::print_record(&record::of_row(&table, 0, for_people), globals)
}

/// The one row `accounts show` prints for the account `reference` names.
fn show_table(database: &Database, reference: &str) -> Result<Table, Error> {
    let account = account_ref::resolve(database, reference)?;

    let stored = schema::find_table(table::ACCOUNTS)
        .ok_or_else(|| Error::new("the schema has no table of accounts"))?;
    let names: Vec<&str> = stored.column_names().chain(SHOW_STATISTICS).collect();
    let columns: Vec<String> = stored
        .column_names()
        .map(|name| format!("a.{}", quote_identifier(name)))
        .collect();

    // Sums only where there is something to add, and never over several
    // currencies.
    let sum = |name: &str| {
        format!(
            "case when s.account_id is not null and s.currencies <= 1 \
             then cast(coalesce(s.{name}, 0) as decimal(38,4)) end as {name}"
        )
    };
    // The statistics are selected in the order of SHOW_STATISTICS.
    let sql = format!(
        "select {columns}, \
           coalesce(s.booking_count, 0) as booking_count, \
           coalesce(s.pending_count, 0) as pending_count, \
           s.first_booking_date, s.last_booking_date, \
           {credits}, {debits}, {net} \
         from {accounts} a \
         left join (select account_id, count(*) as booking_count, \
                      count(*) filter (where pending) as pending_count, \
                      min(booking_date) as first_booking_date, \
                      max(booking_date) as last_booking_date, \
                      count(distinct currency) as currencies, \
                      sum(amount) filter (where not debit) as credits, \
                      sum(amount) filter (where debit) as debits, \
                      sum(amount) as net \
                    from {bookings} where counted group by account_id) s \
           on s.account_id = a.id \
         where a.id = ? order by a.rowid limit 1",
        columns = columns.join(", "),
        credits = sum("credits"),
        debits = sum("debits"),
        net = sum("net"),
        accounts = table::ACCOUNTS,
        bookings = table::BOOKINGS,
    );
    let hints = record::hints(names.iter().copied(), &MONEY_COLUMNS);
    let found = database
        .query_with(&sql, &[Param::from(account.id)], &hints)
        .context(format!("cannot read the account {}", account.id))?;
    if found.is_empty() {
        // Resolved a moment ago: only if the database changed under us.
        return Err(Error::new(format!(
            "the account {} is not there",
            account.id
        )));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::AccountType;
    use crate::commands::testdb::{self, lines, rendered, rendered_record};
    use crate::output::Cell;

    fn listed(database: &Database, types: &[AccountType], all: bool) -> Table {
        let args = AccountsListArgs {
            types: types.to_vec(),
            all,
        };
        list_table(database, &args, false).unwrap()
    }

    fn names(table: &Table) -> Vec<String> {
        let index = table
            .columns()
            .iter()
            .position(|column| column.name() == "name")
            .unwrap();
        table
            .rows()
            .iter()
            .map(|row| match &row[index] {
                Cell::Text(name) => name.clone(),
                other => panic!("unexpected name {other:?}"),
            })
            .collect()
    }

    #[test]
    fn the_list_for_people_leaves_hidden_accounts_out() {
        let (_dir, database) = testdb::open();
        let args = AccountsListArgs {
            types: Vec::new(),
            all: false,
        };
        let table = list_table(&database, &args, true).unwrap();
        let expected = "\
ID  TYPE         NAME         IBAN / NUMBER           BANK             BALANCE  AS OF       BOOKINGS
 1  bank         Girokonto    DE31999900011000471100  MUBADEM0XXX  1423.76 EUR  2024-03-25        10
 2  credit_card  Kreditkarte  4711                    99000000      -20.00 USD  2024-03-30         3
 4  portfolio    Depot                                                                             0
";
        assert_eq!(lines(&rendered(&table, Format::Table)), lines(expected));
    }

    #[test]
    fn all_includes_hidden_accounts_and_marks_them() {
        let (_dir, database) = testdb::open();
        let args = AccountsListArgs {
            types: Vec::new(),
            all: true,
        };
        let table = list_table(&database, &args, true).unwrap();
        let expected = "\
ID  TYPE         NAME             IBAN / NUMBER           BANK             BALANCE  AS OF       BOOKINGS  HIDDEN
 1  bank         Girokonto        DE31999900011000471100  MUBADEM0XXX  1423.76 EUR  2024-03-25        10
 2  credit_card  Kreditkarte      4711                    99000000      -20.00 USD  2024-03-30         3
 3  bank         Altes Sparkonto  DE84999900033000471144  SPMUDEF0XXX   500.00 EUR  2024-01-01         1  hidden
 4  portfolio    Depot                                                                                 0
";
        assert_eq!(lines(&rendered(&table, Format::Table)), lines(expected));
    }

    #[test]
    fn the_machine_formats_carry_the_documented_columns() {
        let (_dir, database) = testdb::open();
        let table = listed(&database, &[], true);
        let columns: Vec<&str> = table.columns().iter().map(|c| c.name()).collect();
        assert_eq!(columns, LIST_COLUMNS);
        let stored = schema::find_table(table::ACCOUNTS).unwrap();
        for name in &LIST_COLUMNS[..11] {
            assert!(
                stored.column_names().any(|column| column == *name),
                "{name}"
            );
        }

        let jsonl = rendered(&table, Format::Jsonl);
        let rows = lines(&jsonl);
        assert_eq!(rows.len(), 4);
        // The masters of the split and of the batch are not counted: 10 of
        // the 12 rows, the pending one among them.
        assert_eq!(
            rows[0],
            "{\"id\":1,\"kind\":\"bank\",\"name\":\"Girokonto\",\
             \"iban\":\"DE31999900011000471100\",\"account_number\":\"1000471100\",\
             \"bic\":\"MUBADEM0XXX\",\"bank_code\":\"99990001\",\"currency\":\"EUR\",\
             \"balance\":1423.76,\"balance_at\":\"2024-03-25T00:00:00\",\"hidden\":false,\
             \"booking_count\":10,\"first_booking_date\":\"2024-01-15\",\
             \"last_booking_date\":\"2024-03-25\"}"
        );
        // An account with nothing but a name: nulls, and no bookings.
        assert_eq!(
            rows[3],
            "{\"id\":4,\"kind\":\"portfolio\",\"name\":\"Depot\",\"iban\":null,\
             \"account_number\":null,\"bic\":null,\"bank_code\":null,\"currency\":null,\
             \"balance\":null,\"balance_at\":null,\"hidden\":null,\"booking_count\":0,\
             \"first_booking_date\":null,\"last_booking_date\":null}"
        );
    }

    #[test]
    fn types_are_a_union_and_combine_with_all() {
        let (_dir, database) = testdb::open();
        assert_eq!(
            names(&listed(&database, &[], false)),
            ["Girokonto", "Kreditkarte", "Depot"]
        );
        assert_eq!(
            names(&listed(&database, &[AccountType::Bank], false)),
            ["Girokonto"]
        );
        assert_eq!(
            names(&listed(&database, &[AccountType::Bank], true)),
            ["Girokonto", "Altes Sparkonto"]
        );
        assert_eq!(
            names(&listed(
                &database,
                &[AccountType::Portfolio, AccountType::CreditCard],
                false
            )),
            ["Kreditkarte", "Depot"]
        );
        let none = listed(&database, &[AccountType::Paypal], true);
        assert!(none.is_empty());
        assert_eq!(rendered(&none, Format::Json), "[]\n");
        assert_eq!(rendered(&none, Format::Table), "");
    }

    #[test]
    fn accounts_come_in_the_order_of_banking4() {
        let (_dir, database) = testdb::open_with(
            "insert into accounts (id, kind, name, position, hidden) values
               (9, 'cash', 'Vorne', -1, false), (8, 'cash', 'Ohne Position', null, false),
               (7, 'cash', 'Gleiche Position', 0, false);",
        );
        assert_eq!(
            names(&listed(&database, &[], false)),
            [
                "Vorne",
                "Girokonto",
                "Gleiche Position",
                "Kreditkarte",
                "Depot",
                "Ohne Position"
            ]
        );
    }

    #[test]
    fn show_prints_the_stored_columns_and_the_statistics() {
        let (_dir, database) = testdb::open();
        let table = show_table(&database, "giro").unwrap();
        let text = rendered_record(&record::of_row(&table, 0, true), Format::Table);
        let expected = "\
ID:                  1
Kind:                bank
Name:                Girokonto
IBAN:                DE31999900011000471100
BIC:                 MUBADEM0XXX
Bank code:           99990001
Account number:      1000471100
Currency:            EUR
Holder name:         Erika Mustermann
Position:            0
Balance:             1423.76 EUR
Balance at:          2024-03-25 00:00:00
Hidden:              false
Booking count:       10
Pending count:       1
First booking date:  2024-01-15
Last booking date:   2024-03-25
Credits:             3000.00 EUR
Debits:              -1303.87 EUR
Net:                 1696.13 EUR
";
        assert_eq!(lines(&text), lines(expected));

        // Every column of the table, in its order, then the statistics.
        let stored = schema::find_table(table::ACCOUNTS).unwrap();
        let columns: Vec<&str> = table.columns().iter().map(|c| c.name()).collect();
        let expected: Vec<&str> = stored.column_names().chain(SHOW_STATISTICS).collect();
        assert_eq!(columns, expected);
    }

    #[test]
    fn show_finds_a_hidden_account_and_one_in_another_currency() {
        let (_dir, database) = testdb::open();
        let hidden = show_table(&database, "DE84 9999 0003 3000 4711 44").unwrap();
        let text = rendered_record(&record::of_row(&hidden, 0, true), Format::Table);
        assert!(
            text.contains("Name:                Altes Sparkonto\n"),
            "{text}"
        );
        assert!(text.contains("Hidden:              true\n"), "{text}");
        assert!(text.contains("Net:                 500.00 EUR\n"), "{text}");

        let card = show_table(&database, "4711").unwrap();
        let text = rendered_record(&record::of_row(&card, 0, true), Format::Table);
        assert!(text.contains("Debits:              -69.00 USD\n"), "{text}");
        assert!(text.contains("Pending count:       1\n"), "{text}");
        assert!(!text.contains("IBAN"), "{text}");
    }

    #[test]
    fn show_copes_with_an_account_that_has_nothing_set() {
        let (_dir, database) = testdb::open();
        let table = show_table(&database, "depot").unwrap();
        let text = rendered_record(&record::of_row(&table, 0, true), Format::Table);
        assert_eq!(
            text,
            "ID:             4\nKind:           portfolio\nName:           Depot\n\
             Position:       3\nBooking count:  0\nPending count:  0\n"
        );
        // A program gets every key, the sums as null: there is nothing to add.
        let json = rendered_record(&record::of_row(&table, 0, false), Format::Json);
        let object: serde_json::Value = serde_json::from_str(&json).unwrap();
        let object = object.as_object().unwrap();
        assert_eq!(object.len(), table.columns().len());
        assert_eq!(object["booking_count"], 0);
        for key in [
            "iban",
            "balance",
            "first_booking_date",
            "credits",
            "debits",
            "net",
        ] {
            assert_eq!(object[key], serde_json::Value::Null, "{key}");
        }
    }

    #[test]
    fn show_never_adds_different_currencies() {
        let (_dir, database) = testdb::open_with(
            "insert into bookings (account_id, id, booking_date, status, pending, amount, debit,
                                   currency, counted)
             values (3, 301, date '2024-02-02', 'booked', false, -5.00, true, 'CHF', true);",
        );
        let table = show_table(&database, "3").unwrap();
        let json = rendered_record(&record::of_row(&table, 0, false), Format::Json);
        let object: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(object["booking_count"], 2);
        assert_eq!(object["last_booking_date"], "2024-02-02");
        for key in ["credits", "debits", "net"] {
            assert_eq!(object[key], serde_json::Value::Null, "{key}");
        }
    }

    #[test]
    fn show_fails_for_an_unknown_or_ambiguous_account() {
        let (_dir, database) = testdb::open();
        assert!(show_table(&database, "nope")
            .unwrap_err()
            .to_string()
            .starts_with("no account matches `nope`"));
        assert!(show_table(&database, "konto")
            .unwrap_err()
            .to_string()
            .starts_with("`konto` matches several accounts:"));
    }
}
