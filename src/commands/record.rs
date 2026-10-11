//! Records for the `show` commands: one row of a query as key and value.

use crate::db::read::Hint;
use crate::output::{Cell, Column, Kind, Record, Table};

/// Words of column names that are written in capitals in a title.
const ACRONYMS: [&str; 9] = [
    "iban", "bic", "id", "vat", "gvc", "swift", "sepa", "isin", "url",
];

/// The title a person reads for the column `name`: `payee_iban` becomes
/// `Payee IBAN`.
pub(crate) fn title_of(name: &str) -> String {
    let mut title = String::with_capacity(name.len());
    for (index, word) in name.split('_').enumerate() {
        if index > 0 {
            title.push(' ');
        }
        if ACRONYMS.contains(&word) {
            title.push_str(&word.to_uppercase());
        } else if index == 0 {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                title.extend(first.to_uppercase());
                title.push_str(chars.as_str());
            }
        } else {
            title.push_str(word);
        }
    }
    title
}

/// Hints that give every column of `names` its [`title_of`], and show each
/// `(amount, currency)` pair of `money` as money in that currency.
pub(crate) fn hints<'a>(
    names: impl IntoIterator<Item = &'a str>,
    money: &[(&str, &str)],
) -> Vec<Hint> {
    names
        .into_iter()
        .map(|name| {
            let hint = Hint::new(name).title(title_of(name));
            match money.iter().find(|(amount, _)| *amount == name) {
                Some((_, currency)) => hint.money(*currency),
                None => hint,
            }
        })
        .collect()
}

/// The row `index` of `table` as a record with the table's columns.
///
/// With `only_set`, fields without a value are left out and a decimal number
/// is shown without trailing zeros: that is for people, who do not want to
/// read dozens of empty lines. The machine formats keep every field, so a key
/// is always there.
pub(crate) fn of_row(table: &Table, index: usize, only_set: bool) -> Record {
    let mut record = Record::new();
    let Some(row) = table.rows().get(index) else {
        return record;
    };
    for (column, cell) in table.columns().iter().zip(row) {
        if only_set && *cell == Cell::Null {
            continue;
        }
        // A rate or a price is stored with ten decimals: a person reads
        // `9.75`, not `9.7500000000`.
        let column = match (only_set, column.kind(), cell) {
            (true, Kind::Decimal { .. }, Cell::Decimal(value)) => {
                Column::decimal(column.name(), column.title(), value.normalize().scale())
            }
            _ => column.clone(),
        };
        record.push(column, cell.clone());
    }
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_made_from_column_names() {
        assert_eq!(title_of("name"), "Name");
        assert_eq!(title_of("booking_date"), "Booking date");
        assert_eq!(title_of("payee_iban"), "Payee IBAN");
        assert_eq!(title_of("id"), "ID");
        assert_eq!(title_of("account_id"), "Account ID");
        assert_eq!(title_of("vat_amount"), "VAT amount");
        assert_eq!(title_of("end_to_end_id"), "End to end ID");
        assert_eq!(title_of(""), "");
    }

    #[test]
    fn a_row_becomes_a_record_with_or_without_its_empty_fields() {
        let mut table = Table::new(vec![
            Column::integer("id", "ID"),
            Column::text("notes", "Notes"),
            Column::text("name", "Name"),
        ]);
        table.push(vec![Cell::Integer(7), Cell::Null, Cell::from("x")]);

        let all = of_row(&table, 0, false);
        let keys: Vec<&str> = all.fields().map(|(column, _)| column.name()).collect();
        assert_eq!(keys, ["id", "notes", "name"]);

        let set = of_row(&table, 0, true);
        let keys: Vec<&str> = set.fields().map(|(column, _)| column.name()).collect();
        assert_eq!(keys, ["id", "name"]);

        assert_eq!(of_row(&table, 1, false).fields().count(), 0);
    }

    #[test]
    fn people_read_a_decimal_without_trailing_zeros() {
        let mut table = Table::new(vec![
            Column::decimal("rate", "Rate", 10),
            Column::decimal("none", "None", 10),
            Column::money("balance", "Balance", "currency"),
        ]);
        table.push(vec![
            Cell::Decimal("9.7500000000".parse().unwrap()),
            Cell::Decimal("0.0000000000".parse().unwrap()),
            Cell::Decimal("5.5000".parse().unwrap()),
        ]);
        let options = crate::output::Options {
            format: crate::cli::Format::Table,
            header: true,
            width: None,
        };
        let mut out = Vec::new();
        crate::output::render_record_on(&mut out, &of_row(&table, 0, true), &options, false)
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Rate:     9.75\nNone:     0\nBalance:  5.50\n"
        );
        // The columns of a program are left as they are.
        let exact = of_row(&table, 0, false);
        let kinds: Vec<&Kind> = exact.fields().map(|(column, _)| column.kind()).collect();
        assert_eq!(kinds[0], &Kind::Decimal { scale: 10 });
    }
}
