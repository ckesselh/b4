//! Tables, columns and their descriptions.
//!
//! This is the one definition of the database layout. Everything else is
//! generated from it: the `CREATE TABLE` and `CREATE VIEW` statements, the
//! `COMMENT ON` texts that `b4 schema` reads back, the column order the
//! writer appends rows in, and `docs/schema.md`, which `tests/schema.rs`
//! renders from it. A column therefore cannot exist without a description,
//! and the documentation cannot drift from the database.
//!
//! Conventions, the same in every table:
//!
//! * `id` is the record's object id as stored. Nothing is unique: the format
//!   guarantees no uniqueness, so no table has a key or a constraint.
//! * Money is `DECIMAL(18,4)`; quantities, prices and rates are
//!   `DECIMAL(28,10)`. Sums are exact.
//! * A coded value is its lower-case name in a `VARCHAR`, an unknown code its
//!   decimal number as text. A flag set is the names of the set flags joined
//!   by commas. No `ENUM` types: a value from a newer Banking4 must not make
//!   decrypt fail.
//! * An empty text is NULL.
//! * None of the bank access fields is a column; the record structs do not
//!   even have them.

use std::fmt::Write as _;
use std::sync::OnceLock;

use duckdb::Connection;

use super::{META_TABLE, WARNINGS_TABLE};

/// Names of the tables, for the writer and the query commands.
pub mod table {
    /// Accounts.
    pub const ACCOUNTS: &str = "accounts";
    /// Groups that accounts are sorted into.
    pub const ACCOUNT_GROUPS: &str = "account_groups";
    /// Statement entries.
    pub const BOOKINGS: &str = "bookings";
    /// Payments entered in Banking4.
    pub const PAYMENTS: &str = "payments";
    /// Standing orders.
    pub const STANDING_ORDERS: &str = "standing_orders";
    /// The address book of counterparties.
    pub const PAYEES: &str = "payees";
    /// Counterparties for direct debits.
    pub const PAYERS: &str = "payers";
    /// The category catalogue.
    pub const CATEGORIES: &str = "categories";
    /// Direct debit mandates.
    pub const MANDATES: &str = "mandates";
    /// Exchange rates.
    pub const EXCHANGE_RATES: &str = "exchange_rates";
    /// Securities master data.
    pub const STOCK_INSTRUMENTS: &str = "stock_instruments";
    /// Price history of the securities.
    pub const STOCK_PRICES: &str = "stock_prices";
    /// Securities positions.
    pub const PORTFOLIO_ITEMS: &str = "portfolio_items";
    /// Securities transactions.
    pub const PORTFOLIO_TRANSACTIONS: &str = "portfolio_transactions";
    /// Tax exemption orders.
    pub const TAX_EXEMPTIONS: &str = "tax_exemptions";
    /// Items of asset accounts.
    pub const ASSETS: &str = "assets";
}

/// Names of the views.
pub mod view {
    /// Bookings with their account and category context.
    pub const BOOKINGS: &str = "bookings_view";
    /// Stored balance of each account next to the sum of its bookings.
    pub const ACCOUNT_BALANCES: &str = "account_balances";
    /// Sums per month, category and currency.
    pub const MONTHLY_CATEGORY_SUMS: &str = "monthly_category_sums";
}

/// Digits and decimals of a money column.
pub const MONEY: (u8, u8) = (18, 4);

/// Digits and decimals of a quantity, price or rate column.
pub const DECIMAL: (u8, u8) = (28, 10);

/// What a column holds, which decides its DuckDB type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An object id or a plain whole number: `BIGINT`.
    BigInt,
    /// An amount of money: `DECIMAL(18,4)`.
    Money,
    /// A quantity, price or rate: `DECIMAL(28,10)`.
    Decimal,
    /// A calendar date: `DATE`.
    Date,
    /// A date and time without a zone: `TIMESTAMP`.
    Timestamp,
    /// A time of day: `TIME`.
    Time,
    /// `BOOLEAN`.
    Boolean,
    /// Text, a coded value or a flag set: `VARCHAR`.
    Text,
}

impl Kind {
    /// The DuckDB type of a column of this kind, as `duckdb_columns()` spells
    /// it.
    pub const fn sql_type(self) -> &'static str {
        match self {
            Kind::BigInt => "BIGINT",
            Kind::Money => "DECIMAL(18,4)",
            Kind::Decimal => "DECIMAL(28,10)",
            Kind::Date => "DATE",
            Kind::Timestamp => "TIMESTAMP",
            Kind::Time => "TIME",
            Kind::Boolean => "BOOLEAN",
            Kind::Text => "VARCHAR",
        }
    }
}

/// One column of a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// The column name.
    pub name: &'static str,
    /// What it holds.
    pub kind: Kind,
    /// What it means, stored in the database as the column comment.
    pub description: &'static str,
}

/// One table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The table name.
    pub name: &'static str,
    /// What a row is, stored in the database as the table comment.
    pub description: &'static str,
    /// The columns in table order, which is the order the writer appends in.
    pub columns: Vec<Column>,
}

impl Table {
    /// The names of the columns in table order.
    pub fn column_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.columns.iter().map(|column| column.name)
    }
}

/// One column of a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewColumn {
    /// The column name.
    pub name: &'static str,
    /// The DuckDB type the expression has, as `duckdb_columns()` spells it.
    pub sql_type: &'static str,
    /// The expression the column is computed from.
    pub expression: String,
    /// What it means, stored in the database as the column comment.
    pub description: &'static str,
}

/// One view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// The view name.
    pub name: &'static str,
    /// What a row is, stored in the database as the view comment.
    pub description: &'static str,
    /// The columns in order.
    pub columns: Vec<ViewColumn>,
    /// Everything after the select list: `from ...`, joins, filters, grouping.
    pub source: &'static str,
}

/// Every table of the database, `meta` and `warnings` last.
pub fn tables() -> &'static [Table] {
    static TABLES: OnceLock<Vec<Table>> = OnceLock::new();
    TABLES.get_or_init(build_tables)
}

/// Every view of the database.
pub fn views() -> &'static [View] {
    static VIEWS: OnceLock<Vec<View>> = OnceLock::new();
    VIEWS.get_or_init(build_views)
}

/// The table called `name`.
pub fn find_table(name: &str) -> Option<&'static Table> {
    tables().iter().find(|table| table.name == name)
}

/// The view called `name`.
#[cfg(test)]
pub fn find_view(name: &str) -> Option<&'static View> {
    views().iter().find(|view| view.name == name)
}

/// The statements that create every table and view with its descriptions, in
/// one batch.
pub fn ddl() -> String {
    let mut sql = String::new();
    for table in tables() {
        let columns: Vec<String> = table
            .columns
            .iter()
            .map(|column| format!("{} {}", quote(column.name), column.kind.sql_type()))
            .collect();
        let name = quote(table.name);
        let _ = writeln!(sql, "create table {name} ({});", columns.join(", "));
        let _ = writeln!(
            sql,
            "comment on table {name} is {};",
            literal(table.description)
        );
        for column in &table.columns {
            let _ = writeln!(
                sql,
                "comment on column {name}.{} is {};",
                quote(column.name),
                literal(column.description)
            );
        }
    }
    for view in views() {
        let columns: Vec<String> = view
            .columns
            .iter()
            .map(|column| format!("{} as {}", column.expression, quote(column.name)))
            .collect();
        let name = quote(view.name);
        let _ = writeln!(
            sql,
            "create view {name} as select {} {};",
            columns.join(", "),
            view.source
        );
        let _ = writeln!(
            sql,
            "comment on view {name} is {};",
            literal(view.description)
        );
        for column in &view.columns {
            let _ = writeln!(
                sql,
                "comment on column {name}.{} is {};",
                quote(column.name),
                literal(column.description)
            );
        }
    }
    sql
}

/// Create the empty schema, with all descriptions, in `connection`.
pub fn create(connection: &Connection) -> duckdb::Result<()> {
    connection.execute_batch(&ddl())
}

/// `name` as a quoted SQL identifier.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `text` as an SQL string literal.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

const fn col(name: &'static str, kind: Kind, description: &'static str) -> Column {
    Column {
        name,
        kind,
        description,
    }
}

/// `id`, `created_at`, `updated_at`: what every record has.
fn record_columns(id: &'static str) -> Vec<Column> {
    vec![
        col("id", Kind::BigInt, id),
        col(
            "created_at",
            Kind::Timestamp,
            "When the record was created, in UTC. Derived from id, which is a creation stamp \
             in seconds; approximate when many records were created within one second.",
        ),
        col(
            "updated_at",
            Kind::Timestamp,
            "When the record was last changed, in UTC. NULL when the vault holds no change \
             stamp for it.",
        ),
    ]
}

/// `account_id` followed by the record columns: what every per-account
/// record has.
fn account_record_columns(what: &'static str) -> Vec<Column> {
    let mut columns = vec![col("account_id", Kind::BigInt, what)];
    columns.extend(record_columns(
        "Object id of the record. Unique within one account at most: identify a row by \
         (account_id, id).",
    ));
    columns
}

const ACCOUNT_ID: &str = "The account the record belongs to (accounts.id), taken from the \
                          directory the record is stored in. Normally an account exists for it; \
                          records of a directory without an account are kept with a warning.";

/// Amount, direction and currency of a booking, payment or standing order.
fn amount_columns(with_stored_direction: bool) -> Vec<Column> {
    let mut columns = vec![
        col(
            "amount",
            Kind::Money,
            "Signed amount in `currency`: negative when money leaves the account, positive \
             when it arrives. Rounded to four decimals if the stored value has more.",
        ),
        col(
            "debit",
            Kind::Boolean,
            "Direction: true when money leaves the account (amount is negative or zero), false \
             when it arrives.",
        ),
        col(
            "stored_amount",
            Kind::Decimal,
            "The amount as the vault stores it, without the direction; normally not negative.",
        ),
    ];
    if with_stored_direction {
        columns.push(col(
            "stored_debit_flag",
            Kind::Boolean,
            "The direction bit as stored. `debit` is what counts: for a record with a scheme \
             the scheme decides the direction and this bit can differ.",
        ));
    }
    columns
}

/// The counterparty of a booking, payment or standing order.
fn payee_columns() -> Vec<Column> {
    vec![
        col(
            "payee_name",
            Kind::Text,
            "Name of the other party: the recipient of a debit, the sender of a credit.",
        ),
        col(
            "payee_iban",
            Kind::Text,
            "Account of the other party: an IBAN, or a national account number in old records.",
        ),
        col(
            "payee_bic",
            Kind::Text,
            "Bank of the other party: a BIC, or a national bank code in old records.",
        ),
        col(
            "payee_country",
            Kind::Text,
            "Country code of the other party.",
        ),
        col(
            "payee_street",
            Kind::Text,
            "Street of the other party's address.",
        ),
        col(
            "payee_town",
            Kind::Text,
            "Postal code and town of the other party's address.",
        ),
        col(
            "ultimate_payee_name",
            Kind::Text,
            "Name of the party on whose behalf the other party acts (SEPA ultimate creditor \
             or debtor).",
        ),
    ]
}

/// Purpose and category of a booking, payment or standing order.
fn purpose_and_category_columns() -> Vec<Column> {
    vec![
        col(
            "purpose",
            Kind::Text,
            "Remittance text; several lines are joined with a line feed.",
        ),
        col(
            "category",
            Kind::Text,
            "Category as the record names it: the full name with levels separated by `:`, such \
             as `Wohnen:Miete`. NULL when uncategorised. Need not exist in `categories`.",
        ),
        col(
            "category_key",
            Kind::Text,
            "Matching key of `category`: the name trimmed and upper-cased. Join it to \
             categories.category_key; names that differ only in case are one category.",
        ),
    ]
}

/// The SEPA identifiers of the payment base.
fn sepa_columns() -> Vec<Column> {
    vec![
        col("end_to_end_id", Kind::Text, "SEPA end-to-end reference."),
        col(
            "payment_info_id",
            Kind::Text,
            "SEPA payment information id, shared by the transfers of one batch.",
        ),
        col(
            "mandate_id",
            Kind::Text,
            "Mandate reference of a direct debit.",
        ),
        col(
            "mandate_signed_date",
            Kind::Date,
            "Day the direct debit mandate was signed.",
        ),
        col(
            "creditor_id",
            Kind::Text,
            "SEPA creditor identifier of a direct debit.",
        ),
        col(
            "purpose_code",
            Kind::Text,
            "SEPA purpose code, such as SALA.",
        ),
        col(
            "category_purpose_code",
            Kind::Text,
            "SEPA category purpose code.",
        ),
        col(
            "local_instrument_code",
            Kind::Text,
            "SEPA local instrument code, such as CORE or B2B.",
        ),
        col(
            "sequence_type",
            Kind::Text,
            "SEPA direct debit sequence type, such as FRST or RCUR.",
        ),
        col("order_id", Kind::Text, "Order number the bank assigned."),
    ]
}

/// Details of a foreign payment, rarely set.
fn foreign_payment_columns() -> Vec<Column> {
    vec![
        col(
            "cost_bearer",
            Kind::Text,
            "Who pays the charges of a foreign payment: none, ours, shared or beneficiary.",
        ),
        col(
            "intl_remittance_method",
            Kind::Text,
            "Remittance method of a foreign payment.",
        ),
        col(
            "intl_remittance_instruction",
            Kind::Text,
            "Instruction text of a foreign payment.",
        ),
        col(
            "regulatory_reporting_code",
            Kind::Text,
            "Regulatory reporting code of a foreign payment.",
        ),
        col(
            "payee_bank_name",
            Kind::Text,
            "Name of the other party's bank.",
        ),
        col(
            "payee_bank_street",
            Kind::Text,
            "Street of the other party's bank.",
        ),
        col(
            "payee_bank_town",
            Kind::Text,
            "Town of the other party's bank.",
        ),
    ]
}

/// Scheme columns of payments and standing orders.
fn scheme_columns() -> Vec<Column> {
    vec![
        col(
            "scheme",
            Kind::Text,
            "Kind of payment: none, user_expense, user_income, sepa_credit_transfer, \
             sepa_direct_debit_core, sepa_direct_debit_b2b, sepa_instant_credit_transfer, \
             urgent_credit_transfer, foreign_credit_transfer, swiss_credit_transfer, \
             domestic_legacy_credit_transfer or foreign_legacy_credit_transfer. Derived from \
             the stored class when an old record stores none.",
        ),
        col(
            "stored_scheme",
            Kind::Text,
            "The scheme as stored: none in records of the old layout, where `scheme` is derived.",
        ),
    ]
}

#[allow(clippy::too_many_lines)]
fn build_tables() -> Vec<Table> {
    let mut tables = Vec::new();

    // accounts
    let mut columns = record_columns(
        "Object id of the account. Other tables refer to it as account_id. Not guaranteed \
         unique.",
    );
    columns.extend([
        col(
            "kind",
            Kind::Text,
            "Kind of account: bank, cash, credit_card, portfolio, asset, paypal or crypto.",
        ),
        col(
            "stored_kind",
            Kind::Text,
            "The kind as stored. Differs from `kind` only for a cash account in a crypto \
             currency, which Banking4 treats as crypto.",
        ),
        col("name", Kind::Text, "Name the user gave the account."),
        col(
            "iban",
            Kind::Text,
            "IBAN, without blanks. NULL for accounts that have none.",
        ),
        col("bic", Kind::Text, "BIC of the account's bank."),
        col(
            "bank_code",
            Kind::Text,
            "National bank code. Accounts that are not at a bank carry a pseudo code.",
        ),
        col("account_number", Kind::Text, "National account number."),
        col("country_code", Kind::Text, "Country code of the bank."),
        col(
            "currency",
            Kind::Text,
            "Currency of the account as an ISO code. NULL for accounts without a currency of \
             their own, such as securities accounts.",
        ),
        col("holder_name", Kind::Text, "Name of the account holder."),
        col(
            "holder_street",
            Kind::Text,
            "Street of the holder's address.",
        ),
        col(
            "holder_town",
            Kind::Text,
            "Postal code and town of the holder's address.",
        ),
        col("description", Kind::Text, "Free text the user entered."),
        col(
            "group_id",
            Kind::BigInt,
            "The group the account is sorted into (account_groups.id). NULL when in no group.",
        ),
        col(
            "position",
            Kind::BigInt,
            "Sort position in Banking4: order by position, then id.",
        ),
        col("opening_date", Kind::Date, "Day the account was opened."),
        col(
            "balance",
            Kind::Money,
            "Balance according to the last statement, in `currency`; negative when overdrawn. \
             Zero for kinds without statements (portfolio, asset). As stored: not computed \
             from bookings (see the view account_balances).",
        ),
        col(
            "balance_at",
            Kind::Timestamp,
            "Moment `balance` refers to; zone unknown, often midnight of the statement day. \
             NULL when there is no statement balance.",
        ),
        col(
            "online_balance",
            Kind::Money,
            "Balance the bank reported in the last online query, which can include entries \
             that are not booked yet.",
        ),
        col(
            "online_balance_at",
            Kind::Timestamp,
            "Moment `online_balance` was reported; zone unknown. NULL when never queried.",
        ),
        col(
            "available_funds",
            Kind::Money,
            "Amount available according to the bank or computed by Banking4.",
        ),
        col("overdraft_limit", Kind::Money, "Overdraft limit."),
        col(
            "credit_interest_rate",
            Kind::Decimal,
            "Interest rate on a credit balance, in percent.",
        ),
        col(
            "debit_interest_rate",
            Kind::Decimal,
            "Interest rate on an overdraft within the limit, in percent.",
        ),
        col(
            "overdraft_interest_rate",
            Kind::Decimal,
            "Interest rate beyond the overdraft limit, in percent.",
        ),
        col(
            "last_download_at",
            Kind::Timestamp,
            "When statements were last fetched; zone unknown. NULL when never.",
        ),
        col(
            "last_sync_at",
            Kind::Timestamp,
            "When the account was last synchronised; zone unknown. NULL when never.",
        ),
        col(
            "flags",
            Kind::Text,
            "Settings of the account as a comma-joined list of flag names, such as \
             `hidden,exclude_from_totals`. NULL when none is set.",
        ),
        col(
            "hidden",
            Kind::Boolean,
            "True when the user hid the account in Banking4.",
        ),
        col(
            "exclude_from_totals",
            Kind::Boolean,
            "True when the account is left out of Banking4's total of all balances.",
        ),
        col(
            "balance_flags",
            Kind::Text,
            "Where limit and available funds come from and what is displayed, as flag names.",
        ),
        col(
            "creditor_id",
            Kind::Text,
            "SEPA creditor identifier used for direct debits from this account.",
        ),
        col(
            "reference_account_iban",
            Kind::Text,
            "IBAN of the reference account, for savings and call money accounts.",
        ),
        col(
            "originator_name",
            Kind::Text,
            "Name used as originator in payments from this account.",
        ),
        col(
            "icon_name",
            Kind::Text,
            "Name of the icon chosen for the account.",
        ),
        col(
            "record_version",
            Kind::BigInt,
            "Layout version of the stored record; tells which generation of Banking4 wrote it.",
        ),
    ]);
    tables.push(Table {
        name: table::ACCOUNTS,
        description: "One row per account of the vault: bank accounts, cash, credit cards, \
                      securities accounts and others, including hidden ones (see `hidden`). \
                      Bank access data of an account is never exported.",
        columns,
    });

    // account_groups
    let mut columns = record_columns("Object id of the group; accounts.group_id refers to it.");
    columns.extend([
        col("position", Kind::BigInt, "Sort position in Banking4."),
        col("name", Kind::Text, "Name of the group."),
        col("description", Kind::Text, "Free text the user entered."),
        col("flags", Kind::Text, "Settings of the group as flag names."),
        col(
            "exclude_from_totals",
            Kind::Boolean,
            "True when the group is left out of Banking4's total of all balances.",
        ),
    ]);
    tables.push(Table {
        name: table::ACCOUNT_GROUPS,
        description: "One row per group that accounts are sorted into.",
        columns,
    });

    // bookings
    let mut columns = account_record_columns(ACCOUNT_ID);
    columns.extend([
        col(
            "booking_date",
            Kind::Date,
            "Day the bank booked the entry. NULL for an entry that is not booked yet and for \
             a stored date that is no calendar date.",
        ),
        col(
            "value_date",
            Kind::Date,
            "Value date. NULL when the bank gave none, and always for cash accounts.",
        ),
        col(
            "status",
            Kind::Text,
            "booked, pending (announced by the bank, not booked yet) or information (a notice \
             that moves no money).",
        ),
        col(
            "pending",
            Kind::Boolean,
            "True when status is not booked. Pending entries are not part of the account's \
             stored balance.",
        ),
    ]);
    columns.extend(amount_columns(false));
    columns.extend([
        col(
            "currency",
            Kind::Text,
            "Currency of `amount`: the account's currency, which is what Banking4 shows, in a \
             PayPal account too. The stored code for a booking whose account has no currency \
             or no row in `accounts`.",
        ),
        col(
            "stored_currency",
            Kind::Text,
            "The currency code stored in the record. Banking4 keeps it only in PayPal \
             accounts, where it can differ from `currency`; elsewhere it replaces it with the \
             account's.",
        ),
    ]);
    columns.extend(payee_columns());
    columns.extend(purpose_and_category_columns());
    columns.extend([
        col(
            "booking_text",
            Kind::Text,
            "The bank's name for the kind of transaction, such as `Lastschrift`.",
        ),
        col("notes", Kind::Text, "Note the user attached to the entry."),
        col(
            "counted",
            Kind::Boolean,
            "False for the master row of a split or batch that has parts: its parts carry the \
             money. Sum `amount` over rows where counted is true, or money is counted twice.",
        ),
        col(
            "split_role",
            Kind::Text,
            "Role in a split or batch: split_master, split_slave, batch_master or batch_slave. \
             NULL for an ordinary entry.",
        ),
        col(
            "master_id",
            Kind::BigInt,
            "id of the master row of the split or batch the entry belongs to, in the same \
             account. A master points at itself. NULL for an ordinary entry.",
        ),
        col(
            "batch_index",
            Kind::BigInt,
            "Position within its split or batch: 0 for the master and for ordinary entries, 1 \
             and up for the parts.",
        ),
        col(
            "flag",
            Kind::Text,
            "Colour mark the user set: none, ok, red, blue, yellow, green, gray or purple.",
        ),
        col(
            "is_read",
            Kind::Boolean,
            "True when the user has seen the entry.",
        ),
        col(
            "entry_bits",
            Kind::Text,
            "Processing marks as flag names: read, exported_to_books, uploaded.",
        ),
        col(
            "transaction_code",
            Kind::Text,
            "Kind of transaction as stored: other, remittance, debit, withdrawal_or_deposit, \
             check, standing_order, reversal, charges, cash_management or card_payment.",
        ),
        col(
            "gvc",
            Kind::BigInt,
            "The German banks' business transaction code (Geschäftsvorfallcode); 0 when none.",
        ),
        col(
            "gvc_extension",
            Kind::BigInt,
            "Extension of the business transaction code.",
        ),
        col(
            "swift_transaction_code",
            Kind::Text,
            "SWIFT transaction type of the statement line, such as NTRF.",
        ),
        col("primanota", Kind::Text, "The bank's journal number."),
        col(
            "bank_reference",
            Kind::Text,
            "The bank's reference for the entry.",
        ),
    ]);
    columns.extend(sepa_columns());
    columns.extend([
        col(
            "transaction_date",
            Kind::Date,
            "Day the transaction took place, for card payments. NULL when not given, and \
             always for cash accounts.",
        ),
        col(
            "analysis_date",
            Kind::Date,
            "Day the user assigned the entry to for evaluations, when it differs from the \
             booking date. NULL when not set.",
        ),
        col(
            "vat_amount",
            Kind::Money,
            "Value added tax contained in the amount, without sign; 0 when none.",
        ),
        col(
            "vat_rate_type",
            Kind::Text,
            "Kind of tax rate as flag names.",
        ),
        col(
            "instructed_amount",
            Kind::Money,
            "Original amount in `instructed_currency` when the transaction was in another \
             currency; 0 when none. Sign as delivered by the bank.",
        ),
        col(
            "instructed_currency",
            Kind::Text,
            "Currency of `instructed_amount`.",
        ),
        col(
            "charges_amount",
            Kind::Money,
            "Charges contained in the entry, in `charges_currency`; 0 when none.",
        ),
        col(
            "charges_currency",
            Kind::Text,
            "Currency of `charges_amount`.",
        ),
        col(
            "virtual_iban",
            Kind::Text,
            "Virtual IBAN the payment was addressed to.",
        ),
        col(
            "debit_return_order_id",
            Kind::Text,
            "Order number of a returned direct debit.",
        ),
        col(
            "debit_return_date",
            Kind::Date,
            "Day a direct debit was returned.",
        ),
        col(
            "debit_return_time",
            Kind::Time,
            "Time of day a direct debit was returned.",
        ),
    ]);
    columns.extend(foreign_payment_columns());
    tables.push(Table {
        name: table::BOOKINGS,
        description: "One row per statement entry of an account, booked or pending. `amount` \
                      is signed (negative = money leaving the account). The master row of a \
                      split or batch and its parts are separate rows: sum only rows where \
                      `counted` is true. The view bookings_view adds account and category \
                      context.",
        columns,
    });

    // payments
    let mut columns = account_record_columns(ACCOUNT_ID);
    columns.extend(scheme_columns());
    columns.extend(amount_columns(true));
    columns.extend([
        col(
            "currency",
            Kind::Text,
            "Currency of `amount`: the stored code, or the scheme's or the account's currency \
             when none is stored.",
        ),
        col(
            "stored_currency",
            Kind::Text,
            "The currency code stored in the record.",
        ),
        col(
            "scheduled_date",
            Kind::Date,
            "Day the payment is to be executed. NULL for immediate execution.",
        ),
        col(
            "scheduled_time",
            Kind::Time,
            "Time of day for the execution; only meaningful for instant transfers.",
        ),
    ]);
    columns.extend(payee_columns());
    columns.extend(purpose_and_category_columns());
    columns.extend(sepa_columns());
    columns.extend([
        col(
            "state",
            Kind::Text,
            "Where the payment stands, as flag names: local (entered, not sent), in_outbox, \
             sent, postdated_at_bank, reconciled (matched with a booking) and others.",
        ),
        col(
            "flags",
            Kind::Text,
            "Further marks of the payment as flag names.",
        ),
        col(
            "hidden_by_banking4",
            Kind::Boolean,
            "True for a payment Banking4 itself drops when it loads the file. It is kept here \
             because the vault holds it.",
        ),
        col(
            "master_id",
            Kind::BigInt,
            "id of the master row of the batch the payment belongs to, in the same account. A \
             master points at itself. NULL when not in a batch.",
        ),
        col(
            "batch_index",
            Kind::BigInt,
            "Position within its batch: 0 for the master and for single payments.",
        ),
        col(
            "flag",
            Kind::Text,
            "Colour mark the user set, as in bookings.",
        ),
        col(
            "initiator_name",
            Kind::Text,
            "Name of the person who entered the payment.",
        ),
        col(
            "batch_booking",
            Kind::Text,
            "Whether a batch is to be booked as one entry: default, yes, no or mixed.",
        ),
        col(
            "import_id",
            Kind::Text,
            "Identifier from the file the payment was imported from.",
        ),
        col(
            "payment_reference",
            Kind::Text,
            "Reference Banking4 gave the payment.",
        ),
        col(
            "reconciled_booking_id",
            Kind::BigInt,
            "id of the booking the payment was matched with (bookings.id of the same account). \
             NULL when not matched.",
        ),
        col(
            "outbox_id",
            Kind::BigInt,
            "Object id of the outbox order that carries the payment. The outbox itself is \
             bank access data and is not exported. NULL when none.",
        ),
        col(
            "sepa_status",
            Kind::Text,
            "Status the bank reported, as a name: none, received, pending, rejected, the \
             accepted_* stages, or unknown for a code b4 does not know.",
        ),
        col(
            "sepa_status_code",
            Kind::Text,
            "The four-letter status code as stored, such as ACSC.",
        ),
        col(
            "sepa_reason_code",
            Kind::Text,
            "Reason code the bank gave with the status.",
        ),
        col(
            "sepa_additional_info",
            Kind::Text,
            "Additional status text from the bank.",
        ),
        col(
            "payee_verification_choice",
            Kind::Text,
            "The user's choice for the verification of the payee: default, opt_in or opt_out.",
        ),
        col(
            "payee_verification_json",
            Kind::Text,
            "Result of the bank's verification of the payee, as the stored JSON text.",
        ),
        col(
            "cost_bearer_account_id",
            Kind::BigInt,
            "Account that bears the charges of a foreign payment (accounts.id). NULL when none.",
        ),
    ]);
    columns.extend(foreign_payment_columns());
    tables.push(Table {
        name: table::PAYMENTS,
        description: "One row per payment entered in Banking4 (transfers and direct debits), \
                      sent or not. `amount` is signed like in bookings. These are orders, not \
                      account movements: what the bank booked is in `bookings`. TANs are never \
                      exported.",
        columns,
    });

    // standing_orders
    let mut columns = account_record_columns(ACCOUNT_ID);
    columns.extend(scheme_columns());
    columns.extend(amount_columns(true));
    columns.extend([
        col(
            "currency",
            Kind::Text,
            "Currency of `amount`: the stored code, or the scheme's or the account's currency \
             when none is stored.",
        ),
        col(
            "stored_currency",
            Kind::Text,
            "The currency code stored in the record.",
        ),
    ]);
    columns.extend(payee_columns());
    columns.extend(purpose_and_category_columns());
    columns.extend(sepa_columns());
    columns.extend([
        col(
            "state",
            Kind::Text,
            "Where the order stands, as flag names: at_bank, local (kept in Banking4 only), \
             setup_in_outbox, change_in_outbox, cancel_in_outbox, auto_detected, deleted.",
        ),
        col(
            "is_local",
            Kind::Boolean,
            "True when the order is kept in Banking4 only and the bank does not know it.",
        ),
        col(
            "first_execution_date",
            Kind::Date,
            "Day of the first execution.",
        ),
        col(
            "last_execution_date",
            Kind::Date,
            "Day of the last execution. NULL means until further notice.",
        ),
        col(
            "next_execution_date",
            Kind::Date,
            "Next execution day, for local orders only. NULL for orders held at the bank: \
             Banking4 computes their next day and b4 does not.",
        ),
        col(
            "stored_next_execution_date",
            Kind::Date,
            "The next execution day as stored; can be out of date for orders held at the bank.",
        ),
        col(
            "period_unit",
            Kind::Text,
            "Unit of the interval: month or week.",
        ),
        col(
            "period_length",
            Kind::BigInt,
            "Length of the interval in units: 1 with month is monthly, 3 quarterly.",
        ),
        col(
            "execution_day",
            Kind::BigInt,
            "Day of the month or of the week the order runs on; 97 to 99 stand for days \
             counted from the end of the month.",
        ),
        col(
            "execution_time",
            Kind::Time,
            "Time of day for the execution.",
        ),
        col(
            "outbox_id",
            Kind::BigInt,
            "Object id of the outbox order that carries a change to the standing order. NULL \
             when none.",
        ),
    ]);
    columns.extend(foreign_payment_columns());
    tables.push(Table {
        name: table::STANDING_ORDERS,
        description: "One row per standing order of an account, held at the bank or kept in \
                      Banking4 only. `amount` is signed like in bookings and is the amount of \
                      one execution.",
        columns,
    });

    // payees
    let mut columns = record_columns("Object id of the payee; payers.payee_id refers to it.");
    columns.extend([
        col("name", Kind::Text, "Name of the payee."),
        col("iban", Kind::Text, "IBAN. Several payees can share one."),
        col("bic", Kind::Text, "BIC of the payee's bank."),
        col(
            "account_number",
            Kind::Text,
            "National account number, in old entries.",
        ),
        col(
            "bank_code",
            Kind::Text,
            "National bank code, in old entries.",
        ),
        col("street", Kind::Text, "Street of the payee's address."),
        col(
            "town",
            Kind::Text,
            "Postal code and town of the payee's address.",
        ),
        col("country", Kind::Text, "Country code of the payee."),
        col("bank_name", Kind::Text, "Name of the payee's bank."),
        col("bank_street", Kind::Text, "Street of the payee's bank."),
        col("bank_town", Kind::Text, "Town of the payee's bank."),
        col(
            "purpose",
            Kind::Text,
            "Default remittance text for payments to the payee, as stored.",
        ),
        col(
            "category",
            Kind::Text,
            "Default category for payments to the payee, as a full name.",
        ),
        col(
            "category_key",
            Kind::Text,
            "Matching key of `category`; join it to categories.category_key.",
        ),
        col(
            "amount",
            Kind::Money,
            "Default amount for payments to the payee, without direction; 0 when none.",
        ),
        col("currency", Kind::Text, "Currency of the default amount."),
        col("email", Kind::Text, "E-mail address."),
        col(
            "customer_number",
            Kind::Text,
            "The user's customer number at the payee.",
        ),
        col(
            "notes",
            Kind::Text,
            "Notes the user entered, with their line breaks as stored.",
        ),
        col(
            "kind",
            Kind::Text,
            "none, payee, legacy_mandate (a payee with an old-style mandate) or auto (created \
             by Banking4 from a booking).",
        ),
        col(
            "stored_kind",
            Kind::Text,
            "The kind as stored, before Banking4 reduces it.",
        ),
        col("flags", Kind::Text, "Marks of the payee as flag names."),
        col(
            "vop_status",
            Kind::Text,
            "Result of the last verification of the payee's name by the bank: none, match, \
             close_match, no_match, not_applicable or pending.",
        ),
        col(
            "vop_matched_name",
            Kind::Text,
            "The name the bank reported for a close match.",
        ),
        col(
            "mandate_id",
            Kind::Text,
            "Mandate reference of an old-style mandate stored with the payee.",
        ),
        col(
            "mandate_signed_date",
            Kind::Date,
            "Day that mandate was signed.",
        ),
        col(
            "mandate_revocation_date",
            Kind::Date,
            "Day that mandate was revoked.",
        ),
        col(
            "local_instrument_code",
            Kind::Text,
            "SEPA local instrument code of that mandate.",
        ),
    ]);
    tables.push(Table {
        name: table::PAYEES,
        description: "One row per entry of the address book of counterparties. Bookings do \
                      not refer to payees by id: they carry the name and account themselves.",
        columns,
    });

    // payers
    let mut columns = record_columns("Object id of the payer.");
    columns.extend([
        col(
            "payee_id",
            Kind::BigInt,
            "The address book entry the payer belongs to (payees.id).",
        ),
        col("name", Kind::Text, "Name of the payer."),
        col("mandate_id", Kind::Text, "Mandate reference."),
        col(
            "mandate_signed_date",
            Kind::Date,
            "Day the mandate was signed.",
        ),
        col(
            "mandate_revocation_date",
            Kind::Date,
            "Day the mandate was revoked.",
        ),
        col(
            "local_instrument_code",
            Kind::Text,
            "SEPA local instrument code, such as CORE or B2B.",
        ),
        col(
            "notes",
            Kind::Text,
            "Notes the user entered, with their line breaks as stored.",
        ),
        col(
            "category",
            Kind::Text,
            "Default category for direct debits from the payer, as a full name.",
        ),
        col(
            "category_key",
            Kind::Text,
            "Matching key of `category`; join it to categories.category_key.",
        ),
    ]);
    tables.push(Table {
        name: table::PAYERS,
        description: "One row per counterparty the user collects direct debits from, with \
                      the mandate that allows it.",
        columns,
    });

    // categories
    let mut columns = record_columns(
        "Object id of the category. Nothing refers to it: other records name a category by \
         its text.",
    );
    columns.extend([
        col(
            "full_name",
            Kind::Text,
            "Full name with levels separated by `:`, such as `Wohnen:Nebenkosten:Strom`. NULL \
             for a record without a usable name.",
        ),
        col(
            "category_key",
            Kind::Text,
            "Matching key: full_name trimmed and upper-cased. bookings.category_key joins \
             here. Not guaranteed unique.",
        ),
        col("name", Kind::Text, "The last level of full_name."),
        col(
            "parent",
            Kind::Text,
            "Full name of the parent category. NULL for a top-level category. The parent need \
             not exist as a row.",
        ),
        col(
            "parent_key",
            Kind::Text,
            "category_key of the parent. NULL for a top-level category.",
        ),
        col(
            "depth",
            Kind::BigInt,
            "Level in the tree: 0 for a top-level category.",
        ),
        col("description", Kind::Text, "Free text the user entered."),
        col(
            "special",
            Kind::Text,
            "none for an ordinary category; reclassification (transfers between own accounts), \
             excluded (left out of evaluations) or vat for Banking4's built-in categories. The \
             role the category has in Banking4: each role belongs to the first category that \
             stores it, a later one that stores it again is an ordinary category here, and so \
             is one Banking4 drops. See stored_special.",
        ),
        col(
            "totals_bucket",
            Kind::Text,
            "How Banking4 counts the category in evaluations: auto (by the sign of each \
             booking), income, expense or exclude.",
        ),
        col(
            "budget_period",
            Kind::Text,
            "Period the budget applies to: none, week, month, quarter, year, all or \
             month_range.",
        ),
        col(
            "budget_amount",
            Kind::Money,
            "Budget per `budget_period`, as stored; 0 when none.",
        ),
        col(
            "vat_percent",
            Kind::Decimal,
            "Value added tax rate of the category in percent; 0 when none.",
        ),
        col("icon_name", Kind::Text, "Name of the icon."),
        col("color_name", Kind::Text, "Name of the colour."),
        col(
            "rules",
            Kind::Text,
            "Keyword rules that assign bookings to the category automatically, as the stored \
             expression.",
        ),
        col(
            "layout_version",
            Kind::BigInt,
            "Layout version of the stored record; needed to interpret `rules`, whose syntax \
             changed with version 4.",
        ),
        col(
            "stored_full_name",
            Kind::Text,
            "The name as stored, before normalisation.",
        ),
        col(
            "hidden_by_banking4",
            Kind::Boolean,
            "True for a record Banking4 itself drops when it loads the catalogue.",
        ),
        col(
            "stored_special",
            Kind::Text,
            "The special role as stored in the record. Differs from `special` for a category \
             that repeats the role of an earlier one and for one Banking4 drops.",
        ),
    ]);
    tables.push(Table {
        name: table::CATEGORIES,
        description: "One row per category of the catalogue, a tree whose levels are \
                      separated by `:` in the name. Bookings name their category by text and \
                      can use names that have no row here.",
        columns,
    });

    // mandates
    let mut columns = record_columns("Object id of the mandate.");
    columns.extend([
        col(
            "creditor_id",
            Kind::Text,
            "SEPA creditor identifier of the creditor.",
        ),
        col("mandate_id", Kind::Text, "Mandate reference."),
        col("creditor_name", Kind::Text, "Name of the creditor."),
        col(
            "category",
            Kind::Text,
            "Category for direct debits under the mandate, as a full name.",
        ),
        col(
            "category_key",
            Kind::Text,
            "Matching key of `category`; join it to categories.category_key.",
        ),
        col("flags", Kind::Text, "Marks of the mandate as flag names."),
        col(
            "hidden",
            Kind::Boolean,
            "True when the user hid the mandate.",
        ),
        col(
            "revoked",
            Kind::Boolean,
            "True when the mandate was revoked.",
        ),
        col(
            "mandate_date",
            Kind::Timestamp,
            "Date of the mandate; zone unknown, normally midnight.",
        ),
        col(
            "hidden_by_banking4",
            Kind::Boolean,
            "True for a record Banking4 itself drops when it loads the file.",
        ),
    ]);
    tables.push(Table {
        name: table::MANDATES,
        description: "One row per direct debit mandate the user gave to a creditor, as \
                      Banking4 collected them from bookings.",
        columns,
    });

    // exchange_rates
    let mut columns = record_columns("Object id of the rate.");
    columns.extend([
        col(
            "base_currency",
            Kind::Text,
            "Currency one unit of which is priced.",
        ),
        col(
            "target_currency",
            Kind::Text,
            "Currency the price is given in.",
        ),
        col(
            "rate",
            Kind::Decimal,
            "Units of target_currency per one unit of base_currency, as stored.",
        ),
        col(
            "quoted_at",
            Kind::Timestamp,
            "When the rate was quoted; zone unknown.",
        ),
        col(
            "hidden_by_banking4",
            Kind::Boolean,
            "True for a rate Banking4 itself drops when it loads the file (a duplicate or one \
             it cannot use).",
        ),
    ]);
    tables.push(Table {
        name: table::EXCHANGE_RATES,
        description: "One row per stored exchange rate, the latest quote per currency pair. \
                      Stored as found: no rate is inverted or converted.",
        columns,
    });

    // stock_instruments
    let mut columns =
        record_columns("Object id of the instrument; stock_prices.instrument_id refers to it.");
    columns.extend([
        col(
            "isin",
            Kind::Text,
            "ISIN of the security. NULL when the stored identifier is no ISIN and yields none. \
             portfolio_items.isin joins here.",
        ),
        col("stored_isin", Kind::Text, "The identifier text as stored."),
        col("name", Kind::Text, "Name of the security."),
        col(
            "price_currency",
            Kind::Text,
            "Currency the prices of the instrument are in.",
        ),
        col(
            "splits",
            Kind::Text,
            "Stock splits as the stored text, one per line. NULL when none. No price is \
             adjusted for a split.",
        ),
    ]);
    tables.push(Table {
        name: table::STOCK_INSTRUMENTS,
        description: "One row per security Banking4 keeps prices for.",
        columns,
    });

    // stock_prices
    let mut columns = vec![col(
        "instrument_id",
        Kind::BigInt,
        "The instrument the price belongs to (stock_instruments.id), taken from the directory \
         the record is stored in.",
    )];
    columns.extend(record_columns(
        "Object id of the price record. Unique within one instrument at most.",
    ));
    columns.extend([
        col("price_date", Kind::Date, "Day the price is for."),
        col(
            "price",
            Kind::Decimal,
            "Price of one unit in the instrument's price_currency.",
        ),
    ]);
    tables.push(Table {
        name: table::STOCK_PRICES,
        description: "One row per stored daily price of a security. More than one row per \
                      day is possible.",
        columns,
    });

    // portfolio_items
    let mut columns = account_record_columns(ACCOUNT_ID);
    columns.extend([
        col(
            "state",
            Kind::Text,
            "Origin and state as flag names: bank (reported by the bank), local (entered by \
             hand), deleted.",
        ),
        col(
            "deleted",
            Kind::Boolean,
            "True when the position is marked deleted.",
        ),
        col(
            "isin",
            Kind::Text,
            "ISIN of the security; joins to stock_instruments.isin. NULL when the stored \
             identifier yields none.",
        ),
        col("stored_isin", Kind::Text, "The identifier text as stored."),
        col("name", Kind::Text, "Name of the security."),
        col(
            "quantity",
            Kind::Decimal,
            "Size of the position: a number of units, or a face amount in quantity_currency \
             (see quantity_type).",
        ),
        col(
            "quantity_type",
            Kind::Text,
            "units (pieces), face_amount (nominal value, for bonds) or undefined.",
        ),
        col(
            "quantity_currency",
            Kind::Text,
            "Currency of a face amount. NULL when the quantity counts units.",
        ),
        col(
            "price_type",
            Kind::Text,
            "How prices are quoted: actual (money per unit), percent (of the face amount), \
             yield or undefined.",
        ),
        col(
            "current_price",
            Kind::Decimal,
            "Latest price, in current_price_currency or in percent; never negative.",
        ),
        col(
            "current_price_currency",
            Kind::Text,
            "Currency of current_price.",
        ),
        col("current_price_date", Kind::Date, "Day of the latest price."),
        col(
            "current_price_time",
            Kind::Time,
            "Time of day of the latest price.",
        ),
        col(
            "purchase_price",
            Kind::Decimal,
            "Purchase price per unit, in purchase_price_currency or in percent; never negative.",
        ),
        col(
            "purchase_price_currency",
            Kind::Text,
            "Currency of purchase_price.",
        ),
        col("purchase_price_date", Kind::Date, "Day of the purchase."),
        col(
            "interest_rate",
            Kind::Decimal,
            "Interest rate of a bond in percent; 0 when none.",
        ),
        col("maturity_date", Kind::Date, "Day a bond matures."),
        col(
            "blocked_until_date",
            Kind::Date,
            "Day until which the position is blocked.",
        ),
        col(
            "alert_lower_limit",
            Kind::Decimal,
            "Price below which the user wants to be alerted; 0 when none.",
        ),
        col(
            "alert_upper_limit",
            Kind::Decimal,
            "Price above which the user wants to be alerted; 0 when none.",
        ),
        col(
            "portfolio_key",
            Kind::Text,
            "Key of the position at the bank.",
        ),
        col(
            "additional_info",
            Kind::Text,
            "Further text for the position; several lines are joined with a line feed.",
        ),
    ]);
    tables.push(Table {
        name: table::PORTFOLIO_ITEMS,
        description: "One row per position of a securities account. No market value is \
                      stored: for a position in units it is quantity times current_price.",
        columns,
    });

    // portfolio_transactions
    let mut columns = account_record_columns(ACCOUNT_ID);
    columns.extend([
        col(
            "transaction_id",
            Kind::Text,
            "The bank's identifier of the transaction.",
        ),
        col("state", Kind::Text, "none, pending, executed or settled."),
        col(
            "delivery_direction",
            Kind::Text,
            "Which way the securities moved: receive (a purchase), deliver (a sale) or none.",
        ),
        col(
            "activity",
            Kind::Text,
            "Kind of transaction: settlement, corporate_action, collateral, borrowing_lending \
             or none.",
        ),
        col(
            "isin",
            Kind::Text,
            "ISIN of the security; joins to stock_instruments.isin.",
        ),
        col("stored_isin", Kind::Text, "The identifier text as stored."),
        col("name", Kind::Text, "Name of the security."),
        col(
            "quantity",
            Kind::Decimal,
            "Number of units or face amount moved, without sign (see delivery_direction).",
        ),
        col(
            "quantity_type",
            Kind::Text,
            "units, face_amount or undefined.",
        ),
        col(
            "quantity_currency",
            Kind::Text,
            "Currency of a face amount.",
        ),
        col(
            "traded_at",
            Kind::Timestamp,
            "When the trade took place; zone unknown.",
        ),
        col(
            "trading_price",
            Kind::Decimal,
            "Price per unit of the trade, in trading_currency.",
        ),
        col("trading_currency", Kind::Text, "Currency of trading_price."),
        col(
            "settled_at",
            Kind::Timestamp,
            "Settlement day; zone unknown, normally midnight.",
        ),
        col(
            "settlement_amount",
            Kind::Money,
            "Money that changed hands, without sign (see delivery_direction).",
        ),
        col(
            "settlement_currency",
            Kind::Text,
            "Currency of settlement_amount.",
        ),
        col(
            "interest_accrued",
            Kind::Boolean,
            "True when accrued interest was settled with the trade.",
        ),
        col(
            "interest_days",
            Kind::BigInt,
            "Number of days of accrued interest.",
        ),
        col(
            "interest_amount",
            Kind::Money,
            "Accrued interest; 0 when none.",
        ),
        col(
            "interest_currency",
            Kind::Text,
            "Currency of interest_amount.",
        ),
        col(
            "is_read",
            Kind::Boolean,
            "True when the user has seen the transaction.",
        ),
    ]);
    tables.push(Table {
        name: table::PORTFOLIO_TRANSACTIONS,
        description: "One row per securities transaction of a securities account (purchases, \
                      sales, corporate actions).",
        columns,
    });

    // tax_exemptions
    let mut columns = account_record_columns(ACCOUNT_ID);
    columns.extend([
        col(
            "valid_from",
            Kind::Timestamp,
            "First day the order is valid; zone unknown, normally midnight.",
        ),
        col(
            "valid_until",
            Kind::Timestamp,
            "Last day the order is valid; zone unknown.",
        ),
        col("amount", Kind::Money, "Exempted amount per year."),
        col(
            "used_amount",
            Kind::Money,
            "Part of it used so far, as stored.",
        ),
        col(
            "remaining_amount",
            Kind::Money,
            "Part of it still available, as stored.",
        ),
    ]);
    tables.push(Table {
        name: table::TAX_EXEMPTIONS,
        description: "One row per tax exemption order (Freistellungsauftrag) of an account.",
        columns,
    });

    // assets
    let mut columns = account_record_columns(ACCOUNT_ID);
    columns.extend([
        col(
            "asset_id",
            Kind::Text,
            "Identifier of the item at the bank.",
        ),
        col(
            "amount",
            Kind::Money,
            "Signed value of the item in `currency`: negative for a liability such as a loan.",
        ),
        col("debit", Kind::Boolean, "True when the item is a liability."),
        col("currency", Kind::Text, "Currency of `amount`."),
        col(
            "deposit_date",
            Kind::Date,
            "Day the money was deposited or the loan paid out.",
        ),
        col("expiry_date", Kind::Date, "Day the item ends."),
        col("interest_rate", Kind::Decimal, "Interest rate in percent."),
        col(
            "interest_method",
            Kind::Text,
            "Code of the interest calculation method.",
        ),
        col("description", Kind::Text, "Description of the item."),
    ]);
    tables.push(Table {
        name: table::ASSETS,
        description: "One row per item of an asset account: term deposits, savings plans, \
                      loans.",
        columns,
    });

    // meta, warnings
    tables.push(Table {
        name: META_TABLE,
        description: "Describes the database itself, one row per key: the b4 and schema \
                      version that wrote it, the vault it came from and when it was decrypted.",
        columns: vec![
            col(
                "key",
                Kind::Text,
                "Name of the entry, such as schema_version, vault_path or decrypted_at.",
            ),
            col("value", Kind::Text, "Its value as text."),
        ],
    });
    tables.push(Table {
        name: WARNINGS_TABLE,
        description: "What the reader noticed while decrypting, one row per warning: vault \
                      entries it does not know, records it could not decode, values that did \
                      not fit their column. A warning names files, columns and counts, never \
                      a value from the vault.",
        columns: vec![col("message", Kind::Text, "The warning as one sentence.")],
    });

    tables
}

fn view_col(
    name: &'static str,
    sql_type: &'static str,
    expression: &str,
    description: &'static str,
) -> ViewColumn {
    ViewColumn {
        name,
        sql_type,
        expression: expression.to_owned(),
        description,
    }
}

fn build_views() -> Vec<View> {
    let mut views = Vec::new();

    // bookings_view: every column of bookings under its own name and
    // description, then the context.
    let mut columns: Vec<ViewColumn> = build_tables()
        .into_iter()
        .filter(|table| table.name == table::BOOKINGS)
        .flat_map(|table| table.columns)
        .map(|column| ViewColumn {
            name: column.name,
            sql_type: column.kind.sql_type(),
            expression: format!("b.{}", quote(column.name)),
            description: column.description,
        })
        .collect();
    columns.extend([
        view_col(
            "account_name",
            "VARCHAR",
            "a.name",
            "Name of the account (accounts.name). NULL when the account has no row.",
        ),
        view_col(
            "account_iban",
            "VARCHAR",
            "a.iban",
            "IBAN of the account (accounts.iban).",
        ),
        view_col(
            "account_kind",
            "VARCHAR",
            "a.kind",
            "Kind of the account (accounts.kind), such as bank or credit_card.",
        ),
        view_col(
            "account_currency",
            "VARCHAR",
            "a.currency",
            "Currency of the account (accounts.currency).",
        ),
        view_col(
            "account_hidden",
            "BOOLEAN",
            "a.hidden",
            "True when the account is hidden in Banking4 (accounts.hidden).",
        ),
        view_col(
            "booking_month",
            "DATE",
            "cast(date_trunc('month', b.booking_date) as date)",
            "First day of the month of booking_date, for grouping by month. NULL without a \
             booking date.",
        ),
        view_col(
            "category_name",
            "VARCHAR",
            "string_split(b.category, ':')[-1]",
            "Last level of `category`: `Strom` for `Wohnen:Nebenkosten:Strom`.",
        ),
        view_col(
            "category_parent",
            "VARCHAR",
            "nullif(array_to_string(string_split(b.category, ':')[1:-2], ':'), '')",
            "The category's parent path: `Wohnen:Nebenkosten` for `Wohnen:Nebenkosten:Strom`. \
             NULL for a top-level category and for an uncategorised booking.",
        ),
        view_col(
            "category_top",
            "VARCHAR",
            "string_split(b.category, ':')[1]",
            "First level of `category`: `Wohnen` for `Wohnen:Nebenkosten:Strom`. Group by it \
             for totals per main category.",
        ),
        view_col(
            "category_special",
            "VARCHAR",
            "c.special",
            "categories.special of the booking's category: reclassification marks transfers \
             between own accounts, excluded marks bookings left out of evaluations. NULL when \
             the category has no row in `categories`.",
        ),
    ]);
    views.push(View {
        name: view::BOOKINGS,
        description: "Every row of `bookings` with all its columns, plus the name, IBAN, kind \
                      and currency of its account and the levels of its category. Nothing is \
                      filtered out: filter on `counted` (and usually `not pending`) before \
                      summing amounts.",
        columns,
        // The joins go through one row per key: neither accounts.id nor
        // categories.category_key is guaranteed unique, and a repeated key
        // must not repeat a booking.
        source: "from bookings b \
                 left join (select * from accounts \
                   qualify row_number() over (partition by id order by rowid) = 1) a \
                   on a.id = b.account_id \
                 left join (select * from categories where category_key is not null \
                   qualify row_number() over (partition by category_key order by rowid) = 1) c \
                   on c.category_key = b.category_key",
    });

    views.push(View {
        name: view::ACCOUNT_BALANCES,
        description: "One row per account: the balance stored in the vault next to the sum of \
                      the account's bookings. The two agree when the vault holds every booking \
                      since the account started at zero; a constant `difference` is the \
                      balance before the first stored booking.",
        columns: vec![
            view_col("account_id", "BIGINT", "a.id", "The account (accounts.id)."),
            view_col("name", "VARCHAR", "a.name", "Name of the account."),
            view_col("kind", "VARCHAR", "a.kind", "Kind of the account."),
            view_col(
                "currency",
                "VARCHAR",
                "a.currency",
                "Currency of all amounts of the row.",
            ),
            view_col(
                "hidden",
                "BOOLEAN",
                "a.hidden",
                "True when the account is hidden in Banking4.",
            ),
            view_col(
                "balance",
                "DECIMAL(18,4)",
                "a.balance",
                "Balance according to the last statement, as stored (accounts.balance).",
            ),
            view_col(
                "balance_at",
                "TIMESTAMP",
                "a.balance_at",
                "Moment the stored balance refers to; zone unknown.",
            ),
            view_col(
                "booking_count",
                "BIGINT",
                "coalesce(s.booking_count, 0)",
                "Number of rows in `bookings` for the account, of every status and role.",
            ),
            view_col(
                "booked_sum",
                "DECIMAL(38,4)",
                "cast(coalesce(s.booked_sum, 0) as decimal(38,4))",
                "Sum of the signed amounts of the booked, counted bookings.",
            ),
            view_col(
                "pending_sum",
                "DECIMAL(38,4)",
                "cast(coalesce(s.pending_sum, 0) as decimal(38,4))",
                "Sum of the signed amounts of the counted bookings that are not booked yet.",
            ),
            view_col(
                "difference",
                "DECIMAL(38,4)",
                "cast(a.balance - coalesce(s.booked_sum, 0) as decimal(38,4))",
                "balance minus booked_sum: 0 when the bookings explain the stored balance.",
            ),
            view_col(
                "first_booking_date",
                "DATE",
                "s.first_booking_date",
                "Earliest booking date of the account. NULL without dated bookings.",
            ),
            view_col(
                "last_booking_date",
                "DATE",
                "s.last_booking_date",
                "Latest booking date of the account. NULL without dated bookings.",
            ),
        ],
        source: "from accounts a \
                 left join (select account_id, count(*) as booking_count, \
                     sum(amount) filter (where counted and not pending) as booked_sum, \
                     sum(amount) filter (where counted and pending) as pending_sum, \
                     min(booking_date) as first_booking_date, \
                     max(booking_date) as last_booking_date \
                   from bookings group by account_id) s \
                   on s.account_id = a.id",
    });

    views.push(View {
        name: view::MONTHLY_CATEGORY_SUMS,
        description: "Sums of the booked, counted bookings per month, category and currency, \
                      over all accounts. Transfers between own accounts are included under \
                      their category; leave them out by category or use bookings_view with \
                      category_special.",
        columns: vec![
            view_col(
                "month",
                "DATE",
                "cast(date_trunc('month', booking_date) as date)",
                "First day of the month of the booking date.",
            ),
            view_col(
                "category",
                "VARCHAR",
                "min(category)",
                "Full category name. NULL for the uncategorised bookings.",
            ),
            view_col(
                "category_key",
                "VARCHAR",
                "category_key",
                "Matching key of the category; joins to categories.category_key.",
            ),
            view_col("currency", "VARCHAR", "currency", "Currency of the sums."),
            view_col(
                "booking_count",
                "BIGINT",
                "count(*)",
                "Number of bookings in the group.",
            ),
            view_col(
                "credits",
                "DECIMAL(38,4)",
                "cast(coalesce(sum(amount) filter (where amount > 0), 0) as decimal(38,4))",
                "Sum of the money that arrived; positive or 0.",
            ),
            view_col(
                "debits",
                "DECIMAL(38,4)",
                "cast(coalesce(sum(amount) filter (where amount < 0), 0) as decimal(38,4))",
                "Sum of the money that left; negative or 0.",
            ),
            view_col(
                "net",
                "DECIMAL(38,4)",
                "cast(sum(amount) as decimal(38,4))",
                "credits plus debits.",
            ),
        ],
        source: "from bookings \
                 where counted and not pending and booking_date is not null \
                 group by cast(date_trunc('month', booking_date) as date), category_key, currency",
    });

    views
}
