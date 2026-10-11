//! The filters of `bookings list` and `bookings summary` as SQL.
//!
//! One builder for both commands, so a filter means the same in a list and
//! in the sums. It turns [`BookingFilters`] into conditions on the columns of
//! the bookings view and the values to bind: no text of the user ever becomes
//! part of the statement.
//!
//! Text is matched with `contains(lower(column), lower(?))`, a plain
//! substring test. There is no pattern language in it, so `%` and `_` in the
//! user's text stand for themselves, and a column that is NULL never matches.

use crate::cli::BookingFilters;
use crate::db::read::Param;

/// The columns `--filter` searches, as plain substrings.
const FILTER_COLUMNS: [&str; 6] = [
    "payee_name",
    "purpose",
    "booking_text",
    "category",
    "notes",
    "payee_iban",
];

/// Conditions that select bookings, and the values of their placeholders.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Selection {
    conditions: Vec<String>,
    params: Vec<Param>,
}

impl Selection {
    /// The selection `filters` asks for. `account_ids` are the accounts its
    /// `--account` references resolved to; none means every account.
    ///
    /// * accounts: a union;
    /// * `--from` / `--to`: on the booking date, both inclusive, so a booking
    ///   without a booking date passes neither;
    /// * `--filter`: the text in the payee name, the purpose, the booking
    ///   text, the category, the notes or the payee's IBAN. The purpose is
    ///   also searched with its line breaks taken out, because banks break
    ///   lines in the middle of a word, and the IBAN also with the blanks of
    ///   the text taken out, so an IBAN can be pasted in groups of four;
    /// * `--payee`: the text in the payee name;
    /// * `--category`: the text in the full category name. That name holds
    ///   every level (`Wohnen:Nebenkosten:Strom`), so the name of a parent
    ///   selects its whole subtree;
    /// * `--uncategorized`: no category;
    /// * `--min` / `--max`: on the absolute amount, both inclusive;
    /// * `--debit` / `--credit`: the stored direction (an amount of zero is
    ///   a debit);
    /// * `--pending` / `--booked`: by status; a notice that moves no money
    ///   counts as pending, as in the `pending` column.
    pub(crate) fn new(filters: &BookingFilters, account_ids: &[i64]) -> Selection {
        let mut selection = Selection::default();

        if !account_ids.is_empty() {
            let placeholders = vec!["?"; account_ids.len()].join(", ");
            selection
                .conditions
                .push(format!("account_id in ({placeholders})"));
            selection
                .params
                .extend(account_ids.iter().map(|id| Param::from(*id)));
        }
        if let Some(day) = filters.first_day() {
            selection.push("booking_date >= ?", Param::from(day));
        }
        if let Some(day) = filters.last_day() {
            selection.push("booking_date <= ?", Param::from(day));
        }
        if let Some(text) = &filters.filter {
            let mut alternatives: Vec<String> = FILTER_COLUMNS
                .iter()
                .map(|column| contains(column))
                .collect();
            selection
                .params
                .extend(FILTER_COLUMNS.iter().map(|_| Param::from(text)));

            alternatives.push(contains("replace(purpose, chr(10), '')"));
            selection.params.push(Param::from(text));

            let without_blanks: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            if !without_blanks.is_empty() && &without_blanks != text {
                alternatives.push(contains("payee_iban"));
                selection.params.push(Param::from(without_blanks));
            }
            selection
                .conditions
                .push(format!("({})", alternatives.join(" or ")));
        }
        if let Some(text) = &filters.payee {
            selection.push(&contains("payee_name"), Param::from(text));
        }
        if let Some(text) = &filters.category {
            selection.push(&contains("category"), Param::from(text));
        }
        if filters.uncategorized {
            selection.require("category is null");
        }
        if let Some(amount) = filters.min {
            selection.push("abs(amount) >= ?", Param::from(amount));
        }
        if let Some(amount) = filters.max {
            selection.push("abs(amount) <= ?", Param::from(amount));
        }
        if filters.debit {
            selection.require("debit");
        }
        if filters.credit {
            selection.require("not debit");
        }
        if filters.pending {
            selection.require("pending");
        }
        if filters.booked {
            selection.require("not pending");
        }
        selection
    }

    /// Add a condition without a placeholder.
    pub(crate) fn require(&mut self, condition: &str) {
        self.conditions.push(condition.to_owned());
    }

    /// ` where a and b`, or nothing when every booking is selected.
    pub(crate) fn where_clause(&self) -> String {
        if self.conditions.is_empty() {
            String::new()
        } else {
            format!(" where {}", self.conditions.join(" and "))
        }
    }

    /// The values of the placeholders, in the order of the conditions.
    pub(crate) fn params(&self) -> &[Param] {
        &self.params
    }

    /// Add a condition with one placeholder and its value.
    fn push(&mut self, condition: &str, param: Param) {
        self.conditions.push(condition.to_owned());
        self.params.push(param);
    }
}

/// The condition that `expression` contains the bound text, ignoring case.
fn contains(expression: &str) -> String {
    format!("contains(lower({expression}), lower(?))")
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;
    use crate::commands::testdb::{self, CARD, GIRO, OLD};
    use crate::db::read::Database;
    use crate::db::schema::view;
    use crate::output::{Cell, Date};

    fn amount(text: &str) -> Option<Decimal> {
        Some(text.parse().unwrap())
    }

    fn text(text: &str) -> Option<String> {
        Some(text.to_owned())
    }

    /// The `(account, id)` of the bookings `filters` selects.
    fn selected(
        database: &Database,
        filters: &BookingFilters,
        accounts: &[i64],
    ) -> Vec<(i64, i64)> {
        let selection = Selection::new(filters, accounts);
        let sql = format!(
            "select account_id, id from {}{} order by account_id, id",
            view::BOOKINGS,
            selection.where_clause()
        );
        database
            .query(&sql, selection.params())
            .unwrap()
            .rows()
            .iter()
            .map(|row| match row.as_slice() {
                [Cell::Integer(account), Cell::Integer(id)] => (*account, *id),
                other => panic!("unexpected row {other:?}"),
            })
            .collect()
    }

    /// The ids of the bookings of the Girokonto `filters` selects.
    fn giro(database: &Database, filters: &BookingFilters) -> Vec<i64> {
        selected(database, filters, &[GIRO])
            .into_iter()
            .map(|(_, id)| id)
            .collect()
    }

    #[test]
    fn no_filter_selects_everything_and_needs_no_where() {
        let selection = Selection::new(&BookingFilters::default(), &[]);
        assert_eq!(selection.where_clause(), "");
        assert!(selection.params().is_empty());

        let (_dir, database) = testdb::open();
        assert_eq!(
            selected(&database, &BookingFilters::default(), &[]).len(),
            16
        );
    }

    #[test]
    fn the_statement_holds_placeholders_and_never_the_text() {
        let filters = BookingFilters {
            from: Some("2024-02".parse().unwrap()),
            to: Some("2024".parse().unwrap()),
            filter: text("x' or '1'='1"),
            payee: text("%"),
            category: text("Wohnen"),
            min: amount("10"),
            max: amount("99.5"),
            debit: true,
            booked: true,
            ..BookingFilters::default()
        };
        let selection = Selection::new(&filters, &[7, 9]);
        assert_eq!(
            selection.where_clause(),
            " where account_id in (?, ?) and booking_date >= ? and booking_date <= ? and \
             (contains(lower(payee_name), lower(?)) or contains(lower(purpose), lower(?)) or \
             contains(lower(booking_text), lower(?)) or contains(lower(category), lower(?)) or \
             contains(lower(notes), lower(?)) or contains(lower(payee_iban), lower(?)) or \
             contains(lower(replace(purpose, chr(10), '')), lower(?)) or \
             contains(lower(payee_iban), lower(?))) and \
             contains(lower(payee_name), lower(?)) and contains(lower(category), lower(?)) and \
             abs(amount) >= ? and abs(amount) <= ? and debit and not pending"
        );
        let hostile = Param::from("x' or '1'='1");
        let mut expected = vec![
            Param::from(7_i64),
            Param::from(9_i64),
            Param::from(Date::new(2024, 2, 1).unwrap()),
            Param::from(Date::new(2024, 12, 31).unwrap()),
        ];
        expected.extend(std::iter::repeat_n(hostile, 7));
        expected.extend([
            Param::from("x'or'1'='1"),
            Param::from("%"),
            Param::from("Wohnen"),
            Param::from(Decimal::new(10, 0)),
            Param::from(Decimal::new(995, 1)),
        ]);
        assert_eq!(selection.params(), expected);
        // As many placeholders as values.
        assert_eq!(
            selection.where_clause().matches('?').count(),
            selection.params().len()
        );

        let other = BookingFilters {
            uncategorized: true,
            credit: true,
            pending: true,
            ..BookingFilters::default()
        };
        assert_eq!(
            Selection::new(&other, &[]).where_clause(),
            " where category is null and not debit and pending"
        );
    }

    #[test]
    fn accounts_are_a_union() {
        let (_dir, database) = testdb::open();
        let all = BookingFilters::default();
        assert_eq!(selected(&database, &all, &[OLD]), [(OLD, 300)]);
        assert_eq!(
            selected(&database, &all, &[OLD, CARD]),
            [(CARD, 100), (CARD, 201), (CARD, 202), (OLD, 300)]
        );
        assert_eq!(selected(&database, &all, &[GIRO]).len(), 12);
        // An account without bookings selects nothing.
        assert!(selected(&database, &all, &[4]).is_empty());
    }

    #[test]
    fn dates_are_inclusive_at_day_month_and_year_granularity() {
        let (_dir, database) = testdb::open();
        let range = |from: Option<&str>, to: Option<&str>| {
            let filters = BookingFilters {
                from: from.map(|text| text.parse().unwrap()),
                to: to.map(|text| text.parse().unwrap()),
                ..BookingFilters::default()
            };
            giro(&database, &filters)
        };
        // A day at both ends.
        assert_eq!(
            range(Some("2024-01-31"), Some("2024-02-14")),
            [101, 102, 103, 104, 105]
        );
        assert_eq!(range(Some("2024-01-15"), Some("2024-01-15")), [100]);
        // A month: its first day as --from, its last as --to.
        assert_eq!(
            range(Some("2024-02"), Some("2024-02")),
            [102, 103, 104, 105]
        );
        assert_eq!(range(None, Some("2024-01")), [100, 101]);
        assert_eq!(range(Some("2024-03"), None), [106, 107, 108, 110, 111]);
        // A year.
        assert_eq!(range(Some("2024"), Some("2024")).len(), 11);
        assert!(range(Some("2025"), None).is_empty());
        assert!(range(None, Some("2023")).is_empty());
        // The pending booking 109 has no booking date: no range holds it.
        assert!(!range(Some("2000"), None).contains(&109));
        assert!(!range(None, Some("2999")).contains(&109));
    }

    #[test]
    fn filter_searches_every_text_field_ignoring_case() {
        let (_dir, database) = testdb::open();
        let search = |needle: &str| {
            let filters = BookingFilters {
                filter: text(needle),
                ..BookingFilters::default()
            };
            selected(&database, &filters, &[])
        };
        // Payee name, in any case.
        assert_eq!(search("STADTWERKE"), [(GIRO, 102)]);
        // Purpose.
        assert_eq!(search("gehalt 01"), [(GIRO, 100)]);
        // Booking text.
        assert_eq!(search("dauerauftrag"), [(GIRO, 101)]);
        // Category.
        assert_eq!(search("einkommen"), [(GIRO, 100)]);
        // Notes, with a letter outside ASCII.
        assert_eq!(search("GEPRÜFT"), [(GIRO, 102)]);
        // Payee IBAN, also pasted in groups.
        assert_eq!(search("de0299990005"), [(GIRO, 101)]);
        assert_eq!(search("DE02 9999 0005 0001 2345 67"), [(GIRO, 101)]);
        // Several fields of several bookings.
        assert_eq!(
            search("miete"),
            [(GIRO, 101)],
            "payee `Hausverwaltung`, purpose and category of one booking"
        );
        assert_eq!(search("example"), [(CARD, 100), (CARD, 201), (CARD, 202)]);
        // A purpose line that the bank broke: found with the break taken out.
        assert_eq!(search("ökostromvertrag"), [(GIRO, 102)]);
        assert!(search("no such text").is_empty());
    }

    #[test]
    fn wildcards_and_quotes_in_the_text_are_literal() {
        let (_dir, database) = testdb::open();
        let search = |needle: &str| {
            let filters = BookingFilters {
                filter: text(needle),
                ..BookingFilters::default()
            };
            selected(&database, &filters, &[])
        };
        assert_eq!(search("100%"), [(GIRO, 102)]);
        assert_eq!(search("a_b"), [(GIRO, 102)]);
        // As LIKE patterns these would match every booking with a text.
        assert!(search("%").len() == 1);
        assert!(search("_").len() == 1);
        assert!(search("a%b").is_empty());
        assert!(search("abschlag_100").is_empty());
        assert!(search("x' or '1'='1").is_empty());
        assert!(search("\\").is_empty());

        let by_payee = |needle: &str| {
            let filters = BookingFilters {
                payee: text(needle),
                ..BookingFilters::default()
            };
            selected(&database, &filters, &[])
        };
        assert!(by_payee("%").is_empty());
        assert!(by_payee("_").is_empty());
        assert!(by_payee("'; drop table bookings; --").is_empty());
    }

    #[test]
    fn payee_matches_the_payee_name_only() {
        let (_dir, database) = testdb::open();
        let by_payee = |needle: &str| {
            let filters = BookingFilters {
                payee: text(needle),
                ..BookingFilters::default()
            };
            selected(&database, &filters, &[])
        };
        assert_eq!(
            by_payee("drogerie"),
            [(GIRO, 103), (GIRO, 104), (GIRO, 105)]
        );
        assert_eq!(by_payee("SOFTWARE inc"), [(CARD, 100), (CARD, 202)]);
        assert_eq!(by_payee("übertrag"), [(OLD, 300)]);
        // `Miete` is a purpose and a category, but no payee.
        assert!(by_payee("miete").is_empty());
    }

    #[test]
    fn category_matches_the_category_and_everything_below_it() {
        let (_dir, database) = testdb::open();
        let in_category = |needle: &str| {
            let filters = BookingFilters {
                category: text(needle),
                ..BookingFilters::default()
            };
            selected(&database, &filters, &[])
        };
        // A top level: its whole subtree.
        assert_eq!(
            in_category("Wohnen"),
            [(GIRO, 101), (GIRO, 102), (GIRO, 107)]
        );
        assert_eq!(in_category("wohnen"), in_category("WOHNEN"));
        // A middle level: itself and its children.
        assert_eq!(in_category("Nebenkosten"), [(GIRO, 102), (GIRO, 107)]);
        assert_eq!(
            in_category("Wohnen:Nebenkosten"),
            [(GIRO, 102), (GIRO, 107)]
        );
        // A leaf, by its own name or by its full name.
        assert_eq!(in_category("Miete"), [(GIRO, 101)]);
        assert_eq!(in_category("wohnen:miete"), [(GIRO, 101)]);
        assert_eq!(in_category("strom"), [(GIRO, 102)]);
        // Letters outside ASCII fold too.
        assert_eq!(in_category("öpnv"), [(GIRO, 110)]);
        assert_eq!(in_category("mobilität"), [(GIRO, 110)]);
        // Another branch is not touched, and no category never matches.
        assert_eq!(in_category("Haushalt"), [(GIRO, 104)]);
        assert_eq!(in_category("(nicht"), [(OLD, 300)]);
        assert!(in_category("Urlaub").is_empty());
    }

    #[test]
    fn uncategorized_selects_bookings_without_a_category() {
        let (_dir, database) = testdb::open();
        let filters = BookingFilters {
            uncategorized: true,
            ..BookingFilters::default()
        };
        assert_eq!(
            selected(&database, &filters, &[]),
            [
                (GIRO, 103),
                (GIRO, 106),
                (GIRO, 108),
                (GIRO, 109),
                (GIRO, 111),
                (CARD, 201)
            ]
        );
    }

    #[test]
    fn min_and_max_compare_the_absolute_amount() {
        let (_dir, database) = testdb::open();
        let between = |min: Option<&str>, max: Option<&str>| {
            let filters = BookingFilters {
                min: min.and_then(amount),
                max: max.and_then(amount),
                ..BookingFilters::default()
            };
            selected(&database, &filters, &[])
        };
        // Debits and credits alike, both ends inclusive.
        assert_eq!(
            between(Some("500"), None),
            [(GIRO, 100), (GIRO, 101), (OLD, 300)]
        );
        assert_eq!(between(Some("850"), Some("850")), [(GIRO, 101)]);
        assert_eq!(
            between(Some("49"), Some("52.17")),
            [(GIRO, 104), (CARD, 100), (CARD, 202)]
        );
        assert_eq!(between(None, Some("12")), [(GIRO, 110), (GIRO, 111)]);
        assert_eq!(between(None, Some("0")), [(GIRO, 111)]);
        assert_eq!(between(Some("45.5"), Some("45.50")), [(GIRO, 102)]);
        assert!(between(Some("3000.01"), None).is_empty());
    }

    #[test]
    fn direction_and_status() {
        let (_dir, database) = testdb::open();
        let credit = BookingFilters {
            credit: true,
            ..BookingFilters::default()
        };
        assert_eq!(
            selected(&database, &credit, &[]),
            [(GIRO, 100), (CARD, 202), (OLD, 300)]
        );
        let debit = BookingFilters {
            debit: true,
            ..BookingFilters::default()
        };
        assert_eq!(selected(&database, &debit, &[]).len(), 13);
        assert_eq!(
            selected(&database, &debit, &[CARD]),
            [(CARD, 100), (CARD, 201)]
        );

        let pending = BookingFilters {
            pending: true,
            ..BookingFilters::default()
        };
        assert_eq!(
            selected(&database, &pending, &[]),
            [(GIRO, 109), (CARD, 201)]
        );
        let booked = BookingFilters {
            booked: true,
            ..BookingFilters::default()
        };
        assert_eq!(selected(&database, &booked, &[]).len(), 14);
    }

    #[test]
    fn filters_combine_with_and() {
        let (_dir, database) = testdb::open();
        let filters = BookingFilters {
            from: Some("2024-02".parse().unwrap()),
            to: Some("2024-03".parse().unwrap()),
            category: text("wohnen"),
            min: amount("100"),
            debit: true,
            booked: true,
            ..BookingFilters::default()
        };
        assert_eq!(selected(&database, &filters, &[GIRO, CARD]), [(GIRO, 107)]);

        let filters = BookingFilters {
            filter: text("example"),
            payee: text("software"),
            max: amount("49"),
            credit: true,
            ..BookingFilters::default()
        };
        assert_eq!(selected(&database, &filters, &[]), [(CARD, 202)]);
        assert!(selected(&database, &filters, &[GIRO]).is_empty());

        let filters = BookingFilters {
            uncategorized: true,
            pending: true,
            from: Some("2024-03-30".parse().unwrap()),
            ..BookingFilters::default()
        };
        assert_eq!(selected(&database, &filters, &[]), [(CARD, 201)]);
    }
}
