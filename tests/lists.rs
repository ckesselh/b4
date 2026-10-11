//! Black-box tests of `b4 payees list` and `b4 categories list` on the demo
//! vault of `tests/fixtures/vault`. The vault is decrypted once with
//! `b4 decrypt`; what the commands print is compared with the vault's
//! manifest, which holds the expected result of reading the vault.

// The helpers below are not `#[test]` functions, so clippy.toml's exemption
// for tests does not reach them.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
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

const CATEGORY_COLUMNS: &str = "full_name,name,parent,depth,kind,currency,booking_count,sum,\
     booking_count_with_children,sum_with_children,budget_period,budget_amount,special,\
     totals_bucket,hidden_by_banking4";

// --- running b4 ---

/// A `b4` invocation that does not see the environment of the test run and
/// has no terminal on any stream.
fn b4(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_b4"));
    command
        .args(args)
        .env_remove("B4_DB")
        .env_remove("B4_PASSWORD")
        .env_remove("B4_ASKPASS")
        .env_remove("SSH_ASKPASS")
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

/// The demo vault as a database, decrypted by the first test that asks.
///
/// It lives in cargo's directory for test files and not in a temporary
/// directory, which a `static` would never remove.
fn database() -> &'static Path {
    static DATABASE: OnceLock<PathBuf> = OnceLock::new();
    DATABASE.get_or_init(|| {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("lists-demo.duckdb");
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

/// What a command that must succeed quietly prints.
fn printed(args: &[&str]) -> String {
    let output = run(args);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{args:?}: {}",
        stderr(&output)
    );
    assert_eq!(stderr(&output), "", "{args:?}");
    stdout(&output)
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("not JSON ({error}):\n{text}"))
}

/// The rows of a command's `--output json`.
fn rows(args: &[&str]) -> Vec<Value> {
    let mut args = args.to_vec();
    args.extend(["--output", "json"]);
    match json(&printed(&args)) {
        Value::Array(rows) => rows,
        other => panic!("not an array: {other}"),
    }
}

/// A JSON number or a manifest's decimal string as an exact decimal.
fn decimal(value: &Value) -> Decimal {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        other => panic!("not a number: {other}"),
    };
    Decimal::from_str(&text).unwrap_or_else(|error| panic!("{text}: {error}"))
}

/// The words of every line: a table without regard to its column widths.
fn words(text: &str) -> Vec<Vec<&str>> {
    text.lines()
        .map(|line| line.split_whitespace().collect())
        .collect()
}

fn integer(value: &Value) -> i64 {
    value
        .as_i64()
        .unwrap_or_else(|| panic!("not an integer: {value}"))
}

/// The one number a `b4 sql` query returns.
fn sql_count(query: &str) -> i64 {
    let rows = rows(&["sql", query]);
    let row = rows[0].as_object().unwrap();
    integer(row.values().next().unwrap())
}

// --- the manifest ---

/// What the manifest says about one category name (`None`: no category).
struct Expected {
    /// Booking records that are not the master of a split or batch: what
    /// `bookings.counted` selects.
    counted: i64,
    /// Their net sum, known when none of them is pending.
    net: Option<Decimal>,
    special: i64,
    budget_amount: Option<Decimal>,
}

fn manifest() -> Value {
    json(MANIFEST)
}

/// The number of masters over all accounts.
fn masters(manifest: &Value) -> i64 {
    manifest["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|account| integer(&account["masters"]["count"]))
        .sum()
}

/// The manifest's categories by name.
///
/// The manifest counts all booking records of a name (`booking_records`) and
/// the booked ones that are no master (`booked`). b4 counts what is no
/// master, booked or not. The masters of the demo vault are all among its
/// samples, with their category, which gives the number to take off.
fn expected() -> BTreeMap<Option<String>, Expected> {
    let manifest = manifest();
    let mut masters_of: BTreeMap<Option<String>, i64> = BTreeMap::new();
    for sample in manifest["samples"].as_array().unwrap() {
        let about = sample["about"].as_str().unwrap();
        if about.starts_with("split master") || about.starts_with("batch master") {
            let category = sample["fields"]["category"].as_str().map(str::to_owned);
            *masters_of.entry(category).or_default() += 1;
        }
    }
    assert_eq!(
        masters_of.values().sum::<i64>(),
        masters(&manifest),
        "the samples do not show every master"
    );

    manifest["categories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let name = entry["category"].as_str().map(str::to_owned);
            let counted =
                integer(&entry["booking_records"]) - masters_of.get(&name).copied().unwrap_or(0);
            let booked = &entry["booked"];
            let net = (integer(&booked["count"]) == counted).then(|| decimal(&booked["net"]));
            let expected = Expected {
                counted,
                net,
                special: entry["special_category"].as_i64().unwrap_or(0),
                budget_amount: entry.get("budget_amount").map(decimal),
            };
            (name, expected)
        })
        .collect()
}

// --- categories ---

#[test]
fn every_category_has_the_bookings_of_the_manifest() {
    let expected = expected();
    let rows = rows(&["categories", "list"]);
    // The demo vault has one currency, so one row per category, and no
    // booking names a category the catalogue lacks.
    assert_eq!(rows.len(), expected.len());

    let mut seen = 0;
    for row in &rows {
        let name = row["full_name"].as_str().map(str::to_owned);
        let label = name.clone().unwrap_or_else(|| "(none)".to_owned());
        let entry = expected
            .get(&name)
            .unwrap_or_else(|| panic!("{label} is not in the manifest"));
        seen += 1;

        let kind = if name.is_some() {
            "category"
        } else {
            "uncategorized"
        };
        assert_eq!(row["kind"], kind, "{label}");
        assert_eq!(integer(&row["booking_count"]), entry.counted, "{label}");
        if entry.counted == 0 {
            assert_eq!(row["sum"], Value::Null, "{label}");
        } else {
            assert_eq!(row["currency"], "EUR", "{label}");
            if let Some(net) = entry.net {
                assert_eq!(decimal(&row["sum"]), net, "{label}");
            }
        }

        // Everything below: the names that continue this one after a colon.
        let Some(name) = name else {
            assert_eq!(row["booking_count_with_children"], row["booking_count"]);
            assert_eq!(row["depth"], 0);
            continue;
        };
        let prefix = format!("{name}:");
        let family = || {
            expected.iter().filter(|(other, _)| {
                other
                    .as_deref()
                    .is_some_and(|other| other == name || other.starts_with(&prefix))
            })
        };
        let with_children: i64 = family().map(|(_, entry)| entry.counted).sum();
        assert_eq!(
            integer(&row["booking_count_with_children"]),
            with_children,
            "{label}"
        );
        if with_children > 0 && family().all(|(_, entry)| entry.net.is_some()) {
            let net: Decimal = family().filter_map(|(_, entry)| entry.net).sum();
            assert_eq!(decimal(&row["sum_with_children"]), net, "{label}");
        }

        let (parent, last) = match name.rsplit_once(':') {
            Some((parent, last)) => (Value::from(parent), last),
            None => (Value::Null, name.as_str()),
        };
        assert_eq!(row["parent"], parent, "{label}");
        assert_eq!(row["name"], last, "{label}");
        assert_eq!(
            integer(&row["depth"]),
            i64::try_from(name.matches(':').count()).unwrap(),
            "{label}"
        );
        let special = ["none", "reclassification", "excluded", "vat"];
        assert_eq!(
            row["special"],
            special[usize::try_from(entry.special).unwrap()],
            "{label}"
        );
        assert_eq!(
            decimal(&row["budget_amount"]),
            entry.budget_amount.unwrap(),
            "{label}"
        );
        assert_eq!(row["hidden_by_banking4"], false, "{label}");
    }
    assert_eq!(seen, expected.len());
}

#[test]
fn the_counts_add_up_to_the_counted_bookings() {
    let manifest = manifest();
    let counted = integer(&manifest["totals"]["booking"]) - masters(&manifest);
    assert_eq!(counted, 632);
    assert_eq!(
        sql_count("select count(*) from bookings where counted"),
        counted
    );

    let rows = rows(&["categories", "list"]);
    let own: i64 = rows.iter().map(|row| integer(&row["booking_count"])).sum();
    assert_eq!(own, counted);
    // The top level holds every booking once more, through its children.
    let top: i64 = rows
        .iter()
        .filter(|row| row["depth"] == 0)
        .map(|row| integer(&row["booking_count_with_children"]))
        .sum();
    assert_eq!(top, counted);

    let uncategorized: Vec<&Value> = rows
        .iter()
        .filter(|row| row["kind"] == "uncategorized")
        .collect();
    assert_eq!(uncategorized.len(), 1);
    assert_eq!(
        integer(&uncategorized[0]["booking_count"]),
        sql_count("select count(*) from bookings where counted and category is null")
    );
    assert_eq!(rows.last().unwrap()["kind"], "uncategorized");
}

#[test]
fn the_category_table_is_an_indented_tree() {
    let text = printed(&["categories", "list"]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1 + expected().len());
    assert_eq!(
        lines[0],
        "CATEGORY                BOOKINGS            SUM  WITH CHILDREN  SUM WITH CHILDREN  NOTE"
    );
    // Special categories first (`(` sorts before letters), with their mark.
    assert_eq!(
        lines[1..4],
        [
            "(nicht auswerten)              4   15440.00 EUR              4       15440.00 EUR  excluded",
            "(Umbuchung)                  108       0.00 EUR            108           0.00 EUR  reclassification",
            "(Umsatzsteuer)                 0                             0                     vat",
        ]
    );
    // Three levels; a parent without bookings of its own; `Ö` after `K`.
    let from = |start: &str| {
        lines
            .iter()
            .position(|line| line.starts_with(start))
            .unwrap()
    };
    let mobility = from("Mobilität");
    assert_eq!(
        lines[mobility..mobility + 3],
        [
            "Mobilität                      6    -407.82 EUR             60       -3675.44 EUR",
            "  Kraftstoff                  35   -2273.62 EUR             35       -2273.62 EUR",
            "  ÖPNV                        19    -994.00 EUR             19        -994.00 EUR",
        ]
    );
    let insurance = from("Versicherungen");
    assert_eq!(
        lines[insurance..],
        [
            "Versicherungen                 0                            20       -1013.40 EUR",
            "  Haftpflicht                  2    -136.80 EUR              2        -136.80 EUR",
            "  Kfz                         18    -876.60 EUR             18        -876.60 EUR",
            "Wohnen                         1    -240.00 EUR             99      -21277.99 EUR",
            "  Internet und Telefon        36    -989.64 EUR             36        -989.64 EUR",
            "  Miete                       18  -17100.00 EUR             18      -17100.00 EUR",
            "  Nebenkosten                  8    -332.35 EUR             44       -2948.35 EUR",
            "    Heizung                   18   -1182.00 EUR             18       -1182.00 EUR",
            "    Strom                     18   -1434.00 EUR             18       -1434.00 EUR",
            "(uncategorized)               36   -1796.82 EUR             36       -1796.82 EUR",
        ]
    );
    assert!(!text.contains('\u{1b}'), "colour in the table");

    // Without the header the columns are as wide as their cells only.
    let bare = printed(&["categories", "list", "--no-header"]);
    assert_eq!(words(&bare), words(&lines[1..].join("\n")));
    assert!(bare.contains("\n    Strom "), "{bare}");
}

#[test]
fn the_category_formats_carry_the_same_rows() {
    let rows = rows(&["categories", "list"]);
    let keys: Vec<&str> = rows[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut sorted: Vec<&str> = CATEGORY_COLUMNS.split(',').collect();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);

    let jsonl = printed(&["categories", "list", "-o", "jsonl"]);
    let lines: Vec<Value> = jsonl.lines().map(json).collect();
    assert_eq!(lines, rows);
    // Keys come in column order.
    assert!(
        jsonl.starts_with("{\"full_name\":\"(nicht auswerten)\",\"name\":"),
        "{jsonl}"
    );

    let csv = printed(&["categories", "list", "-o", "csv"]);
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], CATEGORY_COLUMNS);
    assert_eq!(lines.len(), 1 + rows.len());
    assert!(
        lines.contains(
            &"Wohnen:Nebenkosten:Strom,Strom,Wohnen:Nebenkosten,2,category,EUR,18,-1434.00,18,\
              -1434.00,month,0,none,expense,false"
        ),
        "{csv}"
    );
    assert_eq!(
        *lines.last().unwrap(),
        ",,,0,uncategorized,EUR,36,-1796.82,36,-1796.82,,,,,"
    );
    let bare = printed(&["categories", "list", "-o", "csv", "--no-header"]);
    assert_eq!(bare.lines().collect::<Vec<_>>(), lines[1..]);
}

// --- payees ---

#[test]
fn every_payee_of_the_manifest_is_listed() {
    let manifest = manifest();
    let count = usize::try_from(integer(&manifest["totals"]["payee"])).unwrap();
    let rows = rows(&["payees", "list"]);
    assert_eq!(rows.len(), count);

    // Ordered by name, ignoring case.
    let names: Vec<String> = rows
        .iter()
        .map(|row| row["name"].as_str().unwrap().to_lowercase())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);

    // The payees the manifest shows in full are there with their fields.
    let samples: Vec<&Value> = manifest["samples"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|sample| sample["record_type"] == "payee")
        .map(|sample| &sample["fields"])
        .collect();
    assert!(!samples.is_empty());
    for fields in samples {
        let row = rows
            .iter()
            .find(|row| row["id"] == fields["oid"])
            .unwrap_or_else(|| panic!("no payee {}", fields["oid"]));
        for key in [
            "name",
            "iban",
            "bic",
            "town",
            "bank_name",
            "category",
            "notes",
        ] {
            assert_eq!(row[key], fields[key], "{key} of {}", fields["name"]);
        }
        // The count is a match by name.
        let name = fields["name"].as_str().unwrap();
        assert!(!name.contains('\''));
        assert_eq!(
            integer(&row["booking_count"]),
            sql_count(&format!(
                "select count(*) from bookings where counted and payee_name = '{name}'"
            )),
            "{name}"
        );
    }
    assert!(rows.iter().any(|row| integer(&row["booking_count"]) > 0));
}

#[test]
fn the_payee_formats_carry_the_same_rows() {
    let rows = rows(&["payees", "list"]);

    let table = printed(&["payees", "list"]);
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines.len(), 1 + rows.len());
    let titles: Vec<&str> = lines[0].split_whitespace().collect();
    assert_eq!(titles, ["NAME", "IBAN", "BIC", "BANK", "TOWN", "BOOKINGS"]);
    for (line, row) in lines[1..].iter().zip(&rows) {
        assert!(line.starts_with(row["name"].as_str().unwrap()), "{line}");
        assert!(
            line.ends_with(&format!(" {}", row["booking_count"])),
            "{line}"
        );
    }
    assert!(!table.contains('\u{1b}'), "colour in the table");
    let bare = printed(&["payees", "list", "--no-header"]);
    assert_eq!(words(&bare), words(&lines[1..].join("\n")));

    let jsonl = printed(&["payees", "list", "-o", "jsonl"]);
    assert_eq!(jsonl.lines().map(json).collect::<Vec<_>>(), rows);

    let csv = printed(&["payees", "list", "-o", "csv"]);
    let header = csv.lines().next().unwrap();
    assert!(
        header.starts_with("id,created_at,updated_at,name,iban,bic,"),
        "{header}"
    );
    assert!(
        header.ends_with(",local_instrument_code,booking_count"),
        "{header}"
    );
    // A purpose or a note of two lines is one quoted field of two lines.
    let records = csv.matches("\n7570").count();
    assert_eq!(records, rows.len(), "{csv}");
}

#[test]
fn the_payee_filter_is_a_literal_substring_without_regard_to_case() {
    let names = |filter: &str| -> Vec<String> {
        rows(&["payees", "list", "--filter", filter])
            .iter()
            .map(|row| row["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(names("HAUSVERWALTUNG"), ["Hausverwaltung Beispiel GmbH"]);
    // By IBAN, with and without blanks; by a word of the notes.
    assert_eq!(
        names("de83999900030004015838"),
        ["Hausverwaltung Beispiel GmbH"]
    );
    assert_eq!(
        names("DE83 9999 0003 0004"),
        ["Hausverwaltung Beispiel GmbH"]
    );
    assert_eq!(names("zahlungsziel"), ["Zahnarztpraxis Dr. Beispiel"]);
    // Non-ASCII letters fold too.
    let bakeries = names("BÄCKEREI");
    assert!(
        bakeries.contains(&"Bäckerei Müller".to_owned()),
        "{bakeries:?}"
    );

    let all = rows(&["payees", "list"]).len();
    let some = names("beispiel").len();
    assert!(0 < some && some < all, "{some} of {all}");

    // Nothing is a wildcard, and nothing is SQL.
    for filter in ["%", "_", "Haus%", "H_usverwaltung", "' or 1=1 --"] {
        let output = run(&["payees", "list", "--filter", filter]);
        assert_eq!(output.status.code(), Some(0), "{filter}");
        assert_eq!(stdout(&output), "", "{filter}");
        assert_eq!(stderr(&output), "no rows\n", "{filter}");
    }
    let output = run(&["payees", "list", "--quiet", "--filter", "%"]);
    assert_eq!(
        (stdout(&output), stderr(&output)),
        (String::new(), String::new())
    );
    assert_eq!(
        printed(&["payees", "list", "-o", "json", "--filter", "%"]),
        "[]\n"
    );

    let output = run(&["payees", "list", "--filter", ""]);
    assert_eq!(output.status.code(), Some(2));
}

// --- without a database ---

#[test]
fn both_commands_fail_plainly_without_a_database() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("b4.duckdb");
    for command in [["payees", "list"], ["categories", "list"]] {
        let output = b4(&command)
            .arg("--db")
            .arg(&missing)
            .output()
            .expect("cannot run b4");
        assert_eq!(output.status.code(), Some(1), "{command:?}");
        assert_eq!(stdout(&output), "", "{command:?}");
        let message = stderr(&output);
        assert!(
            message.starts_with("error: there is no database at ")
                && message.contains("run `b4 decrypt <VAULT>`"),
            "{message}"
        );
    }
}
