//! Black-box tests of the query commands (`accounts`, `bookings`,
//! `standing-orders`, `portfolio`) on the demo vault of `tests/fixtures/vault`.
//!
//! The vault is decrypted once with `b4 decrypt`. What the commands print is
//! compared with the vault's manifest, which holds the expected result of
//! reading the vault: counts and exact sums per account and per category, the
//! split and the batch, the pending entries. Where the manifest has no number
//! for a filter (a date range, an amount range, a text), the expectation is
//! worked out here from the unfiltered list, whose totals were checked against
//! the manifest first.

// The helpers below are not `#[test]` functions, so clippy.toml's exemption
// for tests does not reach them.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::str::FromStr;
use std::sync::OnceLock;

use rust_decimal::Decimal;
use serde_json::Value;

const DEMO_VAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault/demo.sub");
const DEMO_PASSWORD: &str = "demo";
const MANIFEST: &str = include_str!("fixtures/vault/MANIFEST.json");

/// The fields of `bookings list` in the machine formats, as its help says.
const BOOKING_FIELDS: &str = "account_id,id,account_name,account_iban,booking_date,value_date,\
     status,pending,amount,currency,debit,payee_name,payee_iban,payee_bic,purpose,category,\
     booking_text,notes,counted,split_role,master_id,end_to_end_id,mandate_id,creditor_id";

const ACCOUNT_FIELDS: &str = "id,kind,name,iban,account_number,bic,bank_code,currency,balance,\
     balance_at,hidden,booking_count,first_booking_date,last_booking_date";

const SUMMARY_FIELDS: &str = "currency,booking_count,debits,credits,net";

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

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is not UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is not UTF-8")
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("b4 was killed by a signal")
}

/// The demo vault as a database, decrypted by the first test that asks.
///
/// It lives in cargo's directory for test files and not in a temporary
/// directory, which a `static` would never remove.
fn database() -> &'static Path {
    static DATABASE: OnceLock<PathBuf> = OnceLock::new();
    DATABASE.get_or_init(|| {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("queries-demo.duckdb");
        let mut child = b4(&["--quiet", "decrypt", "--password-stdin", DEMO_VAULT])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("cannot run b4");
        writeln!(child.stdin.take().unwrap(), "{DEMO_PASSWORD}").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "b4 decrypt failed: {}",
            stderr(&output)
        );
        path
    })
}

/// Run a command on the demo database.
fn run(args: &[&str]) -> Output {
    b4(args)
        .arg("--db")
        .arg(database())
        .output()
        .expect("cannot run b4")
}

/// The standard output of a command that must succeed without a word on
/// standard error.
fn ok(args: &[&str]) -> String {
    let output = run(args);
    assert_eq!(exit_code(&output), 0, "{args:?}: {}", stderr(&output));
    assert_eq!(stderr(&output), "", "{args:?}");
    stdout(&output)
}

/// The error message of a command that must fail with exit status 1 and
/// print nothing to standard output.
fn failure(args: &[&str]) -> String {
    let output = run(args);
    assert_eq!(exit_code(&output), 1, "{args:?}: {}", stdout(&output));
    assert_eq!(stdout(&output), "", "{args:?}");
    stderr(&output)
}

fn with<'a>(args: &[&'a str], more: &[&'a str]) -> Vec<&'a str> {
    args.iter().chain(more).copied().collect()
}

/// The rows of a command as `--output json` prints them.
fn json(args: &[&str]) -> Vec<Value> {
    let text = ok(&with(args, &["--output", "json"]));
    match serde_json::from_str(&text).expect("not JSON") {
        Value::Array(rows) => rows,
        other => panic!("{args:?} did not print an array: {other}"),
    }
}

/// The one object of a `show` command as `--output json` prints it.
fn json_object(args: &[&str]) -> Value {
    let text = ok(&with(args, &["--output", "json"]));
    let value: Value = serde_json::from_str(&text).expect("not JSON");
    assert!(value.is_object(), "{args:?} did not print an object");
    value
}

/// The rows of a command as `--output jsonl` prints them.
fn jsonl(args: &[&str]) -> Vec<Value> {
    ok(&with(args, &["--output", "jsonl"]))
        .lines()
        .map(|line| serde_json::from_str(line).expect("not JSON"))
        .collect()
}

/// Header and rows of a command as `--output csv` prints them.
fn csv(args: &[&str]) -> (Vec<String>, Vec<Vec<String>>) {
    let mut records = parse_csv(&ok(&with(args, &["--output", "csv"])));
    assert!(!records.is_empty(), "{args:?}: csv without a header");
    let header = records.remove(0);
    for record in &records {
        assert_eq!(record.len(), header.len(), "{args:?}: ragged csv");
    }
    (header, records)
}

/// RFC 4180: fields in quotes may hold commas, quotes (doubled) and line
/// breaks.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            (true, '"') => quoted = false,
            (true, c) => field.push(c),
            (false, '"') => quoted = true,
            (false, ',') => record.push(std::mem::take(&mut field)),
            (false, '\n') => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            (false, c) => field.push(c),
        }
    }
    assert!(!quoted, "csv ends inside a quoted field");
    assert!(
        field.is_empty() && record.is_empty(),
        "csv without a final line break"
    );
    records
}

/// The value of the CSV column `name` in `row`.
fn cell<'a>(header: &[String], row: &'a [String], name: &str) -> &'a str {
    let index = header
        .iter()
        .position(|column| column == name)
        .unwrap_or_else(|| panic!("no csv column {name}"));
    &row[index]
}

/// Check that the JSON object `line` has the comma-separated `fields` as its
/// keys, in that order. (The JSON parser of the tests sorts keys.)
fn assert_in_order(line: &str, fields: &str) {
    let mut from = 0;
    for field in fields.split(',') {
        let key = format!("\"{field}\":");
        let at = line[from..]
            .find(&key)
            .unwrap_or_else(|| panic!("{field} is missing or out of order in {line}"));
        from += at + key.len();
    }
    assert_eq!(
        line.matches("\":").count(),
        fields.split(',').count(),
        "{line}"
    );
}

// --- the manifest ---

fn manifest() -> &'static Value {
    static PARSED: OnceLock<Value> = OnceLock::new();
    PARSED.get_or_init(|| serde_json::from_str(MANIFEST).expect("the manifest is not JSON"))
}

fn accounts() -> &'static [Value] {
    manifest()["accounts"].as_array().unwrap()
}

fn account(name: &str) -> &'static Value {
    accounts()
        .iter()
        .find(|account| account["name"] == name)
        .unwrap_or_else(|| panic!("the manifest has no account {name}"))
}

/// A decimal of the manifest, where every amount is a string.
fn dec(value: &Value) -> Decimal {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("not a decimal: {value}"));
    Decimal::from_str(text).unwrap()
}

/// A decimal of b4's JSON output, where an amount is a number.
fn num(value: &Value) -> Decimal {
    assert!(value.is_number(), "not a number: {value}");
    Decimal::from_str(&value.to_string()).unwrap()
}

fn int(value: &Value) -> i64 {
    value
        .as_i64()
        .unwrap_or_else(|| panic!("not an integer: {value}"))
}

/// Count and sums of the bookings of a manifest account that count: booked
/// and pending, without the masters. Debits negative, as b4 prints them.
struct Sums {
    count: i64,
    credits: Decimal,
    debits: Decimal,
}

impl Sums {
    fn of(groups: &[&Value]) -> Sums {
        Sums {
            count: groups.iter().map(|group| int(&group["count"])).sum(),
            credits: groups.iter().map(|group| dec(&group["credit_sum"])).sum(),
            // Zero minus the sum: negating zero would give a negative zero.
            debits: Decimal::ZERO
                - groups
                    .iter()
                    .map(|group| dec(&group["debit_sum"]))
                    .sum::<Decimal>(),
        }
    }

    fn counted(account: &Value) -> Sums {
        Sums::of(&[&account["booked"], &account["pending"]])
    }

    fn net(&self) -> Decimal {
        self.credits + self.debits
    }
}

/// The unfiltered `bookings list` as JSON: the base of the expectations the
/// manifest has no number for. Its totals are checked against the manifest
/// in `the_list_has_every_booking_record_of_the_manifest`.
fn all_bookings() -> &'static [Value] {
    static ROWS: OnceLock<Vec<Value>> = OnceLock::new();
    ROWS.get_or_init(|| json(&["bookings", "list"]))
}

/// A test on a booking of the list.
type Keep<'a> = &'a dyn Fn(&Value) -> bool;

fn key(row: &Value) -> (i64, i64) {
    (int(&row["account_id"]), int(&row["id"]))
}

fn keys(rows: &[Value]) -> Vec<(i64, i64)> {
    rows.iter().map(key).collect()
}

/// The keys of the bookings of the unfiltered list that pass `keep`, in the
/// order of the list.
fn expected(keep: impl Fn(&Value) -> bool) -> Vec<(i64, i64)> {
    all_bookings()
        .iter()
        .filter(|row| keep(row))
        .map(key)
        .collect()
}

/// Whether the text field `name` of `row` contains `needle`, ignoring case.
fn has(row: &Value, name: &str, needle: &str) -> bool {
    row[name]
        .as_str()
        .is_some_and(|text| text.to_lowercase().contains(&needle.to_lowercase()))
}

// --- accounts ---

#[test]
fn accounts_list_matches_the_manifest_in_json_and_jsonl() {
    let visible: Vec<&Value> = accounts()
        .iter()
        .filter(|account| account["hidden"] == false)
        .collect();
    assert_eq!(visible.len(), 5);
    assert_eq!(accounts().len(), 6);

    for (args, wanted) in [
        (&["accounts", "list"][..], visible),
        (&["accounts", "list", "--all"], accounts().iter().collect()),
    ] {
        for rows in [json(args), jsonl(args)] {
            assert_eq!(rows.len(), wanted.len(), "{args:?}");
            // The manifest is in file order, which is the order of positions.
            for (row, account) in rows.iter().zip(&wanted) {
                let name = &account["name"];
                assert_eq!(
                    row.as_object().unwrap().len(),
                    ACCOUNT_FIELDS.split(',').count()
                );
                assert_eq!(row["id"], account["oid"], "{name}");
                assert_eq!(row["kind"], account["kind_name"], "{name}");
                assert_eq!(row["name"], *name);
                for field in [
                    "iban",
                    "account_number",
                    "bic",
                    "bank_code",
                    "currency",
                    "hidden",
                ] {
                    assert_eq!(row[field], account[field], "{name} {field}");
                }
                assert_eq!(
                    num(&row["balance"]),
                    dec(&account["statement_balance"]),
                    "{name}"
                );
                // Every record but the masters of the split and the batch.
                let counted = Sums::counted(account);
                assert_eq!(int(&row["booking_count"]), counted.count, "{name}");
                assert_eq!(
                    int(&row["booking_count"]),
                    int(&account["booking_records"]) - int(&account["masters"]["count"]),
                    "{name}"
                );
                assert_eq!(
                    row["first_booking_date"], account["first_booking_date"],
                    "{name}"
                );
                assert_eq!(
                    row["last_booking_date"], account["last_booking_date"],
                    "{name}"
                );
            }
        }
    }
}

#[test]
fn accounts_list_matches_the_manifest_in_csv_and_as_a_table() {
    let (header, rows) = csv(&["accounts", "list", "--all"]);
    assert_eq!(header.join(","), ACCOUNT_FIELDS);
    assert_eq!(rows.len(), 6);
    for (row, account) in rows.iter().zip(accounts()) {
        assert_eq!(cell(&header, row, "id"), account["oid"].to_string());
        assert_eq!(
            cell(&header, row, "name"),
            account["name"].as_str().unwrap()
        );
        assert_eq!(
            cell(&header, row, "iban"),
            account["iban"].as_str().unwrap_or("")
        );
        assert_eq!(
            Decimal::from_str(cell(&header, row, "balance")).unwrap(),
            dec(&account["statement_balance"])
        );
        assert_eq!(
            cell(&header, row, "booking_count"),
            Sums::counted(account).count.to_string()
        );
        assert_eq!(cell(&header, row, "hidden"), account["hidden"].to_string());
    }

    let table = ok(&["accounts", "list"]);
    let expected = r"       ID  TYPE         NAME         IBAN / NUMBER           BANK              BALANCE  AS OF       BOOKINGS
757073100  bank         Girokonto    DE31999900011000471100  MUBADEM0XXX  14733.44 EUR  2025-06-30       425
757073160  bank         Tagesgeld    DE09999900022000471122  BSPDDEB0XXX  18793.65 EUR  2025-06-30        26
757073220  credit_card  Kreditkarte  999900XXXXXX4711        99000000      -250.34 EUR  2025-06-25       123
757073280  portfolio    Depot        7000471133              99990002         0.00                         0
757073340  cash         Bargeld      757073340               00001000       391.74 EUR  2025-06-22        55
";
    assert_eq!(table, expected);
    // What the table says is what the manifest says.
    for account in accounts()
        .iter()
        .filter(|account| account["hidden"] == false)
    {
        let line = table
            .lines()
            .find(|line| line.starts_with(&account["oid"].to_string()))
            .unwrap();
        let balance = dec(&account["statement_balance"]).round_dp(2);
        assert!(line.contains(&format!(" {balance:.2}")), "{line}");
        assert!(
            line.ends_with(&format!(" {}", Sums::counted(account).count)),
            "{line}"
        );
    }

    let all = ok(&["accounts", "list", "--all"]);
    assert_eq!(all.lines().count(), 7);
    assert!(all.lines().next().unwrap().ends_with("  HIDDEN"), "{all}");
    let hidden: Vec<&str> = all
        .lines()
        .filter(|line| line.ends_with("  hidden"))
        .collect();
    assert_eq!(hidden.len(), 1);
    assert!(hidden[0].contains("Altes Sparbuch"), "{all}");

    let bare = ok(&["accounts", "list", "--no-header"]);
    assert_eq!(bare.lines().count(), 5);
    assert!(bare.starts_with("757073100  bank "), "{bare}");
}

#[test]
fn accounts_list_filters_by_type() {
    let names = |args: &[&str]| -> Vec<String> {
        json(args)
            .iter()
            .map(|row| row["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let of_kind = |kinds: &[&str], all: bool| -> Vec<String> {
        accounts()
            .iter()
            .filter(|account| kinds.contains(&account["kind_name"].as_str().unwrap()))
            .filter(|account| all || account["hidden"] == false)
            .map(|account| account["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        names(&["accounts", "list", "--type", "bank"]),
        of_kind(&["bank"], false)
    );
    assert_eq!(
        names(&["accounts", "list", "--type", "bank", "--all"]),
        of_kind(&["bank"], true)
    );
    assert_eq!(
        names(&[
            "accounts",
            "list",
            "--type",
            "credit-card",
            "--type",
            "cash"
        ]),
        of_kind(&["credit_card", "cash"], false)
    );
    assert_eq!(
        names(&["accounts", "list", "--type", "portfolio"]),
        ["Depot"]
    );
    assert!(names(&["accounts", "list", "--type", "paypal", "--all"]).is_empty());
}

#[test]
fn accounts_show_adds_up_what_the_manifest_adds_up() {
    for account in accounts() {
        let name = account["name"].as_str().unwrap();
        let id = account["oid"].to_string();
        let shown = json_object(&["accounts", "show", &id]);
        let counted = Sums::counted(account);

        assert_eq!(shown["id"], account["oid"], "{name}");
        assert_eq!(shown["name"], account["name"]);
        assert_eq!(shown["kind"], account["kind_name"], "{name}");
        assert_eq!(shown["holder_name"], account["holder_name"], "{name}");
        assert_eq!(shown["position"], account["position"], "{name}");
        assert_eq!(shown["hidden"], account["hidden"], "{name}");
        assert_eq!(
            num(&shown["balance"]),
            dec(&account["statement_balance"]),
            "{name}"
        );
        assert_eq!(int(&shown["booking_count"]), counted.count, "{name}");
        assert_eq!(
            shown["pending_count"], account["pending"]["count"],
            "{name}"
        );
        assert_eq!(
            shown["first_booking_date"], account["first_booking_date"],
            "{name}"
        );
        assert_eq!(
            shown["last_booking_date"], account["last_booking_date"],
            "{name}"
        );
        if counted.count == 0 {
            // Nothing to add up.
            for field in ["credits", "debits", "net"] {
                assert_eq!(shown[field], Value::Null, "{name} {field}");
            }
        } else {
            assert_eq!(num(&shown["credits"]), counted.credits, "{name}");
            assert_eq!(num(&shown["debits"]), counted.debits, "{name}");
            assert_eq!(num(&shown["net"]), counted.net(), "{name}");
        }
        // No access field of an account is ever shown.
        let object = shown.as_object().unwrap();
        for dropped in ["contact_name", "fints_customer_id", "last_download_token"] {
            assert!(!object.contains_key(dropped), "{dropped}");
        }

        // The other formats say the same.
        let line = jsonl(&["accounts", "show", &id]);
        assert_eq!(line, std::slice::from_ref(&shown), "{name}");
        let (header, rows) = csv(&["accounts", "show", &id]);
        assert_eq!(rows.len(), 1);
        assert_eq!(header.len(), object.len());
        assert_eq!(cell(&header, &rows[0], "name"), name);
        assert_eq!(
            cell(&header, &rows[0], "booking_count"),
            counted.count.to_string()
        );
    }

    let text = ok(&["accounts", "show", "giro"]);
    for line in [
        "ID:                       757073100\n",
        "Name:                     Girokonto\n",
        "IBAN:                     DE31999900011000471100\n",
        "Balance:                  14733.44 EUR\n",
        "Debit interest rate:      9.75\n",
        "Booking count:            425\n",
        "Pending count:            3\n",
        "First booking date:       2024-01-01\n",
        "Last booking date:        2025-06-30\n",
        "Credits:                  62939.55 EUR\n",
        "Debits:                   -48385.13 EUR\n",
        "Net:                      14554.42 EUR\n",
    ] {
        assert!(text.contains(line), "{line:?} is not in:\n{text}");
    }
    // People are spared the fields without a value.
    assert!(!text.contains("Creditor ID"), "{text}");
}

#[test]
fn an_account_is_named_by_id_iban_number_or_part_of_its_name() {
    let shown = |reference: &str| json_object(&["accounts", "show", reference])["name"].clone();
    for account in accounts() {
        let name = &account["name"];
        assert_eq!(shown(&account["oid"].to_string()), *name);
        if let Some(iban) = account["iban"].as_str() {
            assert_eq!(shown(iban), *name);
            // In groups of four, in lower case.
            let grouped: Vec<String> = iban
                .to_lowercase()
                .as_bytes()
                .chunks(4)
                .map(|chunk| String::from_utf8(chunk.to_vec()).unwrap())
                .collect();
            assert_eq!(shown(&grouped.join(" ")), *name);
        }
        let number = account["account_number"].as_str().unwrap();
        assert_eq!(shown(number), *name, "account number {number}");
        assert_eq!(shown(&name.as_str().unwrap().to_uppercase()), *name);
    }
    // The cash account's number is its own id, and a number that is no id
    // is an account number.
    assert_eq!(shown("757073340"), "Bargeld");
    assert_eq!(shown("7000471133"), "Depot");
    assert_eq!(shown("giro"), "Girokonto");
    assert_eq!(shown("sparbuch"), "Altes Sparbuch");
}

#[test]
fn an_ambiguous_account_lists_the_candidates_and_an_unknown_one_says_what_to_do() {
    // Every account whose name has an `e`, in the order of the accounts.
    let mut wanted = String::from("error: `e` matches several accounts:\n");
    for account in accounts() {
        let name = account["name"].as_str().unwrap();
        if name.to_lowercase().contains('e') {
            wanted.push_str(&format!("  {}  {name}\n", account["oid"]));
        }
    }
    wanted.push_str("name one of them by its id\n");
    assert_eq!(wanted.lines().count(), 7);

    let unknown = "error: no account matches `nowhere`: `b4 accounts list --all` lists the \
                   accounts with their ids\n";
    for command in [
        &["accounts", "show"][..],
        &["bookings", "list", "--account"],
        &["bookings", "summary", "--by", "month", "--account"],
        &["bookings", "show", "796370400", "--account"],
        &["standing-orders", "list", "--account"],
        &["portfolio", "list", "--account"],
    ] {
        assert_eq!(failure(&with(command, &["e"])), wanted, "{command:?}");
        assert_eq!(
            failure(&with(command, &["nowhere"])),
            unknown,
            "{command:?}"
        );
        // Also in a machine format: the error is on stderr, stdout is empty.
        assert_eq!(
            failure(&with(command, &["nowhere", "-o", "json"])),
            unknown,
            "{command:?}"
        );
    }
    // Wildcards and quotes are text.
    for reference in ["%", "_", "' or ''='"] {
        let message = failure(&["accounts", "show", reference]);
        assert!(
            message.starts_with("error: no account matches"),
            "{message}"
        );
    }
}

// --- bookings list ---

#[test]
fn the_list_has_every_booking_record_of_the_manifest() {
    let rows = all_bookings();
    assert_eq!(rows.len() as i64, int(&manifest()["totals"]["booking"]));
    assert_eq!(
        rows[0].as_object().unwrap().len(),
        BOOKING_FIELDS.split(',').count()
    );
    // The fields come in the documented order, in JSON as in CSV.
    let first = ok(&["bookings", "list", "--limit", "1", "-o", "jsonl"]);
    assert_in_order(&first, BOOKING_FIELDS);
    let first = ok(&["accounts", "list", "-o", "jsonl"]);
    assert_in_order(first.lines().next().unwrap(), ACCOUNT_FIELDS);

    for account in accounts() {
        let name = account["name"].as_str().unwrap();
        let own: Vec<&Value> = rows
            .iter()
            .filter(|row| row["account_id"] == account["oid"])
            .collect();
        assert_eq!(own.len() as i64, int(&account["booking_records"]), "{name}");
        for row in &own {
            assert_eq!(row["account_name"], account["name"]);
            assert_eq!(row["account_iban"], account["iban"]);
            assert_eq!(row["currency"], account["currency"]);
        }

        // The three groups of the manifest, by status and by role.
        let sums = |keep: &dyn Fn(&Value) -> bool| {
            let kept: Vec<&&Value> = own.iter().filter(|row| keep(row)).collect();
            let total = |debit: bool| -> Decimal {
                kept.iter()
                    .filter(|row| row["debit"] == debit)
                    .map(|row| num(&row["amount"]))
                    .sum()
            };
            (kept.len() as i64, total(false), total(true))
        };
        let groups: [(&str, Keep); 4] = [
            ("booked", &|row| {
                row["counted"] == true && row["pending"] == false
            }),
            ("pending", &|row| {
                row["counted"] == true && row["pending"] == true
            }),
            ("masters", &|row| row["counted"] == false),
            ("all_records", &|_| true),
        ];
        for (group, keep) in groups {
            let wanted = Sums::of(&[&account[group]]);
            let (count, credits, debits) = sums(keep);
            assert_eq!(count, wanted.count, "{name} {group}");
            assert_eq!(credits, wanted.credits, "{name} {group}");
            assert_eq!(debits, wanted.debits, "{name} {group}");
        }

        // The balance rule of the demo vault: the booked bookings that count
        // add up to the stored balance.
        if account["balance_check"]["applies"] == true {
            let (_, credits, debits) =
                sums(&|row: &Value| row["counted"] == true && row["pending"] == false);
            assert_eq!(
                credits + debits,
                dec(&account["statement_balance"]),
                "{name}"
            );
        }
        assert_eq!(
            own.iter()
                .filter(|row| row["booking_date"].is_null())
                .count() as i64,
            int(&account["records_without_booking_date"]),
            "{name}"
        );
    }
}

#[test]
fn the_list_is_the_same_in_every_format() {
    let rows = all_bookings();
    assert_eq!(jsonl(&["bookings", "list"]), rows);

    let (header, records) = csv(&["bookings", "list"]);
    assert_eq!(header.join(","), BOOKING_FIELDS);
    assert_eq!(records.len(), rows.len());
    for (record, row) in records.iter().zip(rows) {
        for (name, text) in header.iter().zip(record) {
            let wanted = match &row[name] {
                Value::Null => String::new(),
                Value::String(text) => text.clone(),
                // 950.00 in csv, 950.0 once JSON has parsed it.
                Value::Number(_) if name == "amount" => {
                    assert_eq!(Decimal::from_str(text).unwrap(), num(&row[name]));
                    continue;
                }
                other => other.to_string(),
            };
            assert_eq!(*text, wanted, "{name} of {:?}", key(row));
        }
    }

    // The table: one line per booking, whatever its purpose holds.
    let table = ok(&["bookings", "list"]);
    let mut lines = table.lines();
    let header: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
    assert_eq!(
        header,
        ["ID", "DATE", "ACCOUNT", "PAYEE", "PURPOSE", "CATEGORY", "AMOUNT"]
    );
    let lines: Vec<&str> = lines.collect();
    assert_eq!(lines.len(), rows.len());
    for (line, row) in lines.iter().zip(rows) {
        assert!(line.starts_with(&format!("{}  ", row["id"])), "{line}");
        let date = match (row["booking_date"].as_str(), row["pending"] == true) {
            (Some(date), false) => format!("  {date}    "),
            (Some(date), true) => format!("  {date} *  "),
            (None, _) => "  pending       ".to_owned(),
        };
        assert!(line.contains(&date), "{line}");
        let amount = format!(
            " {:.2} {}",
            num(&row["amount"]),
            row["currency"].as_str().unwrap()
        );
        assert!(line.ends_with(&amount), "{line} does not end with {amount}");
    }
    // No colour and no other escape sequence, also not for a debit.
    assert!(!table.contains('\u{1b}'));
    assert!(table.contains(" -950.00 EUR\n"));
}

#[test]
fn the_account_filter_selects_the_records_of_the_manifest() {
    for account in accounts() {
        let id = account["oid"].to_string();
        let rows = json(&["bookings", "list", "--account", &id]);
        assert_eq!(rows.len() as i64, int(&account["booking_records"]));
        assert!(rows.iter().all(|row| row["account_id"] == account["oid"]));
    }
    // Several accounts are a union; naming one twice changes nothing.
    let two = json(&[
        "bookings",
        "list",
        "--account",
        "tagesgeld",
        "--account",
        "bargeld",
        "--account",
        "DE09999900022000471122",
    ]);
    assert_eq!(
        two.len() as i64,
        int(&account("Tagesgeld")["booking_records"]) + int(&account("Bargeld")["booking_records"])
    );
    // A hidden account is data like any other.
    let hidden = json(&["bookings", "list", "--account", "sparbuch"]);
    assert_eq!(hidden.len(), 3);
    assert!(all_bookings()
        .iter()
        .any(|row| row["account_name"] == "Altes Sparbuch"));
}

#[test]
fn status_direction_and_category_filters_match_the_manifest() {
    let count = |args: &[&str]| json(&with(&["bookings", "list"], args)).len() as i64;
    let total = |field: &str, group: &str| -> i64 {
        accounts()
            .iter()
            .map(|account| int(&account[group][field]))
            .sum()
    };
    let masters = total("count", "masters");
    assert_eq!(masters, 2);

    // Pending and booked: the masters of the demo vault are booked.
    assert_eq!(count(&["--pending"]), total("count", "pending"));
    assert_eq!(count(&["--pending"]), 5);
    assert_eq!(count(&["--booked"]), total("count", "booked") + masters);

    // Debit and credit, per account and over all.
    for account in accounts() {
        let id = account["oid"].to_string();
        assert_eq!(
            count(&["--account", &id, "--debit"]),
            int(&account["all_records"]["debit_count"])
        );
        assert_eq!(
            count(&["--account", &id, "--credit"]),
            int(&account["all_records"]["credit_count"])
        );
        assert_eq!(
            count(&["--account", &id, "--credit", "--booked"]),
            int(&account["booked"]["credit_count"]) + int(&account["masters"]["credit_count"])
        );
    }
    assert_eq!(
        count(&["--pending", "--credit"]),
        total("credit_count", "pending")
    );

    // Without a category: the booked ones of the manifest, the two masters
    // (their parts carry the categories) and the pending ones without.
    let uncategorized_booked: i64 = accounts()
        .iter()
        .map(|account| int(&account["booked_without_category"]))
        .sum();
    assert_eq!(uncategorized_booked, 33);
    assert_eq!(
        count(&["--uncategorized", "--booked"]),
        uncategorized_booked + masters
    );

    // A category by its exact name: the records the manifest counts for it,
    // plus those of the categories below it.
    let categories = manifest()["categories"].as_array().unwrap();
    for needle in [
        "Wohnen",
        "wohnen:nebenkosten",
        "ÖPNV",
        "öpnv",
        "(Umbuchung)",
        "Einkommen:Gehalt",
        "strom",
    ] {
        let wanted: i64 = categories
            .iter()
            .filter(|category| has(category, "category", needle))
            .map(|category| int(&category["booking_records"]))
            .sum();
        assert!(wanted > 0, "{needle}");
        assert_eq!(count(&["--category", needle]), wanted, "{needle}");
    }
    // `Wohnen` is its own records and those of its five subcategories.
    let below_wohnen = categories
        .iter()
        .filter(|category| {
            category["category"]
                .as_str()
                .is_some_and(|name| name == "Wohnen" || name.starts_with("Wohnen:"))
        })
        .count();
    assert_eq!(below_wohnen, 6);
    assert_eq!(count(&["--category", "no such category"]), 0);
}

#[test]
fn date_ranges_work_at_day_month_and_year_granularity() {
    let listed = |args: &[&str]| keys(&json(&with(&["bookings", "list"], args)));
    let between = |from: &str, to: &str| {
        expected(|row| {
            row["booking_date"]
                .as_str()
                .is_some_and(|date| from <= date && date <= to)
        })
    };

    assert_eq!(
        listed(&["--from", "2025-03-18", "--to", "2025-03-18"]),
        between("2025-03-18", "2025-03-18")
    );
    assert_eq!(
        listed(&["--from", "2024-02", "--to", "2024-02"]),
        between("2024-02-01", "2024-02-29")
    );
    assert_eq!(
        listed(&["--from", "2025-02", "--to", "2025-02"]),
        between("2025-02-01", "2025-02-28")
    );
    assert_eq!(
        listed(&["--from", "2024", "--to", "2024"]),
        between("2024-01-01", "2024-12-31")
    );
    assert_eq!(
        listed(&["--from", "2024-11-14", "--to", "2025-01"]),
        between("2024-11-14", "2025-01-31")
    );
    assert_eq!(
        listed(&["--from", "2025"]),
        between("2025-01-01", "9999-12-31")
    );
    assert_eq!(
        listed(&["--to", "2024-06"]),
        between("0001-01-01", "2024-06-30")
    );
    assert!(!between("2024-02-01", "2024-02-29").is_empty());

    // The data runs from 2024-01-01 to 2025-06-30: the two years are all
    // bookings that have a date, and the three without pass no range.
    let dated = listed(&["--to", "2024"]).len() + listed(&["--from", "2025"]).len();
    let undated: i64 = accounts()
        .iter()
        .map(|account| int(&account["records_without_booking_date"]))
        .sum();
    assert_eq!(
        dated as i64 + undated,
        int(&manifest()["totals"]["booking"])
    );
    assert_eq!(undated, 3);
    assert!(listed(&["--from", "2025-07"]).is_empty());
    assert!(listed(&["--to", "2023"]).is_empty());
}

#[test]
fn amount_ranges_compare_the_absolute_amount() {
    let listed = |args: &[&str]| keys(&json(&with(&["bookings", "list"], args)));
    let between = |min: &str, max: &str| {
        let (min, max) = (
            Decimal::from_str(min).unwrap(),
            Decimal::from_str(max).unwrap(),
        );
        expected(|row| {
            let amount = num(&row["amount"]).abs();
            min <= amount && amount <= max
        })
    };
    assert_eq!(listed(&["--min", "1000"]), between("1000", "99999999"));
    assert_eq!(listed(&["--max", "5"]), between("0", "5"));
    assert_eq!(
        listed(&["--min", "84.37", "--max", "84,37"]),
        between("84.37", "84.37")
    );
    assert_eq!(
        listed(&["--min", "950", "--max", "950.00"]),
        between("950", "950")
    );
    assert_eq!(
        listed(&["--min", "50", "--max", "100"]),
        between("50", "100")
    );
    // Both directions are in one range.
    let rent = json(&["bookings", "list", "--min", "3000"]);
    assert!(rent.iter().any(|row| row["debit"] == true));
    assert!(rent.iter().any(|row| row["debit"] == false));
    assert!(!between("84.37", "84.37").is_empty());
}

#[test]
fn text_filters_are_plain_substrings_without_regard_to_case() {
    let listed = |args: &[&str]| keys(&json(&with(&["bookings", "list"], args)));
    let anywhere = |needle: &str| {
        expected(|row| {
            [
                "payee_name",
                "purpose",
                "booking_text",
                "category",
                "notes",
                "payee_iban",
            ]
            .iter()
            .any(|field| has(row, field, needle))
        })
    };
    for needle in [
        "supermarkt",
        "SUPERMARKT",
        "Lohn/Gehalt",
        "DEMO-SAMMEL",
        "karte 1",
        "ÖPNV",
        "öpnv",
        "€",
        "de46999900040004007919",
    ] {
        let wanted = anywhere(needle);
        assert!(!wanted.is_empty(), "{needle}");
        assert_eq!(listed(&["--filter", needle]), wanted, "{needle}");
    }
    // An IBAN pasted in groups of four.
    assert_eq!(
        listed(&["--filter", "DE46 9999 0004 0004 0079 19"]),
        anywhere("DE46999900040004007919")
    );

    let by_payee = |needle: &str| expected(|row| has(row, "payee_name", needle));
    for needle in ["beispielwerke", "Stadtwerke Musterstadt", "GMBH", "ü"] {
        let wanted = by_payee(needle);
        assert!(!wanted.is_empty(), "{needle}");
        assert_eq!(listed(&["--payee", needle]), wanted, "{needle}");
    }
    // The salary: 18 bookings of the category in the manifest.
    let salary = manifest()["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|category| category["category"] == "Einkommen:Gehalt")
        .unwrap();
    assert_eq!(
        listed(&["--payee", "beispielwerke", "--category", "gehalt"]).len() as i64,
        int(&salary["booking_records"])
    );

    // Not patterns: as LIKE wildcards these would select nearly everything.
    let percent = anywhere("%");
    assert_eq!(listed(&["--filter", "%"]), percent);
    assert!(percent.len() < 100, "{}", percent.len());
    assert_eq!(listed(&["--filter", "_"]), anywhere("_"));
    assert!(listed(&["--payee", "%"]).is_empty());
    assert!(listed(&["--payee", "_"]).is_empty());
    assert!(listed(&["--category", "%"]).is_empty());
    // Not SQL either.
    assert!(listed(&["--filter", "x' or '1'='1"]).is_empty());
    assert!(listed(&["--payee", "'; drop table bookings; --"]).is_empty());
    assert_eq!(json(&["bookings", "list"]).len(), all_bookings().len());
}

#[test]
fn filters_combine() {
    let rows = json(&[
        "bookings",
        "list",
        "--account",
        "giro",
        "--from",
        "2025",
        "--category",
        "wohnen",
        "--min",
        "100",
        "--debit",
        "--booked",
    ]);
    let wanted = expected(|row| {
        row["account_name"] == "Girokonto"
            && row["booking_date"]
                .as_str()
                .is_some_and(|date| date >= "2025-01-01")
            && has(row, "category", "wohnen")
            && num(&row["amount"]).abs() >= Decimal::from(100)
            && row["debit"] == true
            && row["pending"] == false
    });
    assert!(!wanted.is_empty());
    assert_eq!(keys(&rows), wanted);
}

#[test]
fn the_sort_orders() {
    let rows = all_bookings();
    // Default: by booking date, bookings without one last, then by id.
    let sort_key = |row: &Value| {
        (
            row["booking_date"].is_null(),
            row["booking_date"].as_str().unwrap_or("").to_owned(),
            int(&row["id"]),
            int(&row["account_id"]),
        )
    };
    let mut by_date = rows.to_vec();
    by_date.sort_by_key(sort_key);
    assert_eq!(keys(rows), keys(&by_date));
    assert!(rows.last().unwrap()["booking_date"].is_null());
    assert_eq!(rows[0]["booking_date"], "2024-01-01");
    assert_eq!(
        keys(&json(&["bookings", "list", "--sort", "date"])),
        keys(rows)
    );

    // Reversed: the exact mirror image, the undated pending ones first.
    let mut mirrored = keys(rows);
    mirrored.reverse();
    let reversed = json(&["bookings", "list", "--reverse"]);
    assert_eq!(keys(&reversed), mirrored);
    assert!(reversed[0]["booking_date"].is_null());

    // By signed amount, smallest first; equal amounts by date.
    let by_amount = json(&["bookings", "list", "--sort", "amount"]);
    assert_eq!(by_amount.len(), rows.len());
    let mut wanted = rows.to_vec();
    wanted.sort_by_key(|row| (num(&row["amount"]), sort_key(row)));
    assert_eq!(keys(&by_amount), keys(&wanted));
    assert!(num(&by_amount[0]["amount"]) < Decimal::ZERO);
    let largest = json(&[
        "bookings",
        "list",
        "--sort",
        "amount",
        "--reverse",
        "--limit",
        "1",
    ]);
    assert_eq!(
        num(&largest[0]["amount"]),
        rows.iter().map(|row| num(&row["amount"])).max().unwrap()
    );

    // By payee name without regard to case, bookings without a payee last.
    let by_payee = json(&["bookings", "list", "--sort", "payee"]);
    let mut wanted = rows.to_vec();
    wanted.sort_by_key(|row| {
        (
            row["payee_name"].is_null(),
            row["payee_name"].as_str().unwrap_or("").to_lowercase(),
            sort_key(row),
        )
    });
    assert_eq!(keys(&by_payee), keys(&wanted));
    let mut mirrored = keys(&by_payee);
    mirrored.reverse();
    assert_eq!(
        keys(&json(&["bookings", "list", "--sort", "payee", "--reverse"])),
        mirrored
    );
}

#[test]
fn limit_keeps_the_first_rows_of_the_order() {
    let rows = all_bookings();
    assert_eq!(
        keys(&json(&["bookings", "list", "--limit", "7"])),
        keys(&rows[..7])
    );
    assert_eq!(
        json(&["bookings", "list", "--limit", "100000"]).len(),
        rows.len()
    );

    // The 20 most recent, newest first.
    let mut newest = keys(&rows[rows.len() - 20..]);
    newest.reverse();
    assert_eq!(
        keys(&json(&["bookings", "list", "--reverse", "--limit", "20"])),
        newest
    );
    assert_eq!(
        ok(&["bookings", "list", "--reverse", "--limit", "20"])
            .lines()
            .count(),
        21
    );

    // After the filters, not before.
    let salaries = json(&["bookings", "list", "--category", "gehalt", "--limit", "2"]);
    assert_eq!(salaries.len(), 2);
    assert!(salaries
        .iter()
        .all(|row| row["category"] == "Einkommen:Gehalt"));
    let (_, records) = csv(&["bookings", "list", "--limit", "3"]);
    assert_eq!(records.len(), 3);

    // No limit of nothing: a usage error.
    let output = run(&["bookings", "list", "--limit", "0"]);
    assert_eq!(exit_code(&output), 2);
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output).starts_with(
            "error: invalid value '0' for '--limit <N>': expected a whole number of at least 1\n"
        ),
        "{}",
        stderr(&output)
    );
}

// --- splits and batches ---

#[test]
fn the_split_and_the_batch_are_listed_whole_and_counted_once() {
    let giro = account("Girokonto");
    assert_eq!(giro["groups"]["split_masters"], 1);
    assert_eq!(giro["groups"]["split_parts"], 3);
    assert_eq!(giro["groups"]["batch_masters"], 1);
    assert_eq!(giro["groups"]["batch_parts"], 3);

    for (day, master_role, part_role, whole) in [
        ("2024-11-14", "split_master", "split_slave", "-84.37"),
        ("2025-03-18", "batch_master", "batch_slave", "-244.88"),
    ] {
        let listed = json(&[
            "bookings",
            "list",
            "--account",
            "giro",
            "--from",
            day,
            "--to",
            day,
        ]);
        let masters: Vec<&Value> = listed
            .iter()
            .filter(|row| row["split_role"] == master_role)
            .collect();
        assert_eq!(masters.len(), 1, "{day}");
        let master = masters[0];
        assert_eq!(master["counted"], false);
        assert_eq!(master["master_id"], master["id"]);
        assert_eq!(num(&master["amount"]), Decimal::from_str(whole).unwrap());

        let parts: Vec<&Value> = listed
            .iter()
            .filter(|row| row["split_role"] == part_role)
            .collect();
        assert_eq!(parts.len(), 3, "{day}");
        for part in &parts {
            assert_eq!(part["counted"], true);
            assert_eq!(part["master_id"], master["id"]);
        }
        let of_parts: Decimal = parts.iter().map(|part| num(&part["amount"])).sum();
        assert_eq!(of_parts, num(&master["amount"]), "{day}");

        // The summary of that day: the parts, not parts plus the whole.
        let counted: Vec<&Value> = listed.iter().filter(|row| row["counted"] == true).collect();
        let summed = json(&[
            "bookings",
            "summary",
            "--by",
            "month",
            "--account",
            "giro",
            "--from",
            day,
            "--to",
            day,
        ]);
        assert_eq!(summed.len(), 1);
        assert_eq!(summed[0]["month"], day[..7]);
        assert_eq!(int(&summed[0]["booking_count"]), counted.len() as i64);
        assert_eq!(counted.len() + 1, listed.len());
        assert_eq!(
            num(&summed[0]["net"]),
            counted
                .iter()
                .map(|row| num(&row["amount"]))
                .sum::<Decimal>()
        );
    }

    // The split by payee: three parts that add up to the purchase.
    let split = json(&[
        "bookings",
        "summary",
        "--by",
        "payee",
        "--payee",
        "drogerie",
        "--from",
        "2024-11-14",
        "--to",
        "2024-11-14",
    ]);
    assert_eq!(split.len(), 1);
    assert_eq!(split[0]["booking_count"], 3);
    assert_eq!(
        num(&split[0]["debits"]),
        Decimal::from_str("-84.37").unwrap()
    );
    // Its parts keep their own categories.
    let categories = json(&[
        "bookings",
        "summary",
        "--by",
        "category",
        "--payee",
        "drogerie",
        "--from",
        "2024-11-14",
        "--to",
        "2024-11-14",
    ]);
    let names: Vec<&str> = categories
        .iter()
        .map(|row| row["category"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Gesundheit", "Haushalt", "Sonstiges"]);

    // Over everything: every record but the two masters.
    let years = json(&["bookings", "summary", "--by", "year"]);
    let counted: i64 = years.iter().map(|row| int(&row["booking_count"])).sum();
    assert_eq!(counted, int(&manifest()["totals"]["booking"]) - 2);
}

// --- bookings summary ---

#[test]
fn the_summary_by_account_matches_the_manifest_in_every_format() {
    // Accounts with bookings, by name.
    let mut wanted: Vec<&Value> = accounts()
        .iter()
        .filter(|account| int(&account["booking_records"]) > 0)
        .collect();
    wanted.sort_by_key(|account| account["name"].as_str().unwrap().to_lowercase());
    assert_eq!(wanted.len(), 5);

    for (filter, groups) in [
        (&[][..], &["booked", "pending"][..]),
        (&["--booked"], &["booked"]),
    ] {
        let args = with(&["bookings", "summary", "--by", "account"], filter);
        let rows = json(&args);
        assert_eq!(jsonl(&args), rows);
        assert_eq!(rows.len(), wanted.len());
        let (header, records) = csv(&args);
        assert_eq!(
            header.join(","),
            format!("account_id,account_name,{SUMMARY_FIELDS}")
        );
        assert_eq!(records.len(), wanted.len());
        let table = ok(&args);
        assert_eq!(table.lines().count(), wanted.len() + 1);
        let titles: Vec<&str> = table.lines().next().unwrap().split_whitespace().collect();
        assert_eq!(
            titles,
            ["ID", "ACCOUNT", "BOOKINGS", "DEBITS", "CREDITS", "NET"]
        );

        for (index, account) in wanted.iter().enumerate() {
            let name = account["name"].as_str().unwrap();
            let groups: Vec<&Value> = groups.iter().map(|group| &account[*group]).collect();
            let sums = Sums::of(&groups);

            let row = &rows[index];
            assert_eq!(row["account_id"], account["oid"], "{name}");
            assert_eq!(row["account_name"], account["name"]);
            assert_eq!(row["currency"], account["currency"], "{name}");
            assert_eq!(int(&row["booking_count"]), sums.count, "{name}");
            assert_eq!(num(&row["debits"]), sums.debits, "{name}");
            assert_eq!(num(&row["credits"]), sums.credits, "{name}");
            assert_eq!(num(&row["net"]), sums.net(), "{name}");

            let record = &records[index];
            assert_eq!(cell(&header, record, "account_name"), name);
            assert_eq!(
                cell(&header, record, "booking_count"),
                sums.count.to_string()
            );
            for (field, value) in [
                ("debits", sums.debits),
                ("credits", sums.credits),
                ("net", sums.net()),
            ] {
                assert_eq!(
                    Decimal::from_str(cell(&header, record, field)).unwrap(),
                    value,
                    "{name} {field}"
                );
            }

            let line = table.lines().nth(index + 1).unwrap();
            let currency = account["currency"].as_str().unwrap();
            let mut words: Vec<String> = vec![account["oid"].to_string()];
            words.extend(name.split_whitespace().map(str::to_owned));
            words.push(sums.count.to_string());
            for value in [sums.debits, sums.credits, sums.net()] {
                words.push(format!("{value:.2}"));
                words.push(currency.to_owned());
            }
            assert_eq!(
                line.split_whitespace().collect::<Vec<&str>>(),
                words,
                "{line}"
            );

            // With --booked the net amount is the stored balance.
            if filter == ["--booked"] {
                assert_eq!(
                    num(&row["net"]),
                    dec(&account["statement_balance"]),
                    "{name}"
                );
            }
        }
    }

    // One account alone, and only its pending bookings.
    let pending = json(&["bookings", "summary", "--by", "account", "--pending"]);
    let names: Vec<&str> = pending
        .iter()
        .map(|row| row["account_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Girokonto", "Kreditkarte"]);
    for row in &pending {
        let sums = Sums::of(&[&account(row["account_name"].as_str().unwrap())["pending"]]);
        assert_eq!(int(&row["booking_count"]), sums.count);
        assert_eq!(num(&row["net"]), sums.net());
    }
}

#[test]
fn the_summary_by_category_matches_the_manifest() {
    let rows = json(&["bookings", "summary", "--by", "category", "--booked"]);
    let wanted: Vec<&Value> = manifest()["categories"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|category| int(&category["booked"]["count"]) > 0)
        .collect();
    assert_eq!(rows.len(), wanted.len());
    for category in &wanted {
        let name = &category["category"];
        let row = rows
            .iter()
            .find(|row| row["category"] == *name)
            .unwrap_or_else(|| panic!("no row for {name}"));
        let sums = Sums::of(&[&category["booked"]]);
        assert_eq!(row["currency"], "EUR");
        assert_eq!(int(&row["booking_count"]), sums.count, "{name}");
        assert_eq!(num(&row["debits"]), sums.debits, "{name}");
        assert_eq!(num(&row["credits"]), sums.credits, "{name}");
        assert_eq!(num(&row["net"]), sums.net(), "{name}");
    }
    // By name without regard to case, the bookings without a category last.
    let names: Vec<String> = rows
        .iter()
        .filter_map(|row| row["category"].as_str())
        .map(str::to_lowercase)
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(names.len(), rows.len() - 1);
    assert_eq!(rows.last().unwrap()["category"], Value::Null);
    assert_eq!(rows.last().unwrap()["booking_count"], 33);

    // The table names that group.
    let table = ok(&["bookings", "summary", "--by", "category", "--booked"]);
    assert!(
        table.lines().last().unwrap().starts_with("(none)  "),
        "{table}"
    );
    assert!(table.starts_with("CATEGORY  "), "{table}");

    // A subtree as one number: the filter, grouped by something else.
    let subtree: Vec<&&Value> = wanted
        .iter()
        .filter(|category| has(category, "category", "wohnen"))
        .collect();
    let total = json(&[
        "bookings",
        "summary",
        "--by",
        "account",
        "--category",
        "wohnen",
        "--booked",
    ]);
    assert_eq!(
        total
            .iter()
            .map(|row| int(&row["booking_count"]))
            .sum::<i64>(),
        subtree
            .iter()
            .map(|category| int(&category["booked"]["count"]))
            .sum::<i64>()
    );
    assert_eq!(
        total.iter().map(|row| num(&row["net"])).sum::<Decimal>(),
        subtree
            .iter()
            .map(|category| dec(&category["booked"]["net"]))
            .sum::<Decimal>()
    );
}

#[test]
fn the_summary_by_month_year_and_payee_adds_up_to_the_accounts() {
    let counted: Vec<&Value> = all_bookings()
        .iter()
        .filter(|row| row["counted"] == true)
        .collect();
    let net: Decimal = accounts()
        .iter()
        .map(|account| Sums::counted(account).net())
        .sum();

    for by in ["month", "year", "payee"] {
        let rows = json(&["bookings", "summary", "--by", by]);
        assert_eq!(
            rows.iter()
                .map(|row| int(&row["booking_count"]))
                .sum::<i64>(),
            counted.len() as i64,
            "{by}"
        );
        assert_eq!(
            rows.iter().map(|row| num(&row["net"])).sum::<Decimal>(),
            net,
            "{by}"
        );
        for row in &rows {
            assert_eq!(
                num(&row["debits"]) + num(&row["credits"]),
                num(&row["net"]),
                "{by}"
            );
            assert!(num(&row["debits"]) <= Decimal::ZERO);
            assert!(num(&row["credits"]) >= Decimal::ZERO);
        }
    }

    // Months ascending from the first to the last month of the data, then
    // the pending bookings that have no date yet.
    let months = json(&["bookings", "summary", "--by", "month"]);
    assert_eq!(months.len(), 18 + 1);
    assert_eq!(months[0]["month"], "2024-01");
    assert_eq!(months[17]["month"], "2025-06");
    assert_eq!(months[18]["month"], Value::Null);
    assert_eq!(months[18]["booking_count"], 3);
    for (index, row) in months[..18].iter().enumerate() {
        let month = row["month"].as_str().unwrap();
        assert_eq!(
            month,
            format!("{}-{:02}", 2024 + index / 12, index % 12 + 1)
        );
        let of_month: Vec<&&Value> = counted
            .iter()
            .filter(|booking| {
                booking["booking_date"]
                    .as_str()
                    .is_some_and(|date| date.starts_with(month))
            })
            .collect();
        assert_eq!(int(&row["booking_count"]), of_month.len() as i64, "{month}");
        assert_eq!(
            num(&row["net"]),
            of_month
                .iter()
                .map(|booking| num(&booking["amount"]))
                .sum::<Decimal>(),
            "{month}"
        );
    }
    let table = ok(&["bookings", "summary", "--by", "month"]);
    assert!(
        table.lines().last().unwrap().starts_with("(no date)  "),
        "{table}"
    );

    let years = json(&["bookings", "summary", "--by", "year"]);
    let names: Vec<&Value> = years.iter().map(|row| &row["year"]).collect();
    assert_eq!(
        names,
        [&Value::from("2024"), &Value::from("2025"), &Value::Null]
    );

    // One payee: the bookings the list shows for it, masters left out.
    let payees = json(&["bookings", "summary", "--by", "payee"]);
    let salary = payees
        .iter()
        .find(|row| row["payee_name"] == "Beispielwerke AG")
        .unwrap();
    let of_payee: Vec<&&Value> = counted
        .iter()
        .filter(|booking| booking["payee_name"] == "Beispielwerke AG")
        .collect();
    assert_eq!(int(&salary["booking_count"]), of_payee.len() as i64);
    assert_eq!(
        num(&salary["credits"]),
        of_payee
            .iter()
            .map(|booking| num(&booking["amount"]))
            .sum::<Decimal>()
    );
    let (header, _) = csv(&["bookings", "summary", "--by", "payee"]);
    assert_eq!(header.join(","), format!("payee_name,{SUMMARY_FIELDS}"));
    let (header, _) = csv(&["bookings", "summary", "--by", "month"]);
    assert_eq!(header.join(","), format!("month,{SUMMARY_FIELDS}"));
}

// --- bookings show ---

#[test]
fn show_prints_every_stored_field_of_a_booking() {
    let sample = manifest()["samples"]
        .as_array()
        .unwrap()
        .iter()
        .find(|sample| sample["record_type"] == "booking" && sample["fields"]["oid"] == 796_370_400)
        .expect("the manifest has no sample of the salary booking");
    let fields = &sample["fields"];

    let shown = json_object(&["bookings", "show", "796370400"]);
    assert_eq!(shown["id"], fields["oid"]);
    assert_eq!(shown["account_name"], "Girokonto");
    assert_eq!(shown["account_iban"], account("Girokonto")["iban"]);
    assert_eq!(num(&shown["amount"]), dec(&fields["amount"]));
    assert_eq!(shown["debit"], fields["debit_flag"]);
    assert_eq!(shown["currency"], fields["currency"]);
    assert_eq!(shown["payee_name"], fields["counterparty_name"]);
    assert_eq!(shown["payee_iban"], fields["counterparty_account"]);
    assert_eq!(shown["payee_bic"], fields["counterparty_bank_code"]);
    let purpose: Vec<&str> = fields["purpose_lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap())
        .collect();
    assert_eq!(shown["purpose"], purpose.join("\n"));
    for field in [
        "category",
        "booking_date",
        "value_date",
        "booking_text",
        "primanota",
        "bank_reference",
        "end_to_end_id",
        "purpose_code",
        "gvc",
        "swift_transaction_code",
        "notes",
        "mandate_id",
    ] {
        assert_eq!(shown[field], fields[field], "{field}");
    }
    assert_eq!(shown["status"], "booked");
    assert_eq!(shown["counted"], true);
    // Many more fields than the list has, the empty ones included.
    let object = shown.as_object().unwrap();
    assert!(object.len() > 60, "{}", object.len());
    assert!(object.contains_key("debit_return_date"));
    let listed = json(&["bookings", "list", "--filter", "LG202503004711"]);
    assert!(
        listed.is_empty(),
        "the end-to-end id is not a searched field"
    );

    assert_eq!(
        jsonl(&["bookings", "show", "796370400"]),
        std::slice::from_ref(&shown)
    );
    let (header, rows) = csv(&["bookings", "show", "796370400"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(header.len(), object.len());
    assert_eq!(cell(&header, &rows[0], "purpose"), purpose.join("\n"));

    let text = ok(&["bookings", "show", "796370400"]);
    for line in [
        "Account ID:              757073100\n",
        "Account name:            Girokonto\n",
        "ID:                      796370400\n",
        "Booking date:            2025-03-27\n",
        "Amount:                  3342.50 EUR\n",
        "Payee name:              Beispielwerke AG\n",
        "Purpose:                 Lohn/Gehalt 03/2025\n                         Personalnr. 004711\n",
        "Category:                Einkommen:Gehalt\n",
        "End to end ID:           LG202503004711\n",
    ] {
        assert!(text.contains(line), "{line:?} is not in:\n{text}");
    }
    assert!(!text.contains("Notes:"), "{text}");
    assert!(!text.contains("Mandate ID:"), "{text}");

    // Narrowed to its account, by any reference.
    for reference in ["giro", "757073100", "DE31 9999 0001 1000 4711 00"] {
        assert_eq!(
            json_object(&["bookings", "show", "796370400", "--account", reference]),
            shown
        );
    }
}

#[test]
fn show_fails_for_a_booking_that_is_not_there() {
    assert_eq!(
        failure(&["bookings", "show", "1"]),
        "error: no booking has the id 1: `b4 bookings list` lists the bookings with their ids\n"
    );
    assert_eq!(
        failure(&[
            "bookings",
            "show",
            "796370400",
            "--account",
            "tagesgeld",
            "-o",
            "json"
        ]),
        "error: the account Tagesgeld (757073160) has no booking with the id 796370400\n"
    );
    assert_eq!(
        failure(&["bookings", "show", "-5"]),
        "error: no booking has the id -5: `b4 bookings list` lists the bookings with their ids\n"
    );
    let output = run(&["bookings", "show", "abc"]);
    assert_eq!(exit_code(&output), 2);
}

#[test]
fn show_says_what_a_master_and_a_pending_booking_are() {
    let master = json_object(&["bookings", "show", "784879200"]);
    assert_eq!(master["split_role"], "split_master");
    assert_eq!(master["counted"], false);
    assert_eq!(master["master_id"], 784_879_200);
    let part = json_object(&["bookings", "show", "784879201"]);
    assert_eq!(part["split_role"], "split_slave");
    assert_eq!(part["counted"], true);
    assert_eq!(part["master_id"], 784_879_200);
    assert_eq!(part["category"], "Haushalt");

    let pending = json_object(&["bookings", "show", "804664800"]);
    assert_eq!(pending["status"], "pending");
    assert_eq!(pending["pending"], true);
    assert_eq!(pending["booking_date"], Value::Null);
    assert_eq!(pending["value_date"], "2025-07-01");
    let text = ok(&["bookings", "show", "804664800"]);
    assert!(
        text.lines()
            .any(|line| line.starts_with("Status:") && line.ends_with("  pending")),
        "{text}"
    );
    assert!(!text.contains("Booking date:"), "{text}");
}

// --- standing orders and portfolio ---

#[test]
fn standing_orders_list_shows_the_three_orders_of_the_demo_vault() {
    let rows = json(&["standing-orders", "list"]);
    assert_eq!(
        rows.len() as i64,
        int(&manifest()["totals"]["standing_order"])
    );
    assert_eq!(jsonl(&["standing-orders", "list"]), rows);

    // Each against its sample in the manifest.
    let samples: Vec<&Value> = manifest()["samples"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|sample| sample["record_type"] == "standing_order")
        .collect();
    assert!(!samples.is_empty());
    for sample in samples {
        let fields = &sample["fields"];
        let row = rows
            .iter()
            .find(|row| row["id"] == fields["oid"])
            .unwrap_or_else(|| panic!("no standing order {}", fields["oid"]));
        assert_eq!(row["account_name"], "Girokonto");
        assert_eq!(row["payee_name"], fields["counterparty_name"]);
        assert_eq!(row["payee_iban"], fields["counterparty_account"]);
        assert_eq!(row["category"], fields["category"]);
        assert_eq!(row["currency"], fields["currency"]);
        // Stored without a sign and with a direction; shown signed.
        assert_eq!(fields["debit_flag"], true);
        assert_eq!(row["debit"], true);
        assert_eq!(num(&row["amount"]), -dec(&fields["amount"]));
        let purpose: Vec<&str> = fields["purpose_lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect();
        assert_eq!(row["purpose"], purpose.join("\n"));
    }

    let table = ok(&["standing-orders", "list"]);
    let expected = r"       ID  ACCOUNT    PAYEE                                   PURPOSE                                                     AMOUNT  EVERY      DAY  NEXT        STATE
757083601  Girokonto  Hausverwaltung Beispiel GmbH            Miete Musterstraße 12, Whg. 4 ↵ Mietvertrag 4711-2023  -950.00 EUR  monthly      1              at_bank
757083602  Girokonto  Erika Mustermann                        Sparrate                                               -300.00 EUR  monthly     28              at_bank
757083603  Girokonto  Turn- und Sportverein Musterstadt e.V.  Mitgliedsbeitrag ↵ Mitglied 1184                        -45.00 EUR  quarterly    2  2025-07-02  local
";
    assert_eq!(table, expected);

    let (header, records) = csv(&["standing-orders", "list", "--account", "giro"]);
    assert_eq!(
        header.join(","),
        "account_id,id,account_name,scheme,amount,currency,debit,payee_name,payee_iban,\
         payee_bic,purpose,category,state,is_local,period_unit,period_length,execution_day,\
         first_execution_date,last_execution_date,next_execution_date,stored_next_execution_date"
    );
    assert_eq!(records.len(), 3);
    // The next day of an order held at the bank is not known to b4.
    assert_eq!(cell(&header, &records[0], "state"), "at_bank");
    assert_eq!(cell(&header, &records[0], "next_execution_date"), "");
    assert_eq!(
        cell(&header, &records[2], "next_execution_date"),
        "2025-07-02"
    );
}

#[test]
fn portfolio_list_shows_the_stored_positions_and_computes_nothing() {
    let rows = json(&["portfolio", "list"]);
    assert_eq!(
        rows.len() as i64,
        int(&manifest()["totals"]["portfolio_item"])
    );
    assert_eq!(jsonl(&["portfolio", "list"]), rows);
    assert_eq!(json(&["portfolio", "list", "--account", "depot"]), rows);

    let sample = manifest()["samples"]
        .as_array()
        .unwrap()
        .iter()
        .find(|sample| sample["record_type"] == "portfolio_item")
        .expect("the manifest has no sample of a portfolio item");
    let fields = &sample["fields"];
    let row = rows.iter().find(|row| row["id"] == fields["oid"]).unwrap();
    assert_eq!(row["account_name"], "Depot");
    assert_eq!(row["isin"], fields["isin"]);
    assert_eq!(row["name"], fields["name"]);
    assert_eq!(num(&row["quantity"]), dec(&fields["quantity"]));
    assert_eq!(num(&row["current_price"]), dec(&fields["current_price"]));
    assert_eq!(
        row["current_price_currency"],
        fields["current_price_currency"]
    );
    assert_eq!(row["current_price_date"], fields["current_price_date"]);
    assert_eq!(num(&row["purchase_price"]), dec(&fields["purchase_price"]));
    assert_eq!(
        row["purchase_price_currency"],
        fields["purchase_price_currency"]
    );
    assert_eq!(row["purchase_price_date"], fields["purchase_price_date"]);
    // Nothing that b4 would have had to work out.
    let object = row.as_object().unwrap();
    for computed in ["value", "market_value", "gain"] {
        assert!(!object.contains_key(computed), "{computed}");
    }

    let table = ok(&["portfolio", "list"]);
    let expected = "\
ACCOUNT  ISIN          NAME                   QUANTITY       PRICE  PRICE DATE  PURCHASE PRICE  PURCHASE DATE  STATE
Depot    ZZ000DEMO014  Beispiel Industrie AG      20.0   92.21 EUR  2025-06-30       76.02 EUR  2024-09-16     bank
Depot    ZZ000DEMO030  Demo Welt Aktien ETF       35.5   85.79 EUR  2025-06-30       92.18 EUR  2025-01-20     bank
Depot    ZZ000DEMO022  Muster Technologie SE      12.0  154.28 EUR  2025-06-30      131.40 EUR  2024-11-04     bank
";
    assert_eq!(table, expected);

    let (header, records) = csv(&["portfolio", "list"]);
    assert_eq!(
        header.join(","),
        "account_id,id,account_name,isin,name,quantity,quantity_type,quantity_currency,\
         price_type,current_price,current_price_currency,current_price_date,purchase_price,\
         purchase_price_currency,purchase_price_date,state,deleted"
    );
    assert_eq!(records.len(), 3);
    assert_eq!(cell(&header, &records[1], "quantity"), "35.5");
}

// --- empty results, exit codes ---

#[test]
fn an_empty_result_is_not_an_error_in_any_format() {
    for command in [
        &["bookings", "list", "--payee", "nobody has this name"][..],
        &["bookings", "list", "--from", "2030"],
        &["bookings", "list", "--account", "depot"],
        &[
            "bookings",
            "summary",
            "--by",
            "month",
            "--payee",
            "nobody has this name",
        ],
        &[
            "bookings",
            "summary",
            "--by",
            "account",
            "--account",
            "depot",
        ],
        &["accounts", "list", "--type", "paypal"],
        &["standing-orders", "list", "--account", "tagesgeld"],
        &["portfolio", "list", "--account", "giro"],
    ] {
        // Table: nothing on stdout, a note on stderr unless --quiet.
        let output = run(command);
        assert_eq!(exit_code(&output), 0, "{command:?}");
        assert_eq!(stdout(&output), "", "{command:?}");
        assert_eq!(stderr(&output), "no rows\n", "{command:?}");
        let output = run(&with(command, &["--quiet"]));
        assert_eq!(exit_code(&output), 0, "{command:?}");
        assert_eq!(stdout(&output), "", "{command:?}");
        assert_eq!(stderr(&output), "", "{command:?}");

        // The machine formats say it in their own way, and nothing else.
        assert_eq!(ok(&with(command, &["-o", "json"])), "[]\n", "{command:?}");
        assert_eq!(ok(&with(command, &["-o", "jsonl"])), "", "{command:?}");
        let (header, records) = csv(command);
        assert!(!header.is_empty(), "{command:?}");
        assert!(records.is_empty(), "{command:?}");
        assert_eq!(
            ok(&with(command, &["-o", "csv", "--no-header"])),
            "",
            "{command:?}"
        );
    }
}

#[test]
fn exit_codes_tell_success_failure_and_misuse_apart() {
    // 0: it worked.
    for command in [
        &["accounts", "list"][..],
        &["accounts", "show", "giro"],
        &["bookings", "list", "--limit", "1"],
        &["bookings", "show", "796370400"],
        &["bookings", "summary", "--by", "year"],
        &["standing-orders", "list"],
        &["portfolio", "list"],
    ] {
        let output = run(command);
        assert_eq!(exit_code(&output), 0, "{command:?}");
        assert_eq!(stderr(&output), "", "{command:?}");
        assert!(!stdout(&output).is_empty(), "{command:?}");
        assert!(!stdout(&output).contains('\u{1b}'), "{command:?}");
    }

    // 1: the command is fine, the data says no.
    for command in [
        &["accounts", "show", "nowhere"][..],
        &["bookings", "list", "--account", "nowhere"],
        &["bookings", "show", "1"],
    ] {
        assert!(failure(command).starts_with("error: "), "{command:?}");
    }
    // Without a database: what to do about it.
    for command in [
        &["accounts", "list"][..],
        &["bookings", "list"],
        &["bookings", "summary", "--by", "month"],
        &["standing-orders", "list"],
        &["portfolio", "list"],
    ] {
        let missing = Path::new(env!("CARGO_TARGET_TMPDIR")).join("queries-no-such.duckdb");
        let output = b4(command).arg("--db").arg(&missing).output().unwrap();
        assert_eq!(exit_code(&output), 1, "{command:?}");
        assert_eq!(stdout(&output), "", "{command:?}");
        let message = stderr(&output);
        assert!(
            message.starts_with("error: there is no database at "),
            "{message}"
        );
        assert!(message.contains("b4 decrypt"), "{message}");
    }

    // 2: the command line is wrong; the database is not even opened.
    for command in [
        &["bookings", "list", "--from", "2025", "--to", "2024"][..],
        &["bookings", "list", "--min", "10", "--max", "5"],
        &["bookings", "list", "--debit", "--credit"],
        &["bookings", "list", "--pending", "--booked"],
        &["bookings", "list", "--category", "x", "--uncategorized"],
        &["bookings", "list", "--from", "yesterday"],
        &["bookings", "list", "--min", "-5"],
        &["bookings", "list", "--sort", "category"],
        &["bookings", "summary"],
        &["bookings", "summary", "--by", "week"],
        &["bookings", "show"],
        &["accounts", "show"],
        &["accounts", "list", "--type", "savings"],
        &["portfolio", "list", "--account", ""],
    ] {
        let output = run(command);
        assert_eq!(exit_code(&output), 2, "{command:?}: {}", stderr(&output));
        assert_eq!(stdout(&output), "", "{command:?}");
    }
}

#[test]
fn the_query_commands_leave_the_database_alone() {
    let before = std::fs::read(database()).unwrap();
    for command in [
        &["accounts", "list", "--all"][..],
        &["accounts", "show", "giro"],
        &["bookings", "list", "--filter", "x"],
        &["bookings", "show", "796370400"],
        &["bookings", "summary", "--by", "category"],
        &["standing-orders", "list"],
        &["portfolio", "list"],
    ] {
        assert_eq!(exit_code(&run(command)), 0, "{command:?}");
    }
    assert_eq!(std::fs::read(database()).unwrap(), before);
}
