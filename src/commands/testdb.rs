//! The database the unit tests of the query commands run on.
//!
//! It is created from the real schema definition and filled by SQL, so the
//! tests break when a column a command builds on is renamed. What is in it:
//!
//! * four accounts: `Girokonto` (EUR), `Kreditkarte` (USD, no IBAN), `Altes
//!   Sparkonto` (EUR, hidden) and `Depot` (securities, nothing else set);
//! * bookings in two currencies, booked and pending, with and without a
//!   booking date, a split (master 103 with parts 104 and 105) and a batch
//!   (master 106 with parts 107 and 108), categories on three levels, one
//!   booking with nothing but its key and amount (111), and the id 100 in
//!   two accounts;
//! * standing orders and securities positions, one of each with every
//!   nullable column empty.

use duckdb::Connection;
use tempfile::TempDir;

use crate::cli::Format;
use crate::db::read::Database;
use crate::db::{meta_key, schema, SCHEMA_VERSION};
use crate::output::{self, Options, Record, Table};

/// Id of the account `Girokonto`.
pub(crate) const GIRO: i64 = 1;
/// Id of the account `Kreditkarte`.
pub(crate) const CARD: i64 = 2;
/// Id of the hidden account `Altes Sparkonto`.
pub(crate) const OLD: i64 = 3;
/// Id of the account `Depot`.
pub(crate) const DEPOT: i64 = 4;

const ROWS: &str = "
insert into accounts
  (id, kind, name, iban, bic, bank_code, account_number, currency, position, balance,
   balance_at, hidden, holder_name)
values
  (1, 'bank', 'Girokonto', 'DE31999900011000471100', 'MUBADEM0XXX', '99990001',
   '1000471100', 'EUR', 0, 1423.76, timestamp '2024-03-25 00:00:00', false, 'Erika Mustermann'),
  (2, 'credit_card', 'Kreditkarte', null, null, '99000000', '4711', 'USD', 1, -20.00,
   timestamp '2024-03-30 00:00:00', false, null),
  (3, 'bank', 'Altes Sparkonto', 'DE84999900033000471144', 'SPMUDEF0XXX', '99990003',
   '3000471144', 'EUR', 2, 500.00, timestamp '2024-01-01 00:00:00', true, null);
insert into accounts (id, kind, name, position) values (4, 'portfolio', 'Depot', 3);

insert into categories (id, full_name, category_key, name, parent, depth) values
  (1, 'Wohnen', 'WOHNEN', 'Wohnen', null, 0),
  (2, 'Wohnen:Miete', 'WOHNEN:MIETE', 'Miete', 'Wohnen', 1),
  (3, 'Wohnen:Nebenkosten', 'WOHNEN:NEBENKOSTEN', 'Nebenkosten', 'Wohnen', 1),
  (4, 'Wohnen:Nebenkosten:Strom', 'WOHNEN:NEBENKOSTEN:STROM', 'Strom', 'Wohnen:Nebenkosten', 2);

insert into bookings
  (account_id, id, booking_date, value_date, status, pending, amount, debit, currency,
   payee_name, payee_iban, purpose, category, category_key, booking_text, notes, counted,
   split_role, master_id, batch_index)
values
  (1, 100, date '2024-01-15', date '2024-01-15', 'booked', false, 3000.00, false, 'EUR',
   'Beispielwerke AG', 'DE46999900040004007919', 'Lohn/Gehalt 01/2024', 'Einkommen:Gehalt',
   'EINKOMMEN:GEHALT', 'Gutschrift', null, true, null, null, 0),
  (1, 101, date '2024-01-31', date '2024-01-31', 'booked', false, -850.00, true, 'EUR',
   'Hausverwaltung Muster', 'DE02999900050001234567', 'Miete Januar', 'Wohnen:Miete',
   'WOHNEN:MIETE', 'Dauerauftrag', null, true, null, null, 0),
  (1, 102, date '2024-02-10', date '2024-02-10', 'booked', false, -45.50, true, 'EUR',
   'Stadtwerke Musterstadt', null, 'Abschlag 100% Ökostrom' || chr(10) || 'Vertrag a_b',
   'Wohnen:Nebenkosten:Strom', 'WOHNEN:NEBENKOSTEN:STROM', 'Lastschrift', 'geprüft', true,
   null, null, 0),
  (1, 103, date '2024-02-14', date '2024-02-14', 'booked', false, -84.37, true, 'EUR',
   'Drogerie Beispiel', null, 'Kartenzahlung', null, null, null, null, false,
   'split_master', 103, 0),
  (1, 104, date '2024-02-14', date '2024-02-14', 'booked', false, -52.17, true, 'EUR',
   'Drogerie Beispiel', null, 'Kartenzahlung', 'Haushalt', 'HAUSHALT', null, null, true,
   'split_slave', 103, 1),
  (1, 105, date '2024-02-14', date '2024-02-14', 'booked', false, -32.20, true, 'EUR',
   'Drogerie Beispiel', null, 'Kartenzahlung', 'Gesundheit', 'GESUNDHEIT', null, null, true,
   'split_slave', 103, 2),
  (1, 106, date '2024-03-18', date '2024-03-18', 'booked', false, -244.88, true, 'EUR',
   'Sammelüberweisung', null, 'Sammelüberweisung 2 Posten', null, null, null, null, false,
   'batch_master', 106, 0),
  (1, 107, date '2024-03-18', date '2024-03-18', 'booked', false, -144.88, true, 'EUR',
   'Schornsteinfeger Beispiel', null, 'Rechnung 2024/118', 'Wohnen:Nebenkosten',
   'WOHNEN:NEBENKOSTEN', null, null, true, 'batch_slave', 106, 1),
  (1, 108, date '2024-03-18', date '2024-03-18', 'booked', false, -100.00, true, 'EUR',
   'Sportverein Beispiel', null, 'Beitrag', null, null, null, null, true, 'batch_slave',
   106, 2),
  (1, 109, null, date '2024-04-01', 'pending', true, -67.12, true, 'EUR',
   'Supermarkt Beispiel', null, 'Kartenzahlung', null, null, null, null, true, null, null, 0),
  (1, 110, date '2024-03-20', date '2024-03-20', 'booked', false, -12.00, true, 'EUR',
   'Verkehrsbetriebe', null, 'Monatskarte', 'Mobilität:ÖPNV', 'MOBILITÄT:ÖPNV', null, null,
   true, null, null, 0),
  (2, 100, date '2024-02-01', date '2024-02-02', 'booked', false, -49.00, true, 'USD',
   'Example Software Inc.', null, 'Subscription', 'Sonstiges', 'SONSTIGES', null, null, true,
   null, null, 0),
  (2, 201, date '2024-03-30', null, 'pending', true, -20.00, true, 'USD',
   'Cafe Example', null, null, null, null, null, null, true, null, null, 0),
  (2, 202, date '2024-02-20', date '2024-02-20', 'booked', false, 49.00, false, 'USD',
   'Example Software Inc.', null, 'Refund', 'Sonstiges', 'SONSTIGES', null, null, true,
   null, null, 0),
  (3, 300, date '2024-01-01', date '2024-01-01', 'booked', false, 500.00, false, 'EUR',
   'Übertrag', null, 'Anfangsbestand', '(nicht auswerten)', '(NICHT AUSWERTEN)', null, null,
   true, null, null, 0);
-- Nothing but the key, the amount and what the writer always sets.
insert into bookings (account_id, id, booking_date, status, pending, amount, debit, currency,
                      counted)
values (1, 111, date '2024-03-25', 'booked', false, 0.00, true, 'EUR', true);

insert into standing_orders
  (account_id, id, scheme, amount, debit, currency, payee_name, payee_iban, purpose, category,
   state, is_local, first_execution_date, next_execution_date, stored_next_execution_date,
   period_unit, period_length, execution_day)
values
  (1, 501, 'sepa_credit_transfer', -850.00, true, 'EUR', 'Hausverwaltung Muster',
   'DE02999900050001234567', 'Miete' || chr(10) || 'Wohnung 3', 'Wohnen:Miete', 'at_bank',
   false, date '2020-01-01', null, date '2024-04-01', 'month', 1, 1),
  (1, 502, 'sepa_credit_transfer', -30.00, true, 'EUR', 'Sportverein Beispiel', null,
   'Beitrag', null, 'local', true, date '2023-01-15', date '2024-04-15', date '2024-04-15',
   'month', 3, 15),
  (3, 503, 'sepa_credit_transfer', -10.00, true, 'EUR', 'Zeitung', null, 'Abo', null,
   'at_bank', false, null, null, null, 'week', 2, 1);
insert into standing_orders (account_id, id) values (1, 504);

insert into stock_instruments (id, isin, name, price_currency) values
  (1, 'ZZ000DEMO030', 'Demo Welt Aktien ETF (Stammdaten)', 'EUR'),
  (2, 'ZZ000DEMO048', 'Demo Anleihe 2030', 'EUR');
insert into portfolio_items
  (account_id, id, state, deleted, isin, name, quantity, quantity_type, price_type,
   current_price, current_price_currency, current_price_date, purchase_price,
   purchase_price_currency, purchase_price_date)
values
  (4, 601, 'bank', false, 'ZZ000DEMO030', 'Demo Welt Aktien ETF', 35.5, 'units', 'actual',
   85.79, 'EUR', date '2024-03-28', 92.18, 'EUR', date '2023-01-20'),
  (4, 602, 'bank', false, 'ZZ000DEMO048', null, 5000, 'face_amount', 'percent',
   98.125, null, date '2024-03-28', null, null, null);
insert into portfolio_items (account_id, id) values (4, 603);
";

/// The test database, open the way every query command opens it. Keep the
/// directory alive as long as the database is used.
pub(crate) fn open() -> (TempDir, Database) {
    open_with("")
}

/// [`open`] with the statements of `more_sql` run after the rows above.
pub(crate) fn open_with(more_sql: &str) -> (TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b4.duckdb");
    let connection = Connection::open(&path).unwrap();
    schema::create(&connection).unwrap();
    connection
        .execute_batch(&format!(
            "insert into meta values ('{}', '{SCHEMA_VERSION}');",
            meta_key::SCHEMA_VERSION
        ))
        .unwrap();
    connection.execute_batch(ROWS).unwrap();
    connection.execute_batch(more_sql).unwrap();
    drop(connection);
    let database = Database::open(&path).unwrap();
    (dir, database)
}

/// `table` as `format` writes it to a file.
pub(crate) fn rendered(table: &Table, format: Format) -> String {
    let options = Options {
        format,
        header: true,
        width: None,
    };
    let mut out = Vec::new();
    output::render_table(&mut out, table, &options).unwrap();
    String::from_utf8(out).unwrap()
}

/// `record` as `format` writes it.
pub(crate) fn rendered_record(record: &Record, format: Format) -> String {
    let options = Options {
        format,
        header: true,
        width: None,
    };
    let mut out = Vec::new();
    output::render_record_on(&mut out, record, &options, false).unwrap();
    String::from_utf8(out).unwrap()
}

/// The lines of `text`, for comparing a rendered table line by line.
pub(crate) fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}
