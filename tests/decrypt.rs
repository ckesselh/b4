//! `b4 decrypt` from the outside: the demo vault goes in, and everything
//! its manifest expects of reading it (`tests/fixtures/vault/MANIFEST.json`)
//! must come out of the database through `b4 sql`.

// Everything in this file is test code; the allowance for `unwrap` in tests
// only reaches the test functions themselves, not their helpers.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;

const DEMO_VAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault/demo.sub");
const MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/vault/MANIFEST.json"
);
const PASSWORD: &str = "demo";

/// Seconds between 1970-01-01 and 2000-01-01, the epoch of the format's
/// stamps.
const EPOCH_2000: i64 = 946_684_800;

// --- running b4 ---

/// A `b4` invocation that does not see the environment of the test run and
/// has no terminal on any stream.
fn b4(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_b4"));
    command
        .args(args)
        .env_remove("B4_DB")
        .env_remove("B4_PASSWORD")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .stdin(Stdio::null());
    command
}

/// Run `command` with `input` on its standard input.
fn run_with_stdin(mut command: Command, input: &str) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cannot run b4");
    // b4 may fail before it reads the password; a closed pipe is fine then.
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().expect("cannot wait for b4")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is not UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is not UTF-8")
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("b4 was killed by a signal")
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn manifest() -> Value {
    serde_json::from_str(&fs::read_to_string(MANIFEST).unwrap()).unwrap()
}

/// A directory with a copy of the demo vault and, after [`Work::decrypt`], the
/// database `b4.duckdb`.
struct Work {
    dir: TempDir,
}

impl Work {
    fn new() -> Work {
        let work = Work {
            dir: tempfile::tempdir().unwrap(),
        };
        fs::copy(DEMO_VAULT, work.vault()).unwrap();
        work
    }

    /// The demo vault, decrypted.
    fn decrypted() -> Work {
        let work = Work::new();
        let output = work.decrypt(&[]);
        assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
        work
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn vault(&self) -> PathBuf {
        self.path("demo.sub")
    }

    fn db(&self) -> PathBuf {
        self.path("b4.duckdb")
    }

    /// `b4 decrypt <vault> <db> --password-stdin <extra>` with the right
    /// password.
    fn decrypt(&self, extra: &[&str]) -> Output {
        self.decrypt_to(&self.db(), PASSWORD, extra)
    }

    fn decrypt_to(&self, target: &Path, password: &str, extra: &[&str]) -> Output {
        let vault = self.vault();
        let mut args = vec![
            "decrypt",
            vault.to_str().unwrap(),
            target.to_str().unwrap(),
            "--password-stdin",
        ];
        args.extend(extra);
        run_with_stdin(b4(&args), &format!("{password}\n"))
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = b4(args);
        command.arg("--db").arg(self.db());
        command.output().expect("cannot run b4")
    }

    /// The rows of `query` as JSON objects.
    fn sql(&self, query: &str) -> Vec<Value> {
        let output = self.run(&["sql", query, "--output", "json"]);
        assert_eq!(exit_code(&output), 0, "{query}:\n{}", stderr(&output));
        match serde_json::from_str(&stdout(&output)).unwrap() {
            Value::Array(rows) => rows,
            other => panic!("{query}: not an array: {other}"),
        }
    }

    /// The one row of `query`.
    fn row(&self, query: &str) -> Value {
        let mut rows = self.sql(query);
        assert_eq!(rows.len(), 1, "{query}: {rows:?}");
        rows.remove(0)
    }

    /// The one value of `query`, which must select `... as v`.
    fn value(&self, query: &str) -> Value {
        self.row(query)["v"].clone()
    }

    fn count(&self, query: &str) -> i64 {
        self.value(query)
            .as_i64()
            .unwrap_or_else(|| panic!("{query}: not a number"))
    }

    fn files(&self) -> Vec<String> {
        file_names(self.dir.path())
    }
}

/// A decimal text without its trailing zeros, so that `12.5000` and `12.50`
/// compare equal and nothing passes through a float. A zero has no sign.
fn decimal(text: &str) -> String {
    let text = text.trim();
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        text
    };
    match text {
        "-0" | "" | "-" => "0".to_owned(),
        other => other.to_owned(),
    }
}

fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

// --- counts ---

/// Record type of the manifest to table of the database.
const TABLES: [(&str, &str); 14] = [
    ("account", "accounts"),
    ("account_group", "account_groups"),
    ("booking", "bookings"),
    ("payment", "payments"),
    ("standing_order", "standing_orders"),
    ("payee", "payees"),
    ("category", "categories"),
    ("mandate", "mandates"),
    ("exchange_rate", "exchange_rates"),
    ("stock_instrument", "stock_instruments"),
    ("stock_price", "stock_prices"),
    ("portfolio_item", "portfolio_items"),
    ("portfolio_transaction", "portfolio_transactions"),
    ("tax_exemption", "tax_exemptions"),
];

fn table_of(record_type: &str) -> &'static str {
    TABLES
        .iter()
        .find(|(name, _)| *name == record_type)
        .unwrap_or_else(|| panic!("no table for record type {record_type}"))
        .1
}

#[test]
fn every_table_has_the_row_count_of_the_manifest() {
    let work = Work::new();
    let output = work.decrypt(&[]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let summary = stderr(&output);

    let manifest = manifest();
    let totals = manifest["totals"].as_object().unwrap();
    assert_eq!(totals.len(), TABLES.len(), "a record type has no table");
    let mut expected: BTreeMap<&str, i64> = totals
        .iter()
        .map(|(record_type, count)| (table_of(record_type), count.as_i64().unwrap()))
        .collect();
    // The demo vault has no file for these two.
    expected.insert("payers", 0);
    expected.insert("assets", 0);

    for (table, count) in &expected {
        let rows = work.count(&format!("select count(*) as v from {table}"));
        assert_eq!(rows, *count, "{table}");
        // The summary says the same, one line per table.
        let line = summary
            .lines()
            .find(|line| line.split_whitespace().next() == Some(table))
            .unwrap_or_else(|| panic!("no summary line for {table}:\n{summary}"));
        assert_eq!(
            line.split_whitespace().last(),
            Some(count.to_string().as_str()),
            "{line}"
        );
    }
    assert_eq!(
        stdout(&output),
        "",
        "decrypt writes nothing to standard output"
    );
}

// --- accounts and their bookings ---

/// Count and exact sums of one group of bookings, as the manifest gives them.
fn check_group(work: &Work, account: i64, name: &str, filter: &str, expected: &Value) {
    let row = work.row(&format!(
        "select count(*) as count, \
                count(*) filter (where not debit) as credit_count, \
                count(*) filter (where debit) as debit_count, \
                coalesce(sum(amount) filter (where not debit), 0)::varchar as credit_sum, \
                coalesce(-sum(amount) filter (where debit), 0)::varchar as debit_sum, \
                coalesce(sum(amount), 0)::varchar as net \
         from bookings where account_id = {account} and {filter}"
    ));
    for key in ["count", "credit_count", "debit_count"] {
        assert_eq!(row[key], expected[key], "{account} {name} {key}");
    }
    for key in ["credit_sum", "debit_sum", "net"] {
        assert_eq!(
            decimal(row[key].as_str().unwrap()),
            decimal(expected[key].as_str().unwrap()),
            "{account} {name} {key}"
        );
    }
}

#[test]
fn every_account_has_the_bookings_and_sums_of_the_manifest() {
    let work = Work::decrypted();
    let manifest = manifest();
    let accounts = manifest["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 6);

    for expected in accounts {
        let id = expected["oid"].as_i64().unwrap();
        let account = work.row(&format!(
            "select name, kind, iban, bic, bank_code, account_number, currency, holder_name, \
                    position, group_id, hidden, balance::varchar as balance, \
                    strftime(balance_at, '%Y-%m-%dT%H:%M:%S') as balance_at \
             from accounts where id = {id}"
        ));
        for (column, key) in [
            ("name", "name"),
            ("kind", "kind_name"),
            ("iban", "iban"),
            ("bic", "bic"),
            ("bank_code", "bank_code"),
            ("account_number", "account_number"),
            ("currency", "currency"),
            ("holder_name", "holder_name"),
            ("position", "position"),
            ("group_id", "group_oid"),
            ("hidden", "hidden"),
        ] {
            let wanted = match (&expected[key], key) {
                // No group is 0 in the vault and NULL in the database.
                (Value::Number(number), "group_oid") if number.as_i64() == Some(0) => Value::Null,
                (value, _) => value.clone(),
            };
            assert_eq!(account[column], wanted, "account {id}: {column}");
        }
        assert_eq!(
            decimal(account["balance"].as_str().unwrap()),
            decimal(expected["statement_balance"].as_str().unwrap()),
            "account {id}: balance"
        );
        let balance_time = expected["statement_balance_time"]
            .as_str()
            .map(|time| time.trim_end_matches(".0000000").to_owned());
        assert_eq!(
            text_of(&account["balance_at"]),
            balance_time,
            "account {id}: balance_at"
        );

        // Every record falls into exactly one of the manifest's three groups.
        assert_eq!(
            work.count(&format!(
                "select count(*) as v from bookings where account_id = {id}"
            )),
            expected["booking_records"].as_i64().unwrap(),
            "account {id}"
        );
        check_group(
            &work,
            id,
            "booked",
            "counted and not pending",
            &expected["booked"],
        );
        check_group(
            &work,
            id,
            "pending",
            "counted and pending",
            &expected["pending"],
        );
        check_group(&work, id, "masters", "not counted", &expected["masters"]);
        check_group(&work, id, "all_records", "true", &expected["all_records"]);

        let facts = work.row(&format!(
            "select min(booking_date)::varchar as first, max(booking_date)::varchar as last, \
                    count(*) filter (where booking_date is null) as undated, \
                    count(*) filter (where counted and not pending and category is null) \
                      as uncategorised, \
                    count(*) filter (where split_role = 'split_master') as split_masters, \
                    count(*) filter (where split_role = 'split_slave') as split_parts, \
                    count(*) filter (where split_role = 'batch_master') as batch_masters, \
                    count(*) filter (where split_role = 'batch_slave') as batch_parts \
             from bookings where account_id = {id}"
        ));
        assert_eq!(
            facts["first"], expected["first_booking_date"],
            "account {id}"
        );
        assert_eq!(facts["last"], expected["last_booking_date"], "account {id}");
        assert_eq!(
            facts["undated"], expected["records_without_booking_date"],
            "account {id}"
        );
        assert_eq!(
            facts["uncategorised"], expected["booked_without_category"],
            "account {id}"
        );
        for key in [
            "split_masters",
            "split_parts",
            "batch_masters",
            "batch_parts",
        ] {
            assert_eq!(facts[key], expected["groups"][key], "account {id}: {key}");
        }

        // The balance rule of the fixture: the stored balance is the sum of
        // the counted, booked bookings.
        if expected["balance_check"]["applies"] == Value::Bool(true) {
            let balance = work.row(&format!(
                "select balance::varchar as balance, booked_sum::varchar as booked_sum, \
                        difference::varchar as difference, booking_count \
                 from account_balances where account_id = {id}"
            ));
            assert_eq!(
                decimal(balance["difference"].as_str().unwrap()),
                "0",
                "account {id}"
            );
            assert_eq!(
                decimal(balance["booked_sum"].as_str().unwrap()),
                decimal(expected["balance_check"]["booked_net"].as_str().unwrap()),
                "account {id}"
            );
            assert_eq!(
                balance["booking_count"], expected["booking_records"],
                "account {id}"
            );
        }
    }
    assert_eq!(work.count("select count(*) as v from account_balances"), 6);
}

#[test]
fn the_split_and_the_batch_are_counted_exactly_once() {
    let work = Work::decrypted();
    // Each master and its parts: the parts count, the master does not, and
    // in this vault the parts add up to the master.
    let groups = work.sql(
        "select master_id, \
                count(*) filter (where counted) as counted_rows, \
                count(*) filter (where not counted) as masters, \
                sum(amount) filter (where counted)::varchar as parts, \
                sum(amount) filter (where not counted)::varchar as master, \
                min(batch_index) filter (where counted) as first_part, \
                max(batch_index) as last_part, \
                bool_and(id = master_id) filter (where not counted) as master_points_at_itself \
         from bookings where master_id is not null group by master_id order by master_id",
    );
    assert_eq!(groups.len(), 2, "{groups:?}");
    for (group, amount) in groups.iter().zip(["-84.37", "-244.88"]) {
        assert_eq!(group["counted_rows"], 3);
        assert_eq!(group["masters"], 1);
        assert_eq!(decimal(group["parts"].as_str().unwrap()), amount);
        assert_eq!(decimal(group["master"].as_str().unwrap()), amount);
        assert_eq!(group["first_part"], 1);
        assert_eq!(group["last_part"], 3);
        assert_eq!(group["master_points_at_itself"], true);
    }
    assert_eq!(
        work.count("select count(*) as v from bookings where not counted"),
        2
    );

    // A sum over counted rows holds each of the two amounts once; one over
    // all rows holds them twice.
    let manifest = manifest();
    let girokonto = &manifest["accounts"][0];
    let counted = work.value(
        "select sum(amount)::varchar as v from bookings_view \
         where account_name = 'Girokonto' and counted and not pending",
    );
    assert_eq!(
        decimal(counted.as_str().unwrap()),
        decimal(girokonto["booked"]["net"].as_str().unwrap())
    );
    let all = work.value(
        "select (sum(amount) filter (where counted) - sum(amount))::varchar as v from bookings",
    );
    assert_eq!(decimal(all.as_str().unwrap()), "329.25");

    // The view over months and categories applies the rule as well.
    let by_month = work.value("select sum(net)::varchar as v from monthly_category_sums");
    let booked =
        work.value("select sum(amount)::varchar as v from bookings where counted and not pending");
    assert_eq!(by_month, booked);
    assert_eq!(
        work.count("select sum(booking_count)::bigint as v from monthly_category_sums"),
        work.count("select count(*) as v from bookings where counted and not pending")
    );
}

#[test]
fn categories_and_their_bookings_match_the_manifest() {
    let work = Work::decrypted();
    let manifest = manifest();
    let mut without_category = None;
    for expected in manifest["categories"].as_array().unwrap() {
        let filter = match expected["category"].as_str() {
            Some(name) => format!("category = '{}'", name.replace('\'', "''")),
            None => {
                without_category = Some(expected);
                "category is null".to_owned()
            }
        };
        let row = work.row(&format!(
            "select count(*) as records, \
                    count(*) filter (where counted and not pending) as booked, \
                    coalesce(sum(amount) filter (where counted and not pending), 0)::varchar as net \
             from bookings where {filter}"
        ));
        assert_eq!(row["records"], expected["booking_records"], "{filter}");
        assert_eq!(row["booked"], expected["booked"]["count"], "{filter}");
        assert_eq!(
            decimal(row["net"].as_str().unwrap()),
            decimal(expected["booked"]["net"].as_str().unwrap()),
            "{filter}"
        );
        if expected["in_catalogue"] == Value::Bool(true) {
            let name = expected["category"].as_str().unwrap().replace('\'', "''");
            assert_eq!(
                work.count(&format!(
                    "select count(*) as v from categories where full_name = '{name}'"
                )),
                1,
                "{name}"
            );
        }
    }
    assert!(
        without_category.is_some(),
        "the manifest has a row for no category"
    );

    // Every category a booking names joins to the catalogue through the key.
    assert_eq!(
        work.count(
            "select count(*) as v from bookings b where b.category_key is not null \
             and not exists (select 1 from categories c where c.category_key = b.category_key)"
        ),
        0
    );
    // The view gives the parent path and the special role.
    let row = work.row(
        "select count(*) as n, min(category_parent) as parent, min(category_top) as top, \
                min(category_name) as name \
         from bookings_view where category = 'Wohnen:Nebenkosten:Strom'",
    );
    assert!(row["n"].as_i64().unwrap() > 0);
    assert_eq!(row["parent"], "Wohnen:Nebenkosten");
    assert_eq!(row["top"], "Wohnen");
    assert_eq!(row["name"], "Strom");
    assert_eq!(
        work.count(
            "select count(*) as v from bookings_view where category_special = 'reclassification'"
        ),
        work.count("select count(*) as v from bookings where category = '(Umbuchung)'")
    );
    assert_eq!(work.count("select count(*) as v from bookings_view"), 634);
}

// --- the manifest's sample records, field by field ---

type Names = &'static [(i64, &'static str)];

/// How one field of a manifest sample is found in the database.
#[derive(Clone, Copy)]
enum Rule {
    /// The same text.
    Text(&'static str),
    /// A number, as it is.
    Int(&'static str),
    /// A stamp in seconds since 2000 in a UTC timestamp column.
    Stamp(&'static str),
    /// A reference to another record: 0 in the vault is NULL.
    Ref(&'static str),
    Bool(&'static str),
    /// An exact decimal; the scale does not matter.
    Decimal(&'static str),
    /// `YYYY-MM-DD` in a DATE column.
    Date(&'static str),
    /// A date the manifest writes as a timestamp at midnight, in a DATE column.
    DateAtMidnight(&'static str),
    /// `YYYY-MM-DDTHH:MM:SS.fffffff` in a TIMESTAMP column.
    Timestamp(&'static str),
    /// `HH:MM:SS` in a TIME column.
    Time(&'static str),
    /// A coded value: the number in the manifest, its name in the column.
    Code(&'static str, Names),
    /// A flag set: the bits in the manifest, the names of the set flags
    /// joined by commas in the column, NULL when none is set.
    Flags(&'static str, Names),
    /// One bit of a flag set in a BOOLEAN column.
    Bit(&'static str, i64),
    /// An array of lines, joined with a line feed in the column.
    Lines(&'static str),
    /// A security identifier object: its text and the ISIN derived from it.
    SecurityId,
    /// Not compared, and why.
    Not(&'static str),
}

const MEMORY_ONLY: Rule = Rule::Not("exists only in Banking4's memory, not in the vault");
const ACCESS: Rule = Rule::Not("bank access field: dropped when the record is decoded");
const PREFERENCE: Rule = Rule::Not("application preference or internal field: not exported");

const ACCOUNT_KINDS: Names = &[
    (1, "bank"),
    (2, "cash"),
    (4, "credit_card"),
    (8, "portfolio"),
    (16, "asset"),
    (32, "paypal"),
    (64, "crypto"),
];
const ACCOUNT_FLAGS: Names = &[
    (0x1, "hidden"),
    (0x2, "exclude_from_totals"),
    (0x200, "statement_schedule_monthly"),
    (0x1000, "do_not_acknowledge_statements"),
];
const BALANCE_FLAGS: Names = &[
    (0x1, "has_bank_overdraft_limit"),
    (0x2, "has_bank_available_funds"),
    (0x100, "show_overdraft_limit"),
    (0x200, "show_available_funds"),
];
const GROUP_FLAGS: Names = &[(0x4, "exclude_from_totals")];
const TOTALS_BUCKETS: Names = &[(0, "auto"), (1, "exclude"), (2, "income"), (3, "expense")];
const BUDGET_PERIODS: Names = &[
    (0, "none"),
    (1, "week"),
    (2, "month"),
    (3, "quarter"),
    (4, "year"),
];
const SPECIAL: Names = &[
    (0, "none"),
    (1, "reclassification"),
    (2, "excluded"),
    (3, "vat"),
];
const PAYEE_KINDS: Names = &[
    (0, "none"),
    (1, "payee"),
    (2, "legacy_mandate"),
    (4, "auto"),
];
const PAYEE_FLAGS: Names = &[(0x1, "vop_no_match_unsupported")];
const VOP: Names = &[
    (0, "none"),
    (1, "match"),
    (2, "close_match"),
    (3, "no_match"),
];
const MANDATE_FLAGS: Names = &[(0x1, "hidden"), (0x100, "revoked")];
const ITEM_STATE: Names = &[(0x1, "local"), (0x2, "bank"), (0x10000, "deleted")];
const QUANTITY_TYPES: Names = &[(0, "undefined"), (1, "face_amount"), (2, "units")];
const PRICE_TYPES: Names = &[
    (0, "undefined"),
    (1, "actual"),
    (2, "percent"),
    (3, "yield"),
];
const TRANSACTION_STATES: Names = &[(0, "none"), (1, "pending"), (2, "executed"), (3, "settled")];
const DIRECTIONS: Names = &[(0, "none"), (1, "deliver"), (2, "receive")];
const ACTIVITIES: Names = &[
    (0, "none"),
    (1, "borrowing_lending"),
    (2, "collateral"),
    (3, "corporate_action"),
    (4, "settlement"),
];
const SCHEMES: Names = &[
    (0, "none"),
    (1, "user_expense"),
    (2, "user_income"),
    (5, "sepa_credit_transfer"),
    (6, "sepa_direct_debit_core"),
];
const COST_BEARERS: Names = &[(0, "none"), (1, "ours"), (2, "shared"), (3, "beneficiary")];
const BOOKING_STATUS: Names = &[(0, "booked"), (1, "pending"), (2, "information")];
const COLOURS: Names = &[
    (0, "none"),
    (1, "ok"),
    (2, "red"),
    (3, "blue"),
    (4, "yellow"),
    (5, "green"),
    (6, "gray"),
    (7, "purple"),
];
const TRANSACTION_CODES: Names = &[
    (0, "other"),
    (1, "remittance"),
    (2, "debit"),
    (3, "withdrawal_or_deposit"),
    (4, "check"),
    (5, "standing_order"),
    (6, "reversal"),
    (7, "charges"),
    (8, "cash_management"),
    (9, "card_payment"),
];
const ENTRY_BITS: Names = &[(0x1, "read"), (0x2, "exported_to_books"), (0x4, "uploaded")];
const SPLIT_ROLES: Names = &[
    (0x01, "split_master"),
    (0x02, "split_slave"),
    (0x10, "batch_master"),
    (0x20, "batch_slave"),
];
const VAT_RATE_TYPES: Names = &[(0x1, "standard"), (0x2, "reverse_charge")];
const PAYMENT_STATES: Names = &[
    (0x1, "local"),
    (0x2, "in_outbox"),
    (0x20, "postdated_at_bank"),
    (0x40, "sent"),
    (0x200, "reconciled"),
];
const PAYMENT_FLAGS: Names = &[(0x1, "warning")];
const BATCH_BOOKING: Names = &[(0, "default"), (1, "yes"), (2, "no"), (3, "mixed")];
const SEPA_STATUS: Names = &[
    (0, "none"),
    (1, "received"),
    (2, "pending"),
    (9, "accepted_settlement_completed_debtor"),
    (10, "rejected"),
];
const VERIFICATION_CHOICES: Names = &[(0, "default"), (1, "opt_in"), (2, "opt_out")];
const ORDER_STATES: Names = &[(0x1, "local"), (0x10, "at_bank"), (0x20, "auto_detected")];
const PERIOD_UNITS: Names = &[(0, "none"), (77, "month"), (87, "week")];

const RECORD: [(&str, Rule); 2] = [
    ("oid", Rule::Stamp("created_at")),
    ("update_stamp", Rule::Stamp("updated_at")),
];

/// The fields of the payment base, which bookings, payments and standing
/// orders share. Direction, currency and scheme differ per type.
const PAYMENT_BASE: [(&str, Rule); 28] = [
    ("amount", Rule::Decimal("stored_amount")),
    ("counterparty_name", Rule::Text("payee_name")),
    ("counterparty_street", Rule::Text("payee_street")),
    ("counterparty_town", Rule::Text("payee_town")),
    ("counterparty_country", Rule::Text("payee_country")),
    ("counterparty_account", Rule::Text("payee_iban")),
    ("counterparty_bank_code", Rule::Text("payee_bic")),
    ("purpose_lines", Rule::Lines("purpose")),
    ("order_id", Rule::Text("order_id")),
    ("category", Rule::Text("category")),
    ("end_to_end_id", Rule::Text("end_to_end_id")),
    ("payment_info_id", Rule::Text("payment_info_id")),
    ("purpose_code", Rule::Text("purpose_code")),
    ("category_purpose_code", Rule::Text("category_purpose_code")),
    ("mandate_id", Rule::Text("mandate_id")),
    ("mandate_signed_date", Rule::Date("mandate_signed_date")),
    ("creditor_id", Rule::Text("creditor_id")),
    ("local_instrument_code", Rule::Text("local_instrument_code")),
    ("sequence_type", Rule::Text("sequence_type")),
    (
        "ultimate_counterparty_name",
        Rule::Text("ultimate_payee_name"),
    ),
    ("cost_bearer", Rule::Code("cost_bearer", COST_BEARERS)),
    (
        "intl_remittance_method",
        Rule::Text("intl_remittance_method"),
    ),
    (
        "intl_remittance_instruction",
        Rule::Text("intl_remittance_instruction"),
    ),
    ("counterparty_bank_name", Rule::Text("payee_bank_name")),
    ("counterparty_bank_street", Rule::Text("payee_bank_street")),
    ("counterparty_bank_town", Rule::Text("payee_bank_town")),
    (
        "regulatory_reporting_code",
        Rule::Text("regulatory_reporting_code"),
    ),
    ("counterparty_name_digest", MEMORY_ONLY),
];

/// What Banking4 computes from the fields of a record (`derived` in the
/// manifest), for all three types of the payment family.
const DERIVED: [(&str, Rule); 8] = [
    ("effective_debit", Rule::Bool("debit")),
    ("signed_value", Rule::Decimal("amount")),
    ("displayed_currency", Rule::Text("currency")),
    (
        "displayed_category",
        Rule::Not("what Banking4 displays; the column `category` holds the stored name"),
    ),
    ("record_kind", Rule::Not("the record type: it is the table")),
    (
        "scheme_class",
        Rule::Not("part of the stored scheme, exposed as `scheme`"),
    ),
    (
        "reported_scheduled_time",
        Rule::Not("computed from scheduled_time and the scheme; the stored time is the column"),
    ),
    (
        "net_amount",
        Rule::Not("amount minus VAT: computed, not stored"),
    ),
];

fn rules(record_type: &str) -> Vec<(&'static str, Rule)> {
    let mut rules: Vec<(&'static str, Rule)> = RECORD.to_vec();
    let own: &[(&'static str, Rule)] = match record_type {
        "account_group" => &[
            ("position", Rule::Int("position")),
            ("name", Rule::Text("name")),
            ("description", Rule::Text("description")),
            ("flags", Rule::Flags("flags", GROUP_FLAGS)),
        ],
        "account" => &[
            ("account_kind", Rule::Code("kind", ACCOUNT_KINDS)),
            ("country_code", Rule::Text("country_code")),
            ("bank_code", Rule::Text("bank_code")),
            ("account_number", Rule::Text("account_number")),
            ("currency", Rule::Text("currency")),
            ("name", Rule::Text("name")),
            ("iban", Rule::Text("iban")),
            ("bic", Rule::Text("bic")),
            ("holder_name", Rule::Text("holder_name")),
            ("description", Rule::Text("description")),
            ("last_download_time", Rule::Timestamp("last_download_at")),
            ("last_sync_time", Rule::Timestamp("last_sync_at")),
            ("statement_balance_time", Rule::Timestamp("balance_at")),
            ("statement_balance", Rule::Decimal("balance")),
            ("online_balance_time", Rule::Timestamp("online_balance_at")),
            ("online_balance", Rule::Decimal("online_balance")),
            ("flags", Rule::Flags("flags", ACCOUNT_FLAGS)),
            ("flags", Rule::Bit("hidden", 0x1)),
            ("flags", Rule::Bit("exclude_from_totals", 0x2)),
            ("opening_date", Rule::DateAtMidnight("opening_date")),
            ("holder_street", Rule::Text("holder_street")),
            ("holder_town", Rule::Text("holder_town")),
            (
                "credit_interest_rate",
                Rule::Decimal("credit_interest_rate"),
            ),
            ("debit_interest_rate", Rule::Decimal("debit_interest_rate")),
            (
                "overdraft_interest_rate",
                Rule::Decimal("overdraft_interest_rate"),
            ),
            ("overdraft_limit", Rule::Decimal("overdraft_limit")),
            ("creditor_id", Rule::Text("creditor_id")),
            (
                "reference_account_iban",
                Rule::Text("reference_account_iban"),
            ),
            ("originator_name", Rule::Text("originator_name")),
            ("position", Rule::Int("position")),
            ("group_oid", Rule::Ref("group_id")),
            ("balance_flags", Rule::Flags("balance_flags", BALANCE_FLAGS)),
            ("available_funds", Rule::Decimal("available_funds")),
            ("icon_name", Rule::Text("icon_name")),
            ("contact_name", ACCESS),
            ("order_contact_name", ACCESS),
            ("fints_account_sub_id", ACCESS),
            ("last_download_token", ACCESS),
            ("fints_customer_id", ACCESS),
            ("contact_guid", ACCESS),
            ("order_contact_guid", ACCESS),
            ("statement_retention", PREFERENCE),
            ("payments_retention", PREFERENCE),
            ("files_retention", PREFERENCE),
            ("sync_flags", PREFERENCE),
            ("notify_flags", PREFERENCE),
            ("notify_min_debit", PREFERENCE),
            ("notify_min_credit", PREFERENCE),
            ("notify_min_balance", PREFERENCE),
            ("notify_max_balance", PREFERENCE),
            ("notify_spending_limit", PREFERENCE),
            ("sevdesk_last_entry_date", PREFERENCE),
            ("sevdesk_flags", PREFERENCE),
            ("reserved_text", PREFERENCE),
            (
                "internal_transfer_ibans",
                Rule::Not("the list of internal transfer accounts has no table in 0.1.0"),
            ),
        ],
        "category" => &[
            ("full_name", Rule::Text("full_name")),
            ("description", Rule::Text("description")),
            ("rules", Rule::Text("rules")),
            ("totals_bucket", Rule::Code("totals_bucket", TOTALS_BUCKETS)),
            ("budget_period", Rule::Code("budget_period", BUDGET_PERIODS)),
            ("budget_amount", Rule::Decimal("budget_amount")),
            ("vat_percent", Rule::Decimal("vat_percent")),
            ("icon_name", Rule::Text("icon_name")),
            ("color_name", Rule::Text("color_name")),
            ("special_category", Rule::Code("special", SPECIAL)),
            ("pseudo_category", MEMORY_ONLY),
        ],
        "payee" => &[
            ("name", Rule::Text("name")),
            ("account_number", Rule::Text("account_number")),
            ("bank_code", Rule::Text("bank_code")),
            ("purpose", Rule::Text("purpose")),
            ("category", Rule::Text("category")),
            ("iban", Rule::Text("iban")),
            ("bic", Rule::Text("bic")),
            ("notes", Rule::Text("notes")),
            ("street", Rule::Text("street")),
            ("town", Rule::Text("town")),
            ("country", Rule::Text("country")),
            ("bank_name", Rule::Text("bank_name")),
            ("bank_street", Rule::Text("bank_street")),
            ("bank_town", Rule::Text("bank_town")),
            ("mandate_id", Rule::Text("mandate_id")),
            ("mandate_signature_date", Rule::Date("mandate_signed_date")),
            ("currency", Rule::Text("currency")),
            ("amount", Rule::Decimal("amount")),
            ("email", Rule::Text("email")),
            ("customer_number", Rule::Text("customer_number")),
            ("payee_kind", Rule::Code("kind", PAYEE_KINDS)),
            ("local_instrument_code", Rule::Text("local_instrument_code")),
            (
                "mandate_revocation_date",
                Rule::Date("mandate_revocation_date"),
            ),
            ("payee_flags", Rule::Flags("flags", PAYEE_FLAGS)),
            ("vop_status", Rule::Code("vop_status", VOP)),
            ("vop_matched_name", Rule::Text("vop_matched_name")),
            ("name_digest", MEMORY_ONLY),
            ("initials", MEMORY_ONLY),
        ],
        "mandate" => &[
            ("creditor_id", Rule::Text("creditor_id")),
            ("mandate_id", Rule::Text("mandate_id")),
            ("category", Rule::Text("category")),
            ("creditor_name", Rule::Text("creditor_name")),
            ("mandate_flags", Rule::Flags("flags", MANDATE_FLAGS)),
            ("mandate_flags", Rule::Bit("hidden", 0x1)),
            ("mandate_flags", Rule::Bit("revoked", 0x100)),
            ("mandate_date", Rule::Timestamp("mandate_date")),
        ],
        "exchange_rate" => &[
            ("base_currency", Rule::Text("base_currency")),
            ("target_currency", Rule::Text("target_currency")),
            ("quote_time", Rule::Timestamp("quoted_at")),
            ("quote_rate", Rule::Decimal("rate")),
            ("fixed_rate", MEMORY_ONLY),
            ("currency_class", MEMORY_ONLY),
        ],
        "stock_instrument" => &[
            ("isin", Rule::SecurityId),
            ("name", Rule::Text("name")),
            ("price_currency", Rule::Text("price_currency")),
            (
                "splits",
                Rule::Not("the column holds the stored text; the manifest has the parsed list"),
            ),
        ],
        "stock_price" => &[
            ("date", Rule::Date("price_date")),
            ("price", Rule::Decimal("price")),
        ],
        "portfolio_item" => &[
            ("item_state", Rule::Flags("state", ITEM_STATE)),
            ("item_state", Rule::Bit("deleted", 0x10000)),
            ("isin", Rule::Text("isin")),
            ("name", Rule::Text("name")),
            ("maturity_date", Rule::Date("maturity_date")),
            ("interest_rate", Rule::Decimal("interest_rate")),
            ("portfolio_key", Rule::Text("portfolio_key")),
            ("quantity", Rule::Decimal("quantity")),
            ("quantity_type", Rule::Code("quantity_type", QUANTITY_TYPES)),
            ("quantity_currency", Rule::Text("quantity_currency")),
            ("price_type", Rule::Code("price_type", PRICE_TYPES)),
            (
                "current_price_currency",
                Rule::Text("current_price_currency"),
            ),
            ("purchase_price", Rule::Decimal("purchase_price")),
            ("purchase_price_date", Rule::Date("purchase_price_date")),
            ("current_price", Rule::Decimal("current_price")),
            ("current_price_date", Rule::Date("current_price_date")),
            ("current_price_time", Rule::Time("current_price_time")),
            (
                "purchase_price_currency",
                Rule::Text("purchase_price_currency"),
            ),
            ("alert_lower_limit", Rule::Decimal("alert_lower_limit")),
            ("alert_upper_limit", Rule::Decimal("alert_upper_limit")),
            ("blocked_until_date", Rule::Date("blocked_until_date")),
            ("additional_info", Rule::Lines("additional_info")),
        ],
        "portfolio_transaction" => &[
            ("transaction_id", Rule::Text("transaction_id")),
            ("transaction_state", Rule::Code("state", TRANSACTION_STATES)),
            (
                "delivery_direction",
                Rule::Code("delivery_direction", DIRECTIONS),
            ),
            ("activity", Rule::Code("activity", ACTIVITIES)),
            ("isin", Rule::Text("isin")),
            ("name", Rule::Text("name")),
            ("quantity", Rule::Decimal("quantity")),
            ("quantity_type", Rule::Code("quantity_type", QUANTITY_TYPES)),
            ("settlement_date", Rule::Timestamp("settled_at")),
            ("settlement_amount", Rule::Decimal("settlement_amount")),
            ("settlement_currency", Rule::Text("settlement_currency")),
            ("interest_accrued", Rule::Bool("interest_accrued")),
            ("interest_days", Rule::Int("interest_days")),
            ("interest_amount", Rule::Decimal("interest_amount")),
            ("interest_currency", Rule::Text("interest_currency")),
            ("is_read", Rule::Bool("is_read")),
            ("quantity_currency", Rule::Text("quantity_currency")),
            ("trading_date", Rule::Timestamp("traded_at")),
            ("trading_price", Rule::Decimal("trading_price")),
            ("trading_currency", Rule::Text("trading_currency")),
        ],
        "tax_exemption" => &[
            ("valid_from", Rule::Timestamp("valid_from")),
            ("valid_until", Rule::Timestamp("valid_until")),
            ("amount", Rule::Decimal("amount")),
            ("used_amount", Rule::Decimal("used_amount")),
            ("remaining_amount", Rule::Decimal("remaining_amount")),
        ],
        "booking" => &[
            (
                "scheme_type",
                Rule::Not("always none for a booking: no column"),
            ),
            ("debit_flag", Rule::Bool("debit")),
            ("currency", Rule::Text("stored_currency")),
            (
                "scheduled_date",
                Rule::Not("a field of payments; unused in a booking"),
            ),
            (
                "scheduled_time",
                Rule::Not("a field of payments; unused in a booking"),
            ),
            (
                "cost_bearer_account_oid",
                Rule::Not("a field of payments; unused in a booking"),
            ),
            ("batch_oid", Rule::Ref("master_id")),
            ("batch_expanded", MEMORY_ONLY),
            ("batch_index", MEMORY_ONLY),
            ("booking_date", Rule::Date("booking_date")),
            ("value_date", Rule::Date("value_date")),
            ("transaction_date", Rule::DateAtMidnight("transaction_date")),
            (
                "transaction_code",
                Rule::Code("transaction_code", TRANSACTION_CODES),
            ),
            ("booking_text", Rule::Text("booking_text")),
            ("primanota", Rule::Text("primanota")),
            ("bank_reference", Rule::Text("bank_reference")),
            ("gvc", Rule::Int("gvc")),
            ("gvc_extension", Rule::Int("gvc_extension")),
            (
                "swift_transaction_code",
                Rule::Text("swift_transaction_code"),
            ),
            ("instructed_amount", Rule::Decimal("instructed_amount")),
            ("instructed_currency", Rule::Text("instructed_currency")),
            ("charges_amount", Rule::Decimal("charges_amount")),
            ("charges_currency", Rule::Text("charges_currency")),
            ("debit_return_order_id", Rule::Text("debit_return_order_id")),
            ("debit_return_date", Rule::Date("debit_return_date")),
            ("debit_return_time", Rule::Time("debit_return_time")),
            ("virtual_iban", Rule::Text("virtual_iban")),
            ("entry_bits", Rule::Flags("entry_bits", ENTRY_BITS)),
            ("entry_bits", Rule::Bit("is_read", 0x1)),
            ("booking_status", Rule::Code("status", BOOKING_STATUS)),
            ("flag", Rule::Code("flag", COLOURS)),
            ("split", Rule::Flags("split_role", SPLIT_ROLES)),
            ("notes", Rule::Text("notes")),
            ("analysis_date", Rule::DateAtMidnight("analysis_date")),
            ("vat_amount", Rule::Decimal("vat_amount")),
            (
                "vat_rate_type",
                Rule::Flags("vat_rate_type", VAT_RATE_TYPES),
            ),
            ("running_balance", MEMORY_ONLY),
            ("day_balance", MEMORY_ONLY),
            ("cross_entry_oid", MEMORY_ONLY),
        ],
        "payment" => &[
            ("scheme_type", Rule::Code("scheme", SCHEMES)),
            ("debit_flag", Rule::Bool("stored_debit_flag")),
            ("currency", Rule::Text("stored_currency")),
            ("scheduled_date", Rule::Date("scheduled_date")),
            ("scheduled_time", Rule::Time("scheduled_time")),
            (
                "cost_bearer_account_oid",
                Rule::Ref("cost_bearer_account_id"),
            ),
            ("batch_oid", Rule::Ref("master_id")),
            ("batch_expanded", MEMORY_ONLY),
            ("batch_index", MEMORY_ONLY),
            ("state", Rule::Flags("state", PAYMENT_STATES)),
            ("flags", Rule::Flags("flags", PAYMENT_FLAGS)),
            ("payment_reference", Rule::Text("payment_reference")),
            ("outbox_oid", Rule::Ref("outbox_id")),
            ("initiator_name", Rule::Text("initiator_name")),
            ("batch_booking", Rule::Code("batch_booking", BATCH_BOOKING)),
            ("color_flag", Rule::Code("flag", COLOURS)),
            ("import_id", Rule::Text("import_id")),
            ("reconciled_booking_oid", Rule::Ref("reconciled_booking_id")),
            (
                "payee_verification_json",
                Rule::Text("payee_verification_json"),
            ),
            ("sepa_status_code", Rule::Code("sepa_status", SEPA_STATUS)),
            ("sepa_reason_code", Rule::Text("sepa_reason_code")),
            ("sepa_additional_info", Rule::Text("sepa_additional_info")),
            (
                "payee_verification_choice",
                Rule::Code("payee_verification_choice", VERIFICATION_CHOICES),
            ),
            ("used_tan", ACCESS),
            ("order_reference_tan", ACCESS),
        ],
        "standing_order" => &[
            ("scheme_type", Rule::Code("scheme", SCHEMES)),
            ("debit_flag", Rule::Bool("stored_debit_flag")),
            ("currency", Rule::Text("stored_currency")),
            (
                "scheduled_date",
                Rule::Not("a field of payments; unused in a standing order"),
            ),
            (
                "scheduled_time",
                Rule::Not("a field of payments; unused in a standing order"),
            ),
            (
                "cost_bearer_account_oid",
                Rule::Not("a field of payments; unused in a standing order"),
            ),
            ("batch_oid", Rule::Not("a standing order has no batch")),
            ("batch_expanded", MEMORY_ONLY),
            ("batch_index", MEMORY_ONLY),
            ("state", Rule::Flags("state", ORDER_STATES)),
            ("state", Rule::Bit("is_local", 0x1)),
            ("outbox_oid", Rule::Ref("outbox_id")),
            ("next_execution_date", Rule::Date("next_execution_date")),
            ("first_execution_date", Rule::Date("first_execution_date")),
            ("period_unit", Rule::Code("period_unit", PERIOD_UNITS)),
            ("period_length", Rule::Int("period_length")),
            ("execution_day", Rule::Int("execution_day")),
            ("execution_time", Rule::Time("execution_time")),
            ("last_execution_date", Rule::Date("last_execution_date")),
        ],
        other => panic!("no rules for record type {other}"),
    };
    rules.extend_from_slice(own);
    if matches!(record_type, "booking" | "payment" | "standing_order") {
        rules.extend_from_slice(&PAYMENT_BASE);
    }
    rules
}

/// The expression that makes `column` comparable as text.
fn select(rule: Rule) -> Vec<(String, String)> {
    let plain = |column: &str| vec![(column.to_owned(), format!("{column}::varchar"))];
    match rule {
        Rule::Text(column)
        | Rule::Int(column)
        | Rule::Ref(column)
        | Rule::Bool(column)
        | Rule::Decimal(column)
        | Rule::Date(column)
        | Rule::DateAtMidnight(column)
        | Rule::Time(column)
        | Rule::Code(column, _)
        | Rule::Flags(column, _)
        | Rule::Bit(column, _)
        | Rule::Lines(column) => plain(column),
        Rule::Stamp(column) => vec![(
            column.to_owned(),
            format!("(cast(epoch({column}) as bigint) - {EPOCH_2000})::varchar"),
        )],
        Rule::Timestamp(column) => vec![(
            column.to_owned(),
            format!("strftime({column}, '%Y-%m-%dT%H:%M:%S.%f')"),
        )],
        Rule::SecurityId => [plain("isin"), plain("stored_isin")].concat(),
        Rule::Not(_) => Vec::new(),
    }
}

fn name_of(names: Names, number: i64) -> String {
    names
        .iter()
        .find(|(value, _)| *value == number)
        .map_or_else(
            || panic!("the test has no name for {number}"),
            |(_, name)| (*name).to_owned(),
        )
}

fn flags_of(names: Names, bits: i64) -> Option<String> {
    let known: i64 = names.iter().map(|(bit, _)| bit).sum();
    assert_eq!(
        bits & !known,
        0,
        "the test has no name for a bit of {bits:#x}"
    );
    let set: Vec<&str> = names
        .iter()
        .filter(|(bit, _)| bits & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    (!set.is_empty()).then(|| set.join(","))
}

/// What the column must hold for the manifest value `field` under `rule`.
fn expected(rule: Rule, field: &Value) -> Vec<(&'static str, Option<String>)> {
    let text = || field.as_str().map(str::to_owned);
    let number = || field.as_i64().expect("a number in the manifest");
    match rule {
        Rule::Text(column) | Rule::Date(column) | Rule::Time(column) => vec![(column, text())],
        Rule::Int(column) | Rule::Stamp(column) => vec![(column, Some(number().to_string()))],
        Rule::Ref(column) => {
            // -1 is how a standing order and a payment spell "no outbox order".
            let reference = Some(number()).filter(|oid| *oid != 0 && *oid != -1);
            vec![(column, reference.map(|oid| oid.to_string()))]
        }
        Rule::Bool(column) => vec![(column, Some(field.as_bool().unwrap().to_string()))],
        Rule::Decimal(column) => vec![(column, text().map(|value| decimal(&value)))],
        Rule::DateAtMidnight(column) => vec![(
            column,
            text().map(|value| {
                let (date, time) = value.split_once('T').expect("a timestamp in the manifest");
                assert_eq!(time, "00:00:00.0000000");
                date.to_owned()
            }),
        )],
        Rule::Timestamp(column) => vec![(
            column,
            // Seven decimals in the manifest, six in the database.
            text().map(|value| {
                assert!(value.ends_with('0'), "{value} has a seventh decimal");
                value[..value.len() - 1].to_owned()
            }),
        )],
        Rule::Code(column, names) => vec![(column, Some(name_of(names, number())))],
        Rule::Flags(column, names) => vec![(column, flags_of(names, number()))],
        Rule::Bit(column, bit) => vec![(column, Some((number() & bit != 0).to_string()))],
        Rule::Lines(column) => {
            let lines: Vec<&str> = field
                .as_array()
                .expect("an array of lines in the manifest")
                .iter()
                .map(|line| line.as_str().unwrap())
                .collect();
            vec![(column, (!lines.is_empty()).then(|| lines.join("\n")))]
        }
        Rule::SecurityId => vec![
            ("stored_isin", field["text"].as_str().map(str::to_owned)),
            ("isin", field["isin"].as_str().map(str::to_owned)),
        ],
        Rule::Not(_) => Vec::new(),
    }
}

/// The `<oid>` of `Acct<oid>/...` or `Stock<oid>/...`.
fn owner_of(path: &str) -> Option<i64> {
    let (directory, _) = path.split_once('/')?;
    let digits = directory.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    Some(digits.parse().expect("an id in the directory name"))
}

#[test]
fn the_sample_records_of_the_manifest_are_in_the_database_field_by_field() {
    let work = Work::decrypted();
    let manifest = manifest();
    let samples = manifest["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 31);

    let mut seen_types = BTreeSet::new();
    let mut compared = 0;
    let mut not_compared: BTreeMap<&'static str, usize> = BTreeMap::new();
    for sample in samples {
        let record_type = sample["record_type"].as_str().unwrap();
        let path = sample["path"].as_str().unwrap();
        let what = format!("{path} #{} ({})", sample["index"], sample["about"]);
        seen_types.insert(record_type.to_owned());
        let table = table_of(record_type);
        let fields = sample["fields"].as_object().unwrap();
        let derived = sample.get("derived").and_then(Value::as_object);
        let rules = rules(record_type);

        // Every field of the sample is accounted for by a rule.
        for field in fields.keys() {
            assert!(
                rules.iter().any(|(name, _)| name == field),
                "{what}: no rule for field {field}"
            );
        }
        for (field, _) in &rules {
            assert!(
                fields.contains_key(*field),
                "{what}: the sample has no field {field}"
            );
        }
        if let Some(derived) = derived {
            for field in derived.keys() {
                assert!(
                    DERIVED.iter().any(|(name, _)| name == field),
                    "{what}: no rule for derived value {field}"
                );
            }
        }

        let mut pairs: Vec<(Rule, &Value, String)> = rules
            .iter()
            .map(|(field, rule)| (*rule, &fields[*field], (*field).to_owned()))
            .collect();
        if let Some(derived) = derived {
            for (field, rule) in DERIVED {
                if let Some(value) = derived.get(field) {
                    pairs.push((rule, value, format!("derived.{field}")));
                }
            }
        }

        let mut columns: BTreeMap<String, String> = BTreeMap::new();
        for (rule, _, _) in &pairs {
            columns.extend(select(*rule));
        }
        let list: Vec<String> = columns
            .iter()
            .map(|(column, expression)| format!("{expression} as {column}"))
            .collect();
        let id = fields["oid"].as_i64().unwrap();
        let owner = match (owner_of(path), table) {
            (Some(owner), "stock_prices") => format!(" and instrument_id = {owner}"),
            (Some(owner), _) => format!(" and account_id = {owner}"),
            (None, _) => String::new(),
        };
        let row = work.row(&format!(
            "select id, {} from {table} where id = {id}{owner}",
            list.join(", ")
        ));

        for (rule, value, field) in pairs {
            if let Rule::Not(reason) = rule {
                *not_compared.entry(reason).or_default() += 1;
                continue;
            }
            for (column, wanted) in expected(rule, value) {
                let mut found = text_of(&row[column]);
                if matches!(rule, Rule::Decimal(_)) {
                    found = found.map(|text| decimal(&text));
                }
                assert_eq!(found, wanted, "{what}: {field} in {table}.{column}");
                compared += 1;
            }
        }

        // What the loader adds from the directory and the account.
        if let Some(currency) = sample.get("account_currency") {
            if matches!(record_type, "booking" | "payment" | "standing_order") {
                let stored = work.value(&format!(
                    "select currency as v from {table} where id = {id}{owner}"
                ));
                assert_eq!(&stored, currency, "{what}: currency of the account");
            }
        }
    }
    assert_eq!(
        seen_types.len(),
        TABLES.len(),
        "a sample of every record type: {seen_types:?}"
    );
    assert!(compared > 1000, "{compared} values compared");

    // Nothing is left out without one of these reasons.
    let reasons: Vec<&str> = not_compared.keys().copied().collect();
    for reason in &reasons {
        assert!(not_compared[reason] > 0);
    }
    assert!(reasons.contains(&"bank access field: dropped when the record is decoded"));
}

#[test]
fn bank_access_fields_of_the_samples_are_nowhere_in_the_database() {
    let work = Work::decrypted();
    let manifest = manifest();
    let mut secrets = Vec::new();
    for sample in manifest["samples"].as_array().unwrap() {
        for (field, value) in sample["fields"].as_object().unwrap() {
            let is_access = [
                "fints_customer_id",
                "last_download_token",
                "contact_guid",
                "order_contact_guid",
                "contact_name",
                "order_contact_name",
                "fints_account_sub_id",
                "used_tan",
                "order_reference_tan",
            ]
            .contains(&field.as_str());
            if let (true, Some(text)) = (is_access, value.as_str()) {
                if text != "00000000-0000-0000-0000-000000000000" {
                    secrets.push(text.to_owned());
                }
            }
        }
    }
    // The demo accounts do carry such values, so the check is not vacuous.
    assert!(secrets.len() >= 4, "{secrets:?}");

    let bytes = fs::read(work.db()).unwrap();
    for secret in &secrets {
        assert!(
            !contains(&bytes, secret.as_bytes()),
            "a bank access value is in the database file"
        );
    }
    let needles: Vec<&str> = secrets.iter().map(String::as_str).collect();
    assert_eq!(
        rows_holding(&work, &needles),
        0,
        "a bank access value is in a column"
    );
    // The search itself works: a value that is exported is found.
    assert!(rows_holding(&work, &["Girokonto"]) > 0);
}

/// How many rows of any table hold one of `needles` in any text column,
/// whatever the file's compression does to the bytes.
fn rows_holding(work: &Work, needles: &[&str]) -> i64 {
    let text_columns = work.sql(
        "select table_name, list(column_name) as columns from duckdb_columns() \
         where schema_name = 'main' and data_type = 'VARCHAR' and not internal \
           and table_name in (select table_name from duckdb_tables() where not internal) \
         group by table_name",
    );
    // All but the two tables that hold only numbers and dates.
    assert_eq!(text_columns.len(), 16);
    let mut hits = 0;
    for table in &text_columns {
        let name = table["table_name"].as_str().unwrap();
        // A list comes out as JSON text.
        let columns: Vec<String> = match &table["columns"] {
            Value::String(text) => serde_json::from_str(text).unwrap(),
            other => serde_json::from_value(other.clone()).unwrap(),
        };
        let tests: Vec<String> = columns
            .iter()
            .flat_map(|column| {
                needles.iter().map(move |needle| {
                    format!("contains(\"{column}\", '{}')", needle.replace('\'', "''"))
                })
            })
            .collect();
        hits += work.count(&format!(
            "select count(*) as v from \"{name}\" where {}",
            tests.join(" or ")
        ));
    }
    hits
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

// --- what is not read ---

#[test]
fn unknown_entries_warn_once_each_and_bank_access_entries_are_only_counted() {
    let work = Work::new();
    let output = work.decrypt(&[]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let summary = stderr(&output);
    let manifest = manifest();
    let entries = manifest["entries"].as_array().unwrap();

    // The two entries the manifest marks with `b4_warns`, one warning each.
    let warned: Vec<&str> = entries
        .iter()
        .filter(|entry| entry["b4_warns"] == Value::Bool(true))
        .map(|entry| entry["path"].as_str().unwrap())
        .collect();
    assert_eq!(warned, ["Acct757073100/FutureThing", "FutureCollection"]);
    let warnings: Vec<&str> = summary
        .lines()
        .filter_map(|line| line.strip_prefix("warning: "))
        .collect();
    assert_eq!(
        warnings,
        [
            "unknown entry \"FutureCollection\" in the vault (300 bytes): skipped without reading it",
            "unknown entry \"FutureThing\" in 1 account directory (first: Acct757073100): skipped \
             without reading it",
        ]
    );
    let stored: Vec<String> = work
        .sql("select message from warnings order by rowid")
        .iter()
        .map(|row| row["message"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(stored, warnings);

    // The entries of the root directory that are never read: the manifest's
    // class of them.
    let never_read = entries
        .iter()
        .filter(|entry| {
            !entry["path"].as_str().unwrap().contains('/')
                && entry["class"].as_str().unwrap() == "bank_access"
        })
        .count();
    assert_eq!(never_read, 9);
    assert!(
        summary.contains(&format!("bank access entries left unread: {never_read}\n")),
        "{summary}"
    );
    assert_eq!(
        work.value("select value as v from meta where key = 'entries_never_read'"),
        never_read.to_string()
    );
}

#[test]
fn the_decoy_marker_is_nowhere() {
    let work = Work::new();
    let output = work.decrypt(&[]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let manifest = manifest();
    let marker = manifest["decoy_marker"].as_str().unwrap();
    assert_eq!(marker, "B4-DECOY-MUST-NOT-BE-READ");

    // The vault holds it encrypted only, so finding it would mean b4
    // decrypted an entry it must not read.
    assert!(!contains(
        &fs::read(work.vault()).unwrap(),
        marker.as_bytes()
    ));
    assert!(!contains(&fs::read(work.db()).unwrap(), marker.as_bytes()));
    assert!(!stderr(&output).contains(marker));
    assert!(!stdout(&output).contains(marker));

    assert_eq!(rows_holding(&work, &["B4-DECOY"]), 0);
}

// --- the file ---

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn the_database_is_private_and_nothing_else_is_left_behind() {
    let work = Work::decrypted();
    assert_eq!(work.files(), ["b4.duckdb", "demo.sub"]);
    #[cfg(unix)]
    assert_eq!(mode_of(&work.db()), 0o600);
    // Reading leaves nothing either.
    work.sql("select count(*) from bookings");
    assert_eq!(work.files(), ["b4.duckdb", "demo.sub"]);
}

#[test]
fn a_wrong_password_exits_1_and_leaves_an_existing_database_untouched() {
    let work = Work::decrypted();
    let before = fs::read(work.db()).unwrap();
    let modified = fs::metadata(work.db()).unwrap().modified().unwrap();

    let output = work.decrypt_to(&work.db(), "not the password", &[]);
    assert_eq!(exit_code(&output), 1);
    assert_eq!(stdout(&output), "");
    let message = stderr(&output);
    assert!(message.starts_with("error: "), "{message}");
    assert!(message.contains("wrong password"), "{message}");
    assert_eq!(message.lines().count(), 1, "{message}");

    assert_eq!(fs::read(work.db()).unwrap(), before);
    assert_eq!(
        fs::metadata(work.db()).unwrap().modified().unwrap(),
        modified
    );
    assert_eq!(work.files(), ["b4.duckdb", "demo.sub"]);

    // Without a database there is none afterwards.
    let fresh = Work::new();
    let output = fresh.decrypt_to(&fresh.db(), "", &[]);
    assert_eq!(exit_code(&output), 1);
    assert_eq!(fresh.files(), ["demo.sub"]);
}

#[test]
fn decrypting_again_replaces_the_database_as_a_whole() {
    let work = Work::new();
    // A database of another shape is at the target.
    {
        let connection = duckdb::Connection::open(work.db()).unwrap();
        connection
            .execute_batch("create table leftover (x integer); insert into leftover values (1)")
            .unwrap();
    }
    #[cfg(unix)]
    let inode_before = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(work.db()).unwrap().ino()
    };

    let output = work.decrypt(&[]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(work.files(), ["b4.duckdb", "demo.sub"]);
    assert_eq!(work.count("select count(*) as v from accounts"), 6);
    assert_eq!(
        work.count("select count(*) as v from duckdb_tables() where table_name = 'leftover'"),
        0
    );
    // Renamed into place, not written in place: it is another file.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_ne!(fs::metadata(work.db()).unwrap().ino(), inode_before);
        assert_eq!(mode_of(&work.db()), 0o600);
    }

    // And once more over its own output, with a write-ahead log that some
    // other program left beside it.
    let first = work.value("select value as v from meta where key = 'vault_checksum'");
    fs::write(work.path("b4.duckdb.wal"), b"not ours").unwrap();
    let output = work.decrypt(&[]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(work.files(), ["b4.duckdb", "demo.sub"]);
    assert_eq!(work.count("select count(*) as v from bookings"), 634);
    assert_eq!(
        work.value("select value as v from meta where key = 'vault_checksum'"),
        first
    );
}

/// `b4 decrypt <vault> <target> --password-stdin` with nothing on standard
/// input: what is refused here is refused before the password is read.
fn decrypt_without_password(vault: &Path, target: &Path) -> Output {
    b4(&[
        "decrypt",
        vault.to_str().unwrap(),
        target.to_str().unwrap(),
        "--password-stdin",
    ])
    .output()
    .unwrap()
}

#[test]
fn a_directory_with_the_name_of_the_log_is_refused_and_keeps_what_is_in_it() {
    // mkdir out.duckdb.wal; cp demo.sub out.duckdb.wal/;
    // b4 decrypt out.duckdb.wal/demo.sub out.duckdb
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("out.duckdb.wal");
    fs::create_dir(&log).unwrap();
    let vault = log.join("demo.sub");
    fs::copy(DEMO_VAULT, &vault).unwrap();
    let target = dir.path().join("out.duckdb");

    let refused = |output: &Output| {
        assert_eq!(exit_code(output), 1, "{}", stderr(output));
        assert_eq!(stdout(output), "");
        let message = stderr(output);
        assert!(
            message.starts_with(&format!(
                "error: {} is a directory, where the write-ahead log of a database at {} \
                 belongs: b4 will not replace a database while that entry is in the way",
                log.display(),
                target.display()
            )),
            "{message}"
        );
        assert!(!message.to_lowercase().contains("password"), "{message}");
        assert_eq!(file_names(&log), ["demo.sub"]);
        assert_eq!(fs::read(&vault).unwrap(), fs::read(DEMO_VAULT).unwrap());
    };

    // Refused before the password is read, and with the password at hand.
    let output = decrypt_without_password(&vault, &target);
    refused(&output);
    assert_eq!(file_names(dir.path()), ["out.duckdb.wal"]);
    let args = [
        "decrypt",
        vault.to_str().unwrap(),
        target.to_str().unwrap(),
        "--password-stdin",
    ];
    let output = run_with_stdin(b4(&args), &format!("{PASSWORD}\n"));
    refused(&output);
    assert_eq!(file_names(dir.path()), ["out.duckdb.wal"]);

    // The same next to a database that is there: it is not replaced.
    // (Made elsewhere: DuckDB cannot write one next to that directory.)
    let elsewhere = tempfile::tempdir().unwrap();
    {
        let connection = duckdb::Connection::open(elsewhere.path().join("old.duckdb")).unwrap();
        connection
            .execute_batch("create table old (x integer)")
            .unwrap();
    }
    fs::copy(elsewhere.path().join("old.duckdb"), &target).unwrap();
    let before = fs::read(&target).unwrap();
    let output = run_with_stdin(b4(&args), &format!("{PASSWORD}\n"));
    refused(&output);
    assert_eq!(file_names(dir.path()), ["out.duckdb", "out.duckdb.wal"]);
    assert_eq!(fs::read(&target).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn a_link_with_the_name_of_the_log_is_refused_and_what_it_leads_to_is_untouched() {
    use std::os::unix::fs::symlink;

    let work = Work::new();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("notes.txt");
    fs::write(&file, "my notes\n").unwrap();
    let folder = outside.path().join("folder");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("inside.txt"), "inside\n").unwrap();
    let log = work.path("b4.duckdb.wal");

    for leads_to in [&file, &folder] {
        symlink(leads_to, &log).unwrap();
        for output in [
            decrypt_without_password(&work.vault(), &work.db()),
            work.decrypt(&[]),
        ] {
            assert_eq!(exit_code(&output), 1, "{}", stderr(&output));
            assert_eq!(stdout(&output), "");
            let message = stderr(&output);
            assert!(
                message.starts_with(&format!(
                    "error: {} is a symbolic link, where the write-ahead log of a database at \
                     {} belongs: b4 will not replace a database while that entry is in the way",
                    log.display(),
                    work.db().display()
                )),
                "{message}"
            );
            assert_eq!(work.files(), ["b4.duckdb.wal", "demo.sub"]);
            assert_eq!(&fs::read_link(&log).unwrap(), leads_to);
            assert_eq!(fs::read_to_string(&file).unwrap(), "my notes\n");
            assert_eq!(file_names(&folder), ["inside.txt"]);
            assert_eq!(
                fs::read_to_string(folder.join("inside.txt")).unwrap(),
                "inside\n"
            );
            assert_eq!(file_names(outside.path()), ["folder", "notes.txt"]);
        }
        fs::remove_file(&log).unwrap();
    }

    // With the link gone, the same command writes the database.
    let output = work.decrypt(&[]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(work.files(), ["b4.duckdb", "demo.sub"]);
}

#[test]
fn a_target_that_is_not_a_database_is_refused_before_the_password_is_read() {
    let work = Work::new();
    let target = work.path("notes.txt");
    fs::write(&target, "my notes\n").unwrap();
    // No password is on standard input: the refusal comes first.
    let vault = work.vault();
    let output = b4(&[
        "decrypt",
        vault.to_str().unwrap(),
        target.to_str().unwrap(),
        "--password-stdin",
    ])
    .output()
    .unwrap();
    assert_eq!(exit_code(&output), 1);
    let message = stderr(&output);
    assert!(
        message.contains("notes.txt exists and is not a DuckDB database: refusing to replace it"),
        "{message}"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "my notes\n");
    assert_eq!(work.files(), ["demo.sub", "notes.txt"]);

    // The vault itself, directly and through a link.
    let before = fs::read(&vault).unwrap();
    let link = work.path("link.duckdb");
    fs::hard_link(&vault, &link).unwrap();
    for target in [&vault, &link] {
        let output = work.decrypt_to(target, PASSWORD, &[]);
        assert_eq!(exit_code(&output), 1);
        assert!(
            stderr(&output).contains("is the vault itself"),
            "{}",
            stderr(&output)
        );
        assert_eq!(fs::read(&vault).unwrap(), before);
    }

    // A directory.
    let directory = work.path("dir");
    fs::create_dir(&directory).unwrap();
    let output = work.decrypt_to(&directory, PASSWORD, &[]);
    assert_eq!(exit_code(&output), 1);
    assert!(
        stderr(&output).contains("refusing to replace it"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_password_can_come_from_the_environment() {
    let work = Work::new();
    let vault = work.vault();
    let db = work.db();
    let output = b4(&["decrypt", vault.to_str().unwrap(), db.to_str().unwrap()])
        .env("B4_PASSWORD", PASSWORD)
        .output()
        .unwrap();
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(work.count("select count(*) as v from bookings"), 634);

    // The database of `--db` is the default target.
    let other = Work::new();
    let vault = other.vault();
    let output = b4(&[
        "decrypt",
        vault.to_str().unwrap(),
        "--db",
        other.db().to_str().unwrap(),
    ])
    .env("B4_PASSWORD", PASSWORD)
    .output()
    .unwrap();
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert!(
        stderr(&output).ends_with("try: b4 accounts list\n"),
        "{}",
        stderr(&output)
    );
    assert_eq!(other.files(), ["b4.duckdb", "demo.sub"]);

    // A wrong one in the environment fails like any other.
    let output = b4(&["decrypt", vault.to_str().unwrap(), db.to_str().unwrap()])
        .env("B4_PASSWORD", "nope")
        .output()
        .unwrap();
    assert_eq!(exit_code(&output), 1);
    assert!(stderr(&output).contains("wrong password"));
}

#[test]
fn quiet_leaves_only_the_warnings() {
    let work = Work::new();
    let output = work.decrypt(&["--quiet"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert_eq!(stdout(&output), "");
    let lines: Vec<String> = stderr(&output).lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines
            .iter()
            .all(|line| line.starts_with("warning: unknown ")),
        "{lines:?}"
    );

    // Without it: tables, the count, the warnings, then where the database is.
    let output = work.decrypt(&[]);
    let summary = stderr(&output);
    let lines: Vec<&str> = summary.lines().collect();
    assert_eq!(lines.len(), 16 + 1 + 2 + 2, "{summary}");
    assert!(lines[0].starts_with("accounts "), "{summary}");
    assert_eq!(lines[16], "bank access entries left unread: 9");
    assert!(lines[17].starts_with("warning: "));
    assert_eq!(lines[19], format!("database: {}", work.db().display()));
    assert_eq!(
        lines[20],
        format!("try: b4 --db {} accounts list", work.db().display())
    );
}

// --- afterwards ---

#[test]
fn status_is_up_to_date_and_schema_describes_every_table() {
    let work = Work::decrypted();
    let output = work.run(&["status"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let status = stdout(&output);
    assert!(status.contains("up to date"), "{status}");
    assert!(status.contains(work.vault().to_str().unwrap()), "{status}");
    let output = work.run(&["status", "--check"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));

    // What status compares is what the header and the file say.
    let meta: BTreeMap<String, String> = work
        .sql("select key, value from meta")
        .iter()
        .map(|row| {
            (
                row["key"].as_str().unwrap().to_owned(),
                row["value"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let manifest = manifest();
    let unique = &manifest["vault"]["not_reproducible"];
    assert_eq!(meta["schema_version"], "1");
    assert_eq!(meta["b4_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(meta["vault_path"], work.vault().to_str().unwrap());
    assert_eq!(meta["vault_id"], unique["file_guid"].as_str().unwrap());
    assert_eq!(
        meta["vault_modified_stamp"],
        unique["modified_at"].to_string()
    );
    assert_eq!(meta["vault_size"], unique["file_size"].to_string());
    assert_eq!(meta["vault_checksum"].len(), 16);
    assert!(meta["vault_created"].ends_with('Z') && meta["vault_modified"].ends_with('Z'));
    assert!(meta["decrypted_at"].ends_with('Z'));
    assert_eq!(meta["entries_never_read"], "9");
    assert_eq!(meta.len(), 11);

    // A vault that changed is noticed.
    let mut bytes = fs::read(work.vault()).unwrap();
    bytes.extend_from_slice(&[0; 512]);
    fs::write(work.vault(), bytes).unwrap();
    let output = work.run(&["status", "--check"]);
    assert_eq!(exit_code(&output), 1);

    let output = work.run(&["schema", "--output", "json"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let relations: Vec<Value> = serde_json::from_str(&stdout(&output)).unwrap();
    let names: Vec<&str> = relations
        .iter()
        .map(|relation| relation["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "account_balances",
            "account_groups",
            "accounts",
            "assets",
            "bookings",
            "bookings_view",
            "categories",
            "exchange_rates",
            "mandates",
            "meta",
            "monthly_category_sums",
            "payees",
            "payers",
            "payments",
            "portfolio_items",
            "portfolio_transactions",
            "standing_orders",
            "stock_instruments",
            "stock_prices",
            "tax_exemptions",
            "warnings",
        ]
    );
    for relation in &relations {
        let name = relation["name"].as_str().unwrap();
        assert!(
            relation["description"]
                .as_str()
                .is_some_and(|text| text.len() > 20),
            "{name} has no description"
        );
        let output = work.run(&["schema", name, "--output", "json"]);
        assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
        let columns: Vec<Value> = serde_json::from_str(&stdout(&output)).unwrap();
        assert!(!columns.is_empty(), "{name}");
        for column in &columns {
            assert!(
                column["description"]
                    .as_str()
                    .is_some_and(|text| text.len() > 10),
                "{name}.{} has no description",
                column["name"]
            );
        }
    }
}

#[test]
fn no_command_writes_to_the_vault() {
    let work = Work::new();
    let vault = work.vault();
    // An old modification time, so that a write could not go unnoticed
    // within the resolution of the clock.
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    fs::File::options()
        .write(true)
        .open(&vault)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let before = fs::read(&vault).unwrap();

    let output = work.decrypt(&[]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let export = work.path("export");
    let vault_text = vault.to_str().unwrap();
    for args in [
        vec!["info", vault_text],
        vec!["status"],
        vec!["status", "--check"],
        vec!["schema"],
        vec!["sql", "select count(*) from bookings_view"],
        vec!["export", export.to_str().unwrap()],
    ] {
        let output = work.run(&args);
        assert_eq!(exit_code(&output), 0, "{args:?}: {}", stderr(&output));
    }
    // Failing decrypts do not touch it either.
    let output = work.decrypt_to(&work.db(), "wrong", &[]);
    assert_eq!(exit_code(&output), 1);

    assert_eq!(fs::read(&vault).unwrap(), before);
    assert_eq!(fs::metadata(&vault).unwrap().modified().unwrap(), old);
    // No lock file, no journal, nothing beside it.
    assert_eq!(work.files(), ["b4.duckdb", "demo.sub", "export"]);
}

// --- vaults that cannot be decrypted ---

/// Decrypt the vault image `bytes` and return exit status and message.
fn decrypt_image(bytes: &[u8]) -> (i32, String, Vec<String>) {
    let work = Work::new();
    fs::write(work.vault(), bytes).unwrap();
    let output = work.decrypt(&[]);
    assert_eq!(stdout(&output), "");
    (exit_code(&output), stderr(&output), work.files())
}

#[test]
fn a_file_that_is_no_vault_is_refused_clearly() {
    let demo = fs::read(DEMO_VAULT).unwrap();

    let (code, message, files) = decrypt_image(b"this is not a vault at all, just some text");
    assert_eq!(code, 1);
    assert!(message.starts_with("error: cannot decrypt "), "{message}");
    assert!(message.contains("not a Banking4 vault"), "{message}");
    assert_eq!(files, ["demo.sub"]);

    // Cut off in the middle of a cluster.
    let (code, message, files) = decrypt_image(&demo[..demo.len() / 2 + 100]);
    assert_eq!(code, 1);
    assert!(message.contains("truncated or damaged file"), "{message}");
    assert_eq!(files, ["demo.sub"]);

    // Bound to a device: bit 0 of the security flags (01 §3.6). Known from
    // the header, so no password is asked for.
    let mut bound = demo.clone();
    bound[0x010] |= 1;
    let work = Work::new();
    fs::write(work.vault(), &bound).unwrap();
    let vault = work.vault();
    let output = b4(&[
        "decrypt",
        vault.to_str().unwrap(),
        work.db().to_str().unwrap(),
    ])
    .output()
    .unwrap();
    assert_eq!(exit_code(&output), 1);
    assert!(
        stderr(&output).contains("bound to a device"),
        "{}",
        stderr(&output)
    );
    assert_eq!(work.files(), ["demo.sub"]);

    // A vault that is not there.
    let output = b4(&[
        "decrypt",
        "/nonexistent/b4-test/none.sub",
        work.db().to_str().unwrap(),
    ])
    .output()
    .unwrap();
    assert_eq!(exit_code(&output), 1);
    assert!(
        stderr(&output)
            .starts_with("error: cannot read the vault file /nonexistent/b4-test/none.sub"),
        "{}",
        stderr(&output)
    );
}

/// The offset of the directory slot of the entry called `name` (01 §7.1): 64
/// bytes on a 64-byte boundary, first cluster (4), size (4), flags (1), name
/// length (1), name (54, zero-filled).
fn slot_of(image: &[u8], name: &str) -> usize {
    let found: Vec<usize> = (0..image.len() / 64)
        .map(|slot| slot * 64)
        .filter(|&at| {
            let slot = &image[at..at + 64];
            slot[..4] != [0; 4]
                && usize::from(slot[9]) == name.len()
                && &slot[10..10 + name.len()] == name.as_bytes()
                && slot[10 + name.len()..].iter().all(|byte| *byte == 0)
        })
        .collect();
    assert_eq!(found.len(), 1, "one slot named {name}");
    found[0]
}

#[test]
fn a_damaged_vault_fails_with_the_hint_that_banking4_may_have_it_open() {
    let demo = fs::read(DEMO_VAULT).unwrap();
    // The chain of a collection b4 decodes leads out of the file.
    let mut image = demo.clone();
    let at = slot_of(&image, "Payees");
    image[at..at + 4].copy_from_slice(&0x00F0_0000_i32.to_le_bytes());
    let (code, message, files) = decrypt_image(&image);
    assert_eq!(code, 1);
    assert!(
        message.contains("cannot read the vault entry Payees"),
        "{message}"
    );
    assert!(message.contains("corrupt cluster chain"), "{message}");
    assert!(
        message.contains("the vault may be open in Banking4 right now"),
        "{message}"
    );
    assert_eq!(files, ["demo.sub"], "no half-written database");

    // The same damage in an entry b4 never reads changes nothing.
    for name in [
        "ModelSettings",
        "Contacts",
        "OutboxData",
        "FutureCollection",
    ] {
        let mut image = demo.clone();
        let at = slot_of(&image, name);
        image[at..at + 4].copy_from_slice(&0x00F0_0000_i32.to_le_bytes());
        let (code, message, files) = decrypt_image(&image);
        assert_eq!(code, 0, "{name}: {message}");
        assert_eq!(files, ["b4.duckdb", "demo.sub"], "{name}");
    }
}

#[test]
fn a_vault_whose_readable_entry_sits_on_bank_access_data_is_refused_as_manipulated() {
    // The slot of `AcctGroups`, a collection b4 decodes, is given the first
    // cluster, the size and the flags of `Contacts`, which it never reads:
    // nine bytes of a directory page, which is stored in the clear. Reading
    // by name alone would decrypt the contents of `Contacts`.
    let mut image = fs::read(DEMO_VAULT).unwrap();
    let (allowed, never_read) = (slot_of(&image, "AcctGroups"), slot_of(&image, "Contacts"));
    assert_eq!((allowed, never_read), (1024, 264_320));
    image.copy_within(never_read..never_read + 9, allowed);

    let (code, message, files) = decrypt_image(&image);
    assert_eq!(code, 1);
    assert!(message.starts_with("error: cannot decrypt "), "{message}");
    assert!(
        message.contains(
            "cannot read the vault entry AcctGroups: corrupt container: entry \"Contacts\" and \
             entry \"AcctGroups\" share cluster 528: the vault is damaged or was manipulated"
        ),
        "{message}"
    );
    assert!(!message.contains("B4-DECOY"), "{message}");
    // No database, and no working directory left behind.
    assert_eq!(files, ["demo.sub"]);

    // An existing database stays as it is.
    let work = Work::decrypted();
    let before = fs::read(work.db()).unwrap();
    fs::write(work.vault(), &image).unwrap();
    let output = work.decrypt(&[]);
    assert_eq!(exit_code(&output), 1);
    assert!(
        stderr(&output).contains("the vault is damaged or was manipulated"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read(work.db()).unwrap(), before);
    assert_eq!(work.files(), ["b4.duckdb", "demo.sub"]);
}
