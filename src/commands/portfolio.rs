//! `b4 portfolio list`: the securities positions in the database.

use rust_decimal::Decimal;

use crate::cli::{Format, Globals, PortfolioCommand, PortfolioListArgs};
use crate::commands::account_ref;
use crate::db::read::{Database, Hint, Param};
use crate::db::schema::table;
use crate::error::Context;
use crate::output::{self, Cell, Kind, Table};
use crate::Error;

/// The columns of `portfolio list` in the machine formats (json, jsonl, csv),
/// in this order. `account_name` is the name of the account; `name` is the
/// position's own name or else the name of the instrument with its ISIN; the
/// others are columns of the `portfolio_items` table.
const LIST_COLUMNS: [&str; 17] = [
    "account_id",
    "id",
    "account_name",
    "isin",
    "name",
    "quantity",
    "quantity_type",
    "quantity_currency",
    "price_type",
    "current_price",
    "current_price_currency",
    "current_price_date",
    "purchase_price",
    "purchase_price_currency",
    "purchase_price_date",
    "state",
    "deleted",
];

/// Decimals a quantity can have in the database.
const MAX_QUANTITY_SCALE: u32 = 10;

/// Run a verb of `b4 portfolio`.
pub fn run(command: &PortfolioCommand, globals: &Globals) -> Result<(), Error> {
    match command {
        PortfolioCommand::List(args) => list(args, globals),
    }
}

/// `b4 portfolio list [--account <ACCOUNT>]`
///
/// Prints one row per position of the securities accounts, ordered by account
/// (as Banking4 orders the accounts), name and id. With `--account`, only the
/// positions of that account, resolved by the rule in the module
/// documentation of `commands`. Every stored position is listed, also one
/// whose state says it was deleted.
///
/// Only what the vault stores is shown: the quantity, the latest price with
/// its day and the purchase price with its day. No market value and no gain
/// is computed, because that takes rules the vault does not hold (a price
/// quoted in percent of a face amount, a currency other than the account's).
///
/// # Output
///
/// * `table`, for people: account, ISIN, name (cut to fit the terminal),
///   quantity with as many decimals as the listed positions need, the two
///   prices with their currency and day, and the state.
/// * `json`, `jsonl`, `csv`, for programs: the columns of [`LIST_COLUMNS`],
///   quantities and prices exact.
fn list(args: &PortfolioListArgs, globals: &Globals) -> Result<(), Error> {
    let database = Database::open(&globals.db)?;
    let table = list_table(&database, args, globals.format() == Format::Table)?;
    output::print_table(&table, globals)
}

/// The result of `portfolio list`: the columns for people with `for_people`,
/// else the columns for programs.
fn list_table(
    database: &Database,
    args: &PortfolioListArgs,
    for_people: bool,
) -> Result<Table, Error> {
    let account = args
        .account
        .as_deref()
        .map(|reference| account_ref::resolve(database, reference))
        .transpose()?;
    let (filter, params) = match &account {
        Some(account) => (" where p.account_id = ?", vec![Param::from(account.id)]),
        None => ("", Vec::new()),
    };
    let name = "coalesce(p.name, i.name)";

    let (columns, hints) = if for_people {
        let scale = quantity_scale(database, filter, &params)?;
        (
            format!(
                "coalesce(a.name, cast(p.account_id as varchar)) as account, p.isin, \
                 {name} as name, p.quantity, p.current_price, p.current_price_currency, \
                 p.current_price_date, p.purchase_price, p.purchase_price_currency, \
                 p.purchase_price_date, p.state"
            ),
            vec![
                Hint::new("account").title("Account"),
                Hint::new("isin").title("ISIN"),
                Hint::new("name").title("Name").flexible(),
                Hint::new("quantity")
                    .title("Quantity")
                    .kind(Kind::Decimal { scale }),
                Hint::new("current_price")
                    .title("Price")
                    .money("current_price_currency"),
                Hint::new("current_price_date").title("Price date"),
                Hint::new("purchase_price")
                    .title("Purchase price")
                    .money("purchase_price_currency"),
                Hint::new("purchase_price_date").title("Purchase date"),
                Hint::new("state").title("State"),
            ],
        )
    } else {
        let columns: Vec<String> = LIST_COLUMNS
            .iter()
            .map(|column| match *column {
                "account_name" => "a.name as account_name".to_owned(),
                "name" => format!("{name} as name"),
                column => format!("p.{column}"),
            })
            .collect();
        (columns.join(", "), Vec::new())
    };

    // An account id or an ISIN that its table holds twice must not double
    // the positions.
    let sql = format!(
        "select {columns} from {items} p \
         left join (select * from {accounts} \
                    qualify row_number() over (partition by id order by rowid) = 1) a \
           on a.id = p.account_id \
         left join (select * from {instruments} where isin is not null \
                    qualify row_number() over (partition by isin order by rowid) = 1) i \
           on i.isin = p.isin{filter} \
         order by a.position nulls last, p.account_id, lower({name}) nulls last, p.id, p.rowid",
        items = table::PORTFOLIO_ITEMS,
        accounts = table::ACCOUNTS,
        instruments = table::STOCK_INSTRUMENTS,
    );
    database
        .query_with(&sql, &params, &hints)
        .context("cannot list the securities positions")
}

/// The decimals the quantity column needs to show every selected quantity
/// exactly: none for whole numbers of shares, more for fractions.
fn quantity_scale(database: &Database, filter: &str, params: &[Param]) -> Result<u32, Error> {
    let sql = format!(
        "select p.quantity from {} p{filter}",
        table::PORTFOLIO_ITEMS
    );
    let quantities = database
        .query(&sql, params)
        .context("cannot read the quantities of the securities positions")?;
    let scale = quantities
        .rows()
        .iter()
        .filter_map(|row| match row.first() {
            Some(Cell::Decimal(quantity)) => Some(decimals_of(*quantity)),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    Ok(scale.min(MAX_QUANTITY_SCALE))
}

/// The number of decimals of `value` that are not trailing zeros.
fn decimals_of(value: Decimal) -> u32 {
    value.normalize().scale()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testdb::{self, lines, rendered};
    use crate::db::schema;

    fn listed(database: &Database, account: Option<&str>, for_people: bool) -> Table {
        let args = PortfolioListArgs {
            account: account.map(str::to_owned),
        };
        list_table(database, &args, for_people).unwrap()
    }

    #[test]
    fn the_list_for_people_shows_quantity_prices_and_their_days() {
        let (_dir, database) = testdb::open();
        let table = listed(&database, None, true);
        let expected = "\
ACCOUNT  ISIN          NAME                  QUANTITY       PRICE  PRICE DATE  PURCHASE PRICE  PURCHASE DATE  STATE
Depot    ZZ000DEMO048  Demo Anleihe 2030       5000.0  98.125      2024-03-28                                 bank
Depot    ZZ000DEMO030  Demo Welt Aktien ETF      35.5   85.79 EUR  2024-03-28       92.18 EUR  2023-01-20     bank
Depot
";
        assert_eq!(lines(&rendered(&table, Format::Table)), lines(expected));
    }

    #[test]
    fn the_quantity_gets_the_decimals_the_positions_need() {
        assert_eq!(decimals_of("35.5000000000".parse().unwrap()), 1);
        assert_eq!(decimals_of("5000.0000000000".parse().unwrap()), 0);
        assert_eq!(decimals_of("0.0000000001".parse().unwrap()), 10);
        assert_eq!(decimals_of(Decimal::ZERO), 0);

        // Whole numbers only: no decimals at all.
        let (_dir, database) = testdb::open_with(
            "delete from portfolio_items where id <> 602;
             insert into portfolio_items (account_id, id, isin, name, quantity) values
               (4, 610, 'ZZ000DEMO055', 'Demo Aktie', 12);",
        );
        let text = rendered(&listed(&database, None, true), Format::Table);
        assert!(text.contains("  Demo Aktie               12\n"), "{text}");
        assert!(
            text.contains("  Demo Anleihe 2030      5000  98.125 "),
            "{text}"
        );

        // A fraction of a coin.
        let (_dir, database) = testdb::open_with(
            "insert into portfolio_items (account_id, id, name, quantity) values
               (4, 611, 'Demo Coin', 0.00012345);",
        );
        let text = rendered(&listed(&database, None, true), Format::Table);
        assert!(
            text.contains("  Demo Coin                0.00012345\n"),
            "{text}"
        );
        assert!(text.contains("  35.50000000  "), "{text}");
    }

    #[test]
    fn the_machine_formats_carry_the_documented_columns_exactly() {
        let (_dir, database) = testdb::open();
        let table = listed(&database, None, false);
        let columns: Vec<&str> = table.columns().iter().map(|c| c.name()).collect();
        assert_eq!(columns, LIST_COLUMNS);
        let stored = schema::find_table(table::PORTFOLIO_ITEMS).unwrap();
        for name in LIST_COLUMNS {
            assert!(
                name == "account_name" || stored.column_names().any(|column| column == name),
                "{name}"
            );
        }
        assert_eq!(
            rendered(&table, Format::Csv),
            "account_id,id,account_name,isin,name,quantity,quantity_type,quantity_currency,\
             price_type,current_price,current_price_currency,current_price_date,\
             purchase_price,purchase_price_currency,purchase_price_date,state,deleted\n\
             4,602,Depot,ZZ000DEMO048,Demo Anleihe 2030,5000,face_amount,,percent,98.125,,\
             2024-03-28,,,,bank,false\n\
             4,601,Depot,ZZ000DEMO030,Demo Welt Aktien ETF,35.5,units,,actual,85.79,EUR,\
             2024-03-28,92.18,EUR,2023-01-20,bank,false\n\
             4,603,Depot,,,,,,,,,,,,,,\n"
        );
    }

    #[test]
    fn a_position_without_a_name_takes_the_name_of_its_instrument() {
        let (_dir, database) = testdb::open();
        let jsonl = rendered(&listed(&database, None, false), Format::Jsonl);
        // 602 has no name of its own; 601 keeps its own.
        assert!(
            jsonl.contains("\"id\":602,\"account_name\":\"Depot\",\"isin\":\"ZZ000DEMO048\",\"name\":\"Demo Anleihe 2030\","),
            "{jsonl}"
        );
        assert!(
            jsonl.contains("\"name\":\"Demo Welt Aktien ETF\","),
            "{jsonl}"
        );

        // An instrument stored twice does not double the position.
        let (_dir, database) = testdb::open_with(
            "insert into stock_instruments (id, isin, name) values
               (3, 'ZZ000DEMO048', 'Demo Anleihe 2030 (Kopie)');",
        );
        assert_eq!(listed(&database, None, false).len(), 3);
    }

    #[test]
    fn an_account_narrows_the_list() {
        let (_dir, database) = testdb::open();
        assert_eq!(listed(&database, Some("depot"), false).len(), 3);
        let none = listed(&database, Some("giro"), false);
        assert!(none.is_empty());
        assert_eq!(rendered(&none, Format::Json), "[]\n");
        assert_eq!(
            rendered(&listed(&database, Some("giro"), true), Format::Table),
            ""
        );

        let args = PortfolioListArgs {
            account: Some("nope".to_owned()),
        };
        assert!(list_table(&database, &args, true)
            .unwrap_err()
            .to_string()
            .starts_with("no account matches `nope`"));
    }
}
