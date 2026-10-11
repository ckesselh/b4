//! The master data and portfolio records against the record fixtures.
//!
//! `tests/fixtures/records/<record>.bin` is a collection file and
//! `<record>.json` describes it: the values that were written (`written`)
//! and the expected values of reading the bytes (`read`). The well-formed
//! records were produced with Banking4; the others are cases with their
//! expected results. See `tests/fixtures/records/README-masterdata.md`.
//!
//! Every record is checked three ways:
//!
//! 1. The decoded struct, serialised to JSON, equals what
//!    [`Kind::from_banking4`] builds from the description: the `read` value
//!    for every field of the document, the `written` value (through the
//!    writer's rules) for every `stored_*` field. Every field of the
//!    description is either compared or listed in [`Kind::NOT_COMPARED`]
//!    with the reason, and every field of the struct has an expectation.
//! 2. A second reader, driven by the field tables of
//!    docs/format/05-master-data-and-portfolio.md as transcribed in
//!    [`Kind::TABLE`], with the "After reading" rules written out again in
//!    [`Kind::after_reading`], gives the same JSON.
//! 3. The same comparison holds for the record cut after every byte, which
//!    is how a record of an older program version looks (02 §3).
//!
//! Where `b4` deliberately differs from Banking4 the difference is
//! spelled out in [`DEVIATIONS`] rather than hidden in the comparison.

// Test helpers outside `#[test]` functions unwrap freely: a failure is a
// broken fixture, and the panic message names it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use b4::model::base::{load_collection, load_collection_strict, DecodeError, Record};
use b4::model::masterdata::{
    canonical_rules_text, category_loader_view, duplicate_isins, exchange_rate_view,
    missing_special_categories, normalise_category, normalise_category_for, parse_category_rules,
    parse_security_id, parse_splits, split_address, Asset, BudgetPeriod, Category, CategoryRule,
    DeliveryDirection, ExchangeRate, ExchangeRateDrop, Language, Mandate, MandateDropReason,
    MandateFlags, Payee, PayeeFlags, PayeeKind, Payer, PortfolioItem, PortfolioItemState,
    PortfolioTransaction, PortfolioTransactionState, PriceType, QuantityType, SecurityIdKind,
    SpecialCategory, StockInstrument, StockPrice, TaxExemption, TotalsBucket, TransactionActivity,
    VopStatus, APPLICATION_DEFAULT_CURRENCY,
};
use b4::wire::{records, text_lines, Reader, ValueProblem, WireError};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------- fixtures

/// `<record>.json`.
#[derive(Deserialize)]
struct Fixture {
    record: String,
    collection_size: usize,
    fields: Vec<String>,
    fields_in_memory_only: Vec<String>,
    records: Vec<FixtureRecord>,
}

#[derive(Deserialize)]
struct FixtureRecord {
    index: usize,
    about: String,
    offset: usize,
    record_size: usize,
    record_hex: String,
    written: Map<String, Value>,
    read: Map<String, Value>,
}

fn fixture_path(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/records")
        .join(file)
}

fn load<T: Kind>() -> (Fixture, Vec<u8>) {
    let json = fixture_path(&format!("{}.json", T::FIXTURE));
    let text = fs::read_to_string(&json).unwrap_or_else(|e| panic!("{}: {e}", json.display()));
    let fixture: Fixture =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", json.display()));
    let bin = fixture_path(&format!("{}.bin", T::FIXTURE));
    let bytes = fs::read(&bin).unwrap_or_else(|e| panic!("{}: {e}", bin.display()));
    assert_eq!(fixture.record, T::FIXTURE);
    assert_eq!(bytes.len(), fixture.collection_size, "{}", bin.display());
    (fixture, bytes)
}

fn unhex(text: &str) -> Vec<u8> {
    assert!(text.len() % 2 == 0, "odd number of hex digits");
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

// --------------------------------------------------- the table-driven reader

/// A wire primitive of 02 §4, as the tables of document 05 name them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prim {
    I32,
    U8,
    U16,
    Bool,
    Decimal,
    Str,
    Code(usize),
    DateRaw,
    Ticks,
    TimeHms,
}
use Prim::{Bool, Code, DateRaw, Str, Ticks, TimeHms, I32, U16, U8};

impl Prim {
    /// The spelling in the "Wire primitive" column.
    fn doc_name(self) -> String {
        match self {
            I32 => "i32".to_owned(),
            U8 => "u8".to_owned(),
            U16 => "u16".to_owned(),
            Bool => "bool".to_owned(),
            Prim::Decimal => "decimal".to_owned(),
            Str => "string".to_owned(),
            Code(width) => format!("code({width})"),
            DateRaw => "date_raw".to_owned(),
            Ticks => "ticks".to_owned(),
            TimeHms => "time_hms".to_owned(),
        }
    }

    /// Read one value and give it the JSON form the structs serialise to.
    fn read(self, r: &mut Reader<'_>) -> Result<Value, WireError> {
        const F: &str = "field";
        Ok(match self {
            I32 => json!(r.i32()),
            U8 => json!(r.u8()),
            U16 => json!(r.u16()),
            Bool => json!(r.bool()),
            Prim::Decimal => json!(r.decimal(F)?.to_string()),
            Str => json!(r.string(F)?),
            Code(width) => json!(r.code(width)),
            DateRaw => json!(r.date_raw()),
            Ticks => json!(r.ticks(F)?),
            TimeHms => json!(r.time_hms(F)?),
        })
    }
}

/// One row of a field table: own field name and primitive.
type Row = (&'static str, Prim);

/// The values of one record by the names of its table, with helpers for the
/// "After reading" rules.
struct Wire(Map<String, Value>);

impl Wire {
    fn read(table: &[Row], r: &mut Reader<'_>) -> Result<Self, WireError> {
        table
            .iter()
            .map(|&(name, prim)| Ok((name.to_owned(), prim.read(r)?)))
            .collect::<Result<_, _>>()
            .map(Self)
    }

    fn get(&self, name: &str) -> &Value {
        self.0
            .get(name)
            .unwrap_or_else(|| panic!("no field {name}"))
    }

    fn text(&self, name: &str) -> Option<&str> {
        self.get(name).as_str()
    }

    fn int(&self, name: &str) -> i64 {
        self.get(name).as_i64().unwrap()
    }

    fn decimal(&self, name: &str) -> Decimal {
        self.get(name).as_str().unwrap().parse().unwrap()
    }

    fn set(&mut self, name: &str, value: impl Serialize) {
        self.0
            .insert(name.to_owned(), serde_json::to_value(value).unwrap());
    }

    fn remove(&mut self, name: &str) -> Value {
        self.0
            .remove(name)
            .unwrap_or_else(|| panic!("no field {name}"))
    }

    fn rename(&mut self, from: &str, to: &str) {
        let value = self.remove(from);
        self.0.insert(to.to_owned(), value);
    }

    /// Replace the number of a coded field by the name `E` gives it.
    fn coded<E: From<R> + Serialize, R: TryFrom<i64>>(&mut self, name: &str) {
        let value = coded::<E, R>(self.get(name));
        self.set(name, value);
    }

    /// `stored_isin`, `isin` and `isin_kind` from the wire field `isin`.
    fn identifier(&mut self) {
        self.rename("isin", "stored_isin");
        let id = parse_security_id(self.text("stored_isin"));
        self.set("isin", id.canonical_isin);
        self.set("isin_kind", id.kind);
    }
}

/// The JSON form of the enumeration or flag set `E` for a number.
fn coded<E: From<R> + Serialize, R: TryFrom<i64>>(number: &Value) -> Value {
    let number = number
        .as_i64()
        .unwrap_or_else(|| panic!("{number} is no integer"));
    let raw = R::try_from(number)
        .ok()
        .unwrap_or_else(|| panic!("{number} does not fit the wire type"));
    serde_json::to_value(E::from(raw)).unwrap()
}

// ------------------------------------------------ the fixture's expectations

/// Builds the expected struct JSON of one fixture record and keeps track of
/// which fields of the description were used.
struct Expect<'a> {
    record: &'a FixtureRecord,
    used: BTreeSet<&'static str>,
    out: Map<String, Value>,
}

impl Expect<'_> {
    /// A value of the description's `read` object.
    fn read(&mut self, field: &'static str) -> Value {
        self.used.insert(field);
        self.record
            .read
            .get(field)
            .unwrap_or_else(|| panic!("the description has no read field {field}"))
            .clone()
    }

    /// A value that was written.
    fn written(&self, field: &str) -> Value {
        self.record
            .written
            .get(field)
            .unwrap_or_else(|| panic!("the description has no written field {field}"))
            .clone()
    }

    fn put(&mut self, key: &str, value: impl Serialize) {
        self.out
            .insert(key.to_owned(), serde_json::to_value(value).unwrap());
    }

    /// Fields whose JSON form is the same in the description and the struct:
    /// texts, plain numbers, booleans, raw dates, timestamps, times, lines.
    fn same(&mut self, fields: &[&'static str]) {
        for &field in fields {
            let value = self.read(field);
            self.put(field, value);
        }
    }

    /// Decimal fields, as text with the stored number of decimals.
    fn decimals(&mut self, fields: &[&'static str]) {
        for &field in fields {
            let value = unsigned_zero(&self.read(field));
            self.put(field, value);
        }
    }

    /// `stored_<field>` decimals: what the writer was given is what it wrote.
    fn stored_decimals(&mut self, fields: &[&str]) {
        for &field in fields {
            let value = unsigned_zero(&self.written(field));
            self.put(&format!("stored_{field}"), value);
        }
    }

    /// An enumeration or flag set, a number in the description.
    fn coded<E: From<R> + Serialize, R: TryFrom<i64>>(&mut self, field: &'static str) {
        let value = coded::<E, R>(&self.read(field));
        self.put(field, value);
    }

    /// `stored_<field>` of an enumeration: the written number cut to the
    /// width of the wire (05 §8.2 "Narrowing").
    fn stored_coded<E: From<R> + Serialize, R: TryFrom<i64>>(&mut self, field: &str, mask: i64) {
        let number = self.written(field).as_i64().unwrap() & mask;
        self.put(&format!("stored_{field}"), coded::<E, R>(&json!(number)));
    }

    /// `stored_<field>` of a text: an empty text is written as "no value".
    fn stored_text(&mut self, field: &str) {
        let value = match self.written(field) {
            Value::String(text) if text.is_empty() => Value::Null,
            other => other,
        };
        self.put(&format!("stored_{field}"), value);
    }

    /// `stored_<field>` of a `code(n)`: what reads back after the writer cut
    /// the text to `width` characters of one byte each (02 §4.9).
    fn stored_code(&mut self, field: &str, width: usize) {
        let value = match self.written(field) {
            Value::String(text) => {
                let stored: String = text
                    .chars()
                    .take(width)
                    .map(|c| char::from((u32::from(c) & 0xFF) as u8))
                    .collect();
                let stored = stored
                    .trim_end_matches('\0')
                    .trim_matches(|c: char| matches!(c, '\t'..='\r' | ' ' | '\u{85}' | '\u{a0}'));
                if stored.is_empty() {
                    Value::Null
                } else {
                    json!(stored)
                }
            }
            other => other,
        };
        self.put(&format!("stored_{field}"), value);
    }

    /// `stored_isin`, `isin` and `isin_kind` of the two records whose
    /// description holds the identifier as the stored text. The canonical
    /// ISIN and the kind are not in the description: they come from
    /// `parse_security_id`, which the identifier objects of the stock
    /// instrument fixture test (see `identifiers_are_interpreted_like_...`).
    fn identifier_text(&mut self) {
        let stored = self.read("isin");
        let id = parse_security_id(stored.as_str());
        self.put("stored_isin", stored);
        self.put("isin", id.canonical_isin);
        self.put("isin_kind", id.kind);
    }
}

/// The description shows a stored negative zero as `-0` or `-0.00`; a
/// `Decimal` has no negative zero and prints `0` or `0.00`. The two are the
/// same number with the same scale.
fn unsigned_zero(decimal: &Value) -> Value {
    let text = decimal
        .as_str()
        .unwrap_or_else(|| panic!("{decimal} is no decimal"));
    match text.strip_prefix('-') {
        Some(digits) if digits.chars().all(|c| c == '0' || c == '.') => json!(digits),
        _ => json!(text),
    }
}

/// A place where `b4` decodes a fixture record to another value than
/// Banking4 does, on purpose.
struct Deviation {
    fixture: &'static str,
    index: usize,
    field: &'static str,
    /// What `b4` gives; `None` is JSON null.
    b4: Option<&'static str>,
    reason: &'static str,
}

const DEVIATIONS: &[Deviation] = &[Deviation {
    fixture: "payee",
    index: 25,
    field: "category",
    b4: None,
    reason: "the fixture's values hold for a German Banking4, which knows only the German pseudo \
             category names and keeps the English `(automatic)` as a name; b4 does not know the \
             language of a vault and treats the names of all three languages as pseudo \
             categories (04 §5.7)",
}];

// -------------------------------------------------------------- the kinds

/// A record type of this test: where its fixture is, its field table, and
/// the two independent sources of the expected struct.
trait Kind: Record + Serialize + PartialEq + std::fmt::Debug {
    /// Base name of the fixture files.
    const FIXTURE: &'static str;
    /// Section of document 05 with the field table.
    const SECTION: &'static str;
    /// The field table of that section: name and primitive of every
    /// position, in wire order.
    const TABLE: &'static [Row];
    /// Fields of the description that are not compared, with the reason.
    const NOT_COMPARED: &'static [(&'static str, &'static str)] = &[];

    /// The expected struct from the description of a fixture record.
    fn from_banking4(e: &mut Expect<'_>);

    /// The expected struct from the wire values of the table: the "After
    /// reading" rules of the section, written out a second time.
    fn after_reading(w: &mut Wire);
}

const BASE: [Row; 2] = [("oid", I32), ("update_stamp", I32)];

/// `[oid, update_stamp, rows...]` as a constant table.
macro_rules! table {
    ($($name:literal: $prim:expr),+ $(,)?) => {
        &[BASE[0], BASE[1], $(($name, $prim)),+]
    };
}

impl Kind for Payee {
    const FIXTURE: &'static str = "payee";
    const SECTION: &'static str = "3.1";
    const TABLE: &'static [Row] = table![
        "name": Str, "account_number": Str, "bank_code": Str, "purpose": Str, "category": Str,
        "iban": Str, "bic": Str, "notes": Str, "address": Str, "country": Code(2),
        "bank_name": Str, "bank_address": Str, "mandate_id": Str,
        "mandate_signature_date": DateRaw, "reserved_text": Str, "currency": Code(3),
        "amount": Prim::Decimal, "email": Str, "customer_number": Str, "payee_kind": U16,
        "local_instrument_code": Str, "mandate_revocation_date": DateRaw, "payee_flags": U16,
        "vop_status": U8, "vop_matched_name": Str,
    ];
    const NOT_COMPARED: &'static [(&'static str, &'static str)] = &[
        ("name_digest", "exists in memory only, not stored (05 §3.1)"),
        ("initials", "exists in memory only, not stored (05 §3.1)"),
    ];

    fn from_banking4(e: &mut Expect<'_>) {
        e.same(&[
            "oid",
            "update_stamp",
            "name",
            "account_number",
            "bank_code",
            "purpose",
            "category",
            "iban",
            "bic",
            "notes",
            "street",
            "town",
            "country",
            "bank_name",
            "bank_street",
            "bank_town",
            "mandate_id",
            "mandate_signature_date",
            "currency",
            "email",
            "customer_number",
            "local_instrument_code",
            "mandate_revocation_date",
            "vop_matched_name",
        ]);
        e.decimals(&["amount"]);
        e.coded::<PayeeKind, u16>("payee_kind");
        e.stored_coded::<PayeeKind, u16>("payee_kind", 0xFFFF);
        e.coded::<PayeeFlags, u16>("payee_flags");
        e.coded::<VopStatus, u8>("vop_status");
    }

    fn after_reading(w: &mut Wire) {
        for (wire, street, town) in [
            ("address", "street", "town"),
            ("bank_address", "bank_street", "bank_town"),
        ] {
            let (first, second) = split_address(w.text(wire));
            w.remove(wire);
            w.set(street, first);
            w.set(town, second);
        }
        let category = normalise_category(w.text("category"));
        w.set("category", category);
        w.remove("reserved_text");
        let kind = w.int("payee_kind");
        w.set("stored_payee_kind", coded::<PayeeKind, u16>(&json!(kind)));
        let kept = if [0, 1, 2, 4].contains(&kind) {
            kind
        } else {
            0
        };
        w.set("payee_kind", coded::<PayeeKind, u16>(&json!(kept)));
        w.coded::<PayeeFlags, u16>("payee_flags");
        w.coded::<VopStatus, u8>("vop_status");
    }
}

impl Kind for Payer {
    const FIXTURE: &'static str = "payer";
    const SECTION: &'static str = "3.2";
    const TABLE: &'static [Row] = table![
        "party_oid": I32, "party_name": Str, "mandate_id": Str,
        "mandate_signature_date": DateRaw, "mandate_revocation_date": DateRaw,
        "local_instrument_code": Str, "notes": Str, "category": Str,
    ];

    fn from_banking4(e: &mut Expect<'_>) {
        e.same(&[
            "oid",
            "update_stamp",
            "party_oid",
            "party_name",
            "mandate_id",
            "mandate_signature_date",
            "mandate_revocation_date",
            "local_instrument_code",
            "notes",
            "category",
        ]);
    }

    fn after_reading(w: &mut Wire) {
        let category = normalise_category(w.text("category"));
        w.set("category", category);
    }
}

/// The rules of a canonical rule text as Banking4 writes it (05 §4.5), in
/// the order `parse_category_rules` gives. The canonical form needs no
/// character table: rules are separated by blanks, keywords by `&`.
fn rules_of_canonical_text(text: Option<&str>) -> Vec<CategoryRule> {
    let mut rules: Vec<CategoryRule> = text
        .unwrap_or_default()
        .split(' ')
        .filter(|rule| !rule.is_empty())
        .map(|rule| {
            let (negative, keywords) = match rule.strip_prefix('~') {
                Some(keywords) => (true, keywords),
                None => (false, rule),
            };
            let mut keywords: Vec<String> = keywords.split('&').map(str::to_owned).collect();
            keywords.sort();
            CategoryRule { negative, keywords }
        })
        .collect();
    rules.sort();
    rules
}

impl Kind for Category {
    const FIXTURE: &'static str = "category";
    const SECTION: &'static str = "3.3";
    const TABLE: &'static [Row] = table![
        "full_name": Str, "layout_version": I32, "description": Str, "rules": Str,
        "reserved_text": Str, "totals_bucket": U8, "budget_period": U8,
        "budget_amount": Prim::Decimal, "vat_percent": Prim::Decimal, "icon_name": Str,
        "color_name": Str, "special_category": U8, "reserved_byte": U8,
    ];
    const NOT_COMPARED: &'static [(&'static str, &'static str)] = &[(
        "pseudo_category",
        "exists in memory only, not stored (05 §3.3)",
    )];

    fn from_banking4(e: &mut Expect<'_>) {
        e.same(&[
            "oid",
            "update_stamp",
            "full_name",
            "description",
            "icon_name",
            "color_name",
        ]);
        e.stored_text("full_name");
        // The writer always writes version 9 (05 §3.3 "Writer").
        e.put("layout_version", 9);
        // The writer stores the canonical text of the rules it holds, which
        // is what `written` shows. The reader's result is canonical text
        // again, in an order that depends on Banking4's language settings
        // (05 §4.5): the rules are compared as a set.
        e.stored_text("rules");
        let stored = e.out.remove("stored_rules").unwrap();
        e.put("rules", stored);
        let read_rules = e.read("rules");
        e.put("parsed_rules", rules_of_canonical_text(read_rules.as_str()));
        e.coded::<TotalsBucket, u8>("totals_bucket");
        e.stored_coded::<TotalsBucket, u8>("totals_bucket", 0xFF);
        e.coded::<BudgetPeriod, u8>("budget_period");
        e.stored_coded::<BudgetPeriod, u8>("budget_period", 0xFF);
        e.decimals(&["budget_amount", "vat_percent"]);
        e.stored_decimals(&["budget_amount"]);
        e.stored_text("icon_name");
        e.coded::<SpecialCategory, u8>("special_category");
    }

    fn after_reading(w: &mut Wire) {
        let version = w.int("layout_version");
        let special = w.int("special_category");
        let forced = special == 1 || special == 2;

        let stored_name = w.text("full_name").map(str::to_owned);
        let mut name = normalise_category(stored_name.as_deref());
        if forced {
            name = name.map(|name| name.rsplit(':').next().unwrap().to_owned());
        }
        w.set("stored_full_name", stored_name);
        w.set("full_name", name);

        let rules = parse_category_rules(w.text("rules"), i32::try_from(version).unwrap());
        w.set("parsed_rules", rules);

        let bucket = w.int("totals_bucket");
        w.set(
            "stored_totals_bucket",
            coded::<TotalsBucket, u8>(&json!(bucket)),
        );
        let bucket = if forced { 1 } else { bucket };
        w.set("totals_bucket", coded::<TotalsBucket, u8>(&json!(bucket)));

        let period = w.int("budget_period");
        w.set(
            "stored_budget_period",
            coded::<BudgetPeriod, u8>(&json!(period)),
        );
        let period = match (forced, version < 6) {
            (true, _) => 0,
            (false, true) => 2,
            (false, false) => period,
        };
        w.set("budget_period", coded::<BudgetPeriod, u8>(&json!(period)));

        let amount = w.remove("budget_amount");
        w.set(
            "budget_amount",
            if forced { json!("0") } else { amount.clone() },
        );
        w.set("stored_budget_amount", amount);

        let icon = w.remove("icon_name");
        let forced_icon = match special {
            1 => json!("reclass"),
            2 => json!("exclude"),
            _ => icon.clone(),
        };
        w.set("icon_name", forced_icon);
        w.set("stored_icon_name", icon);

        w.coded::<SpecialCategory, u8>("special_category");
        w.remove("reserved_text");
        w.remove("reserved_byte");
    }
}

impl Kind for Mandate {
    const FIXTURE: &'static str = "mandate";
    const SECTION: &'static str = "3.4";
    const TABLE: &'static [Row] = table![
        "creditor_id": Str, "mandate_id": Str, "category": Str, "creditor_name": Str,
        "mandate_flags": I32, "mandate_date": Ticks,
    ];

    fn from_banking4(e: &mut Expect<'_>) {
        e.same(&[
            "oid",
            "update_stamp",
            "creditor_id",
            "mandate_id",
            "category",
            "creditor_name",
            "mandate_date",
        ]);
        e.coded::<MandateFlags, i32>("mandate_flags");
    }

    fn after_reading(w: &mut Wire) {
        let category = normalise_category(w.text("category"));
        w.set("category", category);
        w.coded::<MandateFlags, i32>("mandate_flags");
    }
}

impl Kind for ExchangeRate {
    const FIXTURE: &'static str = "exchange_rate";
    const SECTION: &'static str = "3.5";
    const TABLE: &'static [Row] = table![
        "base_currency": Code(3), "target_currency": Code(3), "quote_time": Ticks,
        "quote_rate": Prim::Decimal,
    ];
    const NOT_COMPARED: &'static [(&'static str, &'static str)] = &[
        ("fixed_rate", "exists in memory only, not stored (05 §3.5)"),
        (
            "currency_class",
            "exists in memory only, not stored (05 §3.5)",
        ),
    ];

    fn from_banking4(e: &mut Expect<'_>) {
        e.same(&[
            "oid",
            "update_stamp",
            "base_currency",
            "target_currency",
            "quote_time",
        ]);
        e.decimals(&["quote_rate"]);
    }

    fn after_reading(_: &mut Wire) {}
}

/// The split lines Banking4 writes for a list of splits
/// (05 §3.6.1), which is the stored text.
fn split_text(list: &Value) -> Option<String> {
    let lines: Vec<String> = list
        .as_array()?
        .iter()
        .map(|split| {
            let date = split["date"].as_str().unwrap();
            format!(
                "{}{}{}/{}/{}/",
                &date[..4],
                &date[5..7],
                &date[8..10],
                if split["reverse"].as_bool().unwrap() {
                    "R"
                } else {
                    ""
                },
                split["factor"].as_i64().unwrap()
            )
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The JSON number of an identifier kind in the description.
fn identifier_kind(number: &Value) -> SecurityIdKind {
    match number.as_i64() {
        Some(0) => SecurityIdKind::None,
        Some(1) => SecurityIdKind::Isin,
        Some(2) => SecurityIdKind::National,
        Some(3) => SecurityIdKind::Ticker,
        Some(4) => SecurityIdKind::Bilateral,
        _ => panic!("identifier kind {number}"),
    }
}

impl Kind for StockInstrument {
    const FIXTURE: &'static str = "stock_instrument";
    const SECTION: &'static str = "3.6";
    const TABLE: &'static [Row] = table![
        "isin": Str, "name": Str, "price_currency": Code(3), "splits": Str,
    ];

    fn from_banking4(e: &mut Expect<'_>) {
        e.same(&["oid", "update_stamp", "name", "price_currency"]);

        // The description holds the identifier as an object: the text the
        // application holds, its kind, the canonical ISIN and two flags.
        let id = e.read("isin");
        e.put("isin", &id["isin"]);
        e.put("isin_kind", identifier_kind(&id["kind"]));
        // The writer stores the canonical ISIN of what it holds, or nothing
        // (05 §3.6 "Writer"); that is also the text the reader then holds.
        let stored = e.written("isin")["isin"].clone();
        assert_eq!(id["text"], stored, "the held text is the stored text");
        assert_eq!(id["is_null"], json!(stored.is_null()));
        assert_eq!(id["isin_fails"], json!(false));
        e.put("stored_isin", stored);

        // Splits: the stored text is the writer's rendering of the list it
        // was given; the reader's list has the dates as midnight timestamps.
        e.put("splits", split_text(&e.written("splits")));
        let list: Vec<Value> = e
            .read("splits")
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|split| {
                        let date = split["date"]
                            .as_str()
                            .and_then(|date| date.strip_suffix("T00:00:00.0000000"))
                            .unwrap_or_else(|| panic!("split date {}", split["date"]));
                        json!({"date": date, "reverse": split["reverse"], "factor": split["factor"]})
                    })
                    .collect()
            })
            .unwrap_or_default();
        e.put("split_list", list);
        e.put("malformed_split_lines", 0);
    }

    fn after_reading(w: &mut Wire) {
        w.identifier();
        let list = parse_splits(w.text("splits"));
        w.set("split_list", list.splits);
        w.set("malformed_split_lines", list.malformed_lines);
    }
}

impl Kind for StockPrice {
    const FIXTURE: &'static str = "stock_price";
    const SECTION: &'static str = "3.7";
    const TABLE: &'static [Row] = table!["date": DateRaw, "price": Prim::Decimal];

    fn from_banking4(e: &mut Expect<'_>) {
        e.same(&["oid", "update_stamp", "date"]);
        e.decimals(&["price"]);
    }

    fn after_reading(_: &mut Wire) {}
}

/// The five decimals of a portfolio item that the reader does not allow to
/// be negative (05 §3.8.1 rule 2).
const GUARDED: [&str; 5] = [
    "interest_rate",
    "purchase_price",
    "current_price",
    "alert_lower_limit",
    "alert_upper_limit",
];

impl Kind for PortfolioItem {
    const FIXTURE: &'static str = "portfolio_item";
    const SECTION: &'static str = "3.8";
    const TABLE: &'static [Row] = table![
        "item_state": I32, "isin": Str, "name": Str, "maturity_date": DateRaw,
        "interest_rate": Prim::Decimal, "portfolio_key": Str, "quantity": Prim::Decimal,
        "quantity_type": I32, "quantity_currency": Code(3), "price_type": I32,
        "current_price_currency": Code(3), "purchase_price": Prim::Decimal,
        "purchase_price_date": DateRaw, "current_price": Prim::Decimal,
        "current_price_date": DateRaw, "current_price_time": TimeHms,
        "purchase_price_currency": Code(3), "alert_lower_limit": Prim::Decimal,
        "alert_upper_limit": Prim::Decimal, "blocked_until_date": DateRaw,
        "additional_info": Str,
    ];

    fn from_banking4(e: &mut Expect<'_>) {
        e.same(&[
            "oid",
            "update_stamp",
            "name",
            "maturity_date",
            "portfolio_key",
            "quantity_currency",
            "current_price_currency",
            "purchase_price_date",
            "current_price_date",
            "current_price_time",
            "purchase_price_currency",
            "blocked_until_date",
            "additional_info",
        ]);
        e.identifier_text();
        e.coded::<PortfolioItemState, i32>("item_state");
        e.coded::<QuantityType, i32>("quantity_type");
        e.coded::<PriceType, i32>("price_type");
        e.decimals(&["quantity"]);
        e.decimals(&GUARDED);
        e.stored_decimals(&GUARDED);
        e.stored_code("quantity_currency", 3);
        e.stored_code("purchase_price_currency", 3);
    }

    fn after_reading(w: &mut Wire) {
        w.identifier();
        w.coded::<PortfolioItemState, i32>("item_state");
        let name = w.text("name").map(|name| name.replace('\n', " "));
        w.set("name", name);
        for field in GUARDED {
            let stored = w.remove(field);
            let negative = w_decimal(&stored) < Decimal::ZERO;
            w.set(field, if negative { json!("0") } else { stored.clone() });
            w.set(&format!("stored_{field}"), stored);
        }
        let lines: Vec<String> = text_lines(w.text("additional_info"))
            .into_iter()
            .map(str::to_owned)
            .collect();
        w.set("additional_info", lines);

        let stored = w.remove("quantity_currency");
        let units = w.int("quantity_type") == 2;
        w.set(
            "quantity_currency",
            if units { Value::Null } else { stored.clone() },
        );
        w.set("stored_quantity_currency", stored);

        let stored = w.remove("purchase_price_currency");
        let fill = w.int("price_type") == 1
            && w.decimal("purchase_price") > Decimal::ZERO
            && stored.is_null();
        let filled = match (fill, w.text("current_price_currency")) {
            (false, _) => stored.clone(),
            (true, Some(current)) => json!(current),
            (true, None) => json!(APPLICATION_DEFAULT_CURRENCY),
        };
        w.set("purchase_price_currency", filled);
        w.set("stored_purchase_price_currency", stored);

        w.coded::<QuantityType, i32>("quantity_type");
        w.coded::<PriceType, i32>("price_type");
    }
}

fn w_decimal(value: &Value) -> Decimal {
    value.as_str().unwrap().parse().unwrap()
}

impl Kind for PortfolioTransaction {
    const FIXTURE: &'static str = "portfolio_transaction";
    const SECTION: &'static str = "3.9";
    const TABLE: &'static [Row] = table![
        "transaction_id": Str, "transaction_state": I32, "delivery_direction": I32,
        "activity": I32, "isin": Str, "name": Str, "quantity": Prim::Decimal,
        "quantity_type": U8, "settlement_date": Ticks, "settlement_amount": Prim::Decimal,
        "settlement_currency": Code(3), "interest_accrued": Bool, "interest_days": U16,
        "interest_amount": Prim::Decimal, "interest_currency": Code(3), "is_read": Bool,
        "quantity_currency": Code(3), "trading_date": Ticks, "trading_price": Prim::Decimal,
        "trading_currency": Code(3),
    ];

    fn from_banking4(e: &mut Expect<'_>) {
        e.same(&[
            "oid",
            "update_stamp",
            "transaction_id",
            "name",
            "settlement_date",
            "settlement_currency",
            "interest_accrued",
            "interest_days",
            "interest_currency",
            "is_read",
            "quantity_currency",
            "trading_date",
            "trading_currency",
        ]);
        e.identifier_text();
        e.coded::<PortfolioTransactionState, i32>("transaction_state");
        e.coded::<DeliveryDirection, i32>("delivery_direction");
        e.coded::<TransactionActivity, i32>("activity");
        e.coded::<QuantityType, i32>("quantity_type");
        e.decimals(&[
            "quantity",
            "settlement_amount",
            "interest_amount",
            "trading_price",
        ]);
    }

    fn after_reading(w: &mut Wire) {
        w.identifier();
        w.coded::<PortfolioTransactionState, i32>("transaction_state");
        w.coded::<DeliveryDirection, i32>("delivery_direction");
        w.coded::<TransactionActivity, i32>("activity");
        w.coded::<QuantityType, i32>("quantity_type");
    }
}

impl Kind for TaxExemption {
    const FIXTURE: &'static str = "tax_exemption";
    const SECTION: &'static str = "3.10";
    const TABLE: &'static [Row] = table![
        "valid_from": Ticks, "valid_until": Ticks, "amount": Prim::Decimal,
        "used_amount": Prim::Decimal, "remaining_amount": Prim::Decimal,
    ];

    fn from_banking4(e: &mut Expect<'_>) {
        e.same(&["oid", "update_stamp", "valid_from", "valid_until"]);
        e.decimals(&["amount", "used_amount", "remaining_amount"]);
    }

    fn after_reading(_: &mut Wire) {}
}

impl Kind for Asset {
    const FIXTURE: &'static str = "asset";
    const SECTION: &'static str = "3.11";
    const TABLE: &'static [Row] = table![
        "asset_id": Str, "signed_amount": Prim::Decimal, "currency": Code(3),
        "deposit_date": DateRaw, "expiry_date": DateRaw, "interest_rate": Prim::Decimal,
        "interest_method": Code(1), "description": Str,
    ];

    fn from_banking4(e: &mut Expect<'_>) {
        e.same(&[
            "oid",
            "update_stamp",
            "asset_id",
            "debit",
            "currency",
            "deposit_date",
            "expiry_date",
            "interest_method",
            "description",
        ]);
        e.decimals(&["amount", "interest_rate"]);
    }

    fn after_reading(w: &mut Wire) {
        // 05 §3.11.1.
        let signed = w.decimal("signed_amount");
        w.remove("signed_amount");
        let debit = signed < Decimal::ZERO;
        w.set("amount", if debit { -signed } else { signed }.to_string());
        w.set("debit", debit);
    }
}

/// Run a generic check for each of the eleven record types.
macro_rules! for_every_kind {
    ($check:ident) => {
        $check::<Payee>();
        $check::<Payer>();
        $check::<Category>();
        $check::<Mandate>();
        $check::<ExchangeRate>();
        $check::<StockInstrument>();
        $check::<StockPrice>();
        $check::<PortfolioItem>();
        $check::<PortfolioTransaction>();
        $check::<TaxExemption>();
        $check::<Asset>();
    };
}

// ------------------------------------------------------------- comparison

fn object(record: &impl Serialize) -> Map<String, Value> {
    match serde_json::to_value(record).unwrap() {
        Value::Object(map) => map,
        other => panic!("a record serialises as an object, not as {other}"),
    }
}

/// The differences between a decoded record and its expectation, one line
/// per field. Fixture values are synthetic, so they may be printed.
fn differences(actual: &Map<String, Value>, expected: &Map<String, Value>) -> Vec<String> {
    let mut problems = Vec::new();
    for (key, value) in expected {
        match actual.get(key) {
            None => problems.push(format!("{key}: expected, but not a field of the struct")),
            Some(actual) if actual != value => {
                problems.push(format!("{key}: decoded {actual}, expected {value}"));
            }
            Some(_) => {}
        }
    }
    for key in actual.keys() {
        if !expected.contains_key(key) {
            problems.push(format!("{key}: field of the struct without an expectation"));
        }
    }
    problems
}

/// The expected struct of a table-driven read of `body`, and whether that
/// read consumed the record exactly; or the value the read fails on.
fn try_reference<T: Kind>(body: &[u8]) -> Result<(Map<String, Value>, bool), WireError> {
    let mut reader = Reader::new(body).unwrap();
    let mut wire = Wire::read(T::TABLE, &mut reader)?;
    T::after_reading(&mut wire);
    Ok((wire.0, reader.finish()?.is_exact()))
}

fn reference<T: Kind>(body: &[u8]) -> (Map<String, Value>, bool) {
    try_reference::<T>(body).unwrap_or_else(|error| panic!("{}: {error}", T::FIXTURE))
}

/// Where and why a read failed, without the name of the field: the
/// table-driven reader does not pass the names on.
fn failure(error: &WireError) -> (usize, ValueProblem) {
    match *error {
        WireError::Value {
            offset, problem, ..
        } => (offset, problem),
        WireError::Chunk { .. } => panic!("the chunk structure is intact: {error}"),
    }
}

fn decode<T: Kind>(body: &[u8]) -> T {
    let mut reader = Reader::new(body).unwrap();
    T::decode(&mut reader).unwrap_or_else(|error| panic!("{}: {error}", T::FIXTURE))
}

// ------------------------------------------------------------------ tests

fn fixture_decodes_to_the_banking4_values<T: Kind>() {
    let (fixture, bytes) = load::<T>();
    let collection = load_collection_strict::<T>(&bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", T::FIXTURE));
    assert!(collection.report.is_clean(), "{}", T::FIXTURE);
    assert!(collection.report.short_chunks.is_zero(), "{}", T::FIXTURE);
    assert_eq!(
        collection.records.len(),
        fixture.records.len(),
        "{}",
        T::FIXTURE
    );

    // The framing is the one the description gives.
    for (raw, described) in records(&bytes).zip(&fixture.records) {
        let raw = raw.unwrap();
        assert_eq!(raw.offset, described.offset);
        assert_eq!(raw.body.len(), described.record_size);
        assert_eq!(raw.body, unhex(&described.record_hex));
    }

    let mut used = BTreeSet::new();
    let mut failures = Vec::new();
    let mut deviations_met = 0;
    for (record, described) in collection.records.iter().zip(&fixture.records) {
        let mut expect = Expect {
            record: described,
            used: BTreeSet::new(),
            out: Map::new(),
        };
        T::from_banking4(&mut expect);
        for deviation in DEVIATIONS {
            if deviation.fixture == T::FIXTURE && deviation.index == described.index {
                let b4 = json!(deviation.b4);
                let banking4 = expect.out.insert(deviation.field.to_owned(), b4.clone());
                assert_ne!(banking4, Some(b4), "stale deviation: {}", deviation.reason);
                deviations_met += 1;
            }
        }
        let problems = differences(&object(record), &expect.out);
        if !problems.is_empty() {
            failures.push(format!(
                "{}[{}] {}:\n    {}",
                T::FIXTURE,
                described.index,
                described.about,
                problems.join("\n    ")
            ));
        }
        used.extend(expect.used);
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    assert_eq!(
        deviations_met,
        DEVIATIONS
            .iter()
            .filter(|d| d.fixture == T::FIXTURE)
            .count()
    );

    // Every field of the description is compared or explicitly left out.
    let not_compared: BTreeSet<&str> = T::NOT_COMPARED.iter().map(|&(field, _)| field).collect();
    assert!(used.is_disjoint(&not_compared), "{}", T::FIXTURE);
    let described: BTreeSet<&str> = fixture.fields.iter().map(String::as_str).collect();
    let accounted: BTreeSet<&str> = used.union(&not_compared).copied().collect();
    assert_eq!(accounted, described, "{}", T::FIXTURE);
    // What is left out is exactly what the description marks as not stored.
    let in_memory_only: BTreeSet<&str> = fixture
        .fields_in_memory_only
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(not_compared, in_memory_only, "{}", T::FIXTURE);
}

#[test]
fn fixtures_decode_to_the_banking4_values() {
    for_every_kind!(fixture_decodes_to_the_banking4_values);
}

fn table_reader_agrees_on<T: Kind>() {
    let (fixture, _) = load::<T>();
    for described in &fixture.records {
        let body = unhex(&described.record_hex);
        let (expected, exact) = reference::<T>(&body);
        // The table accounts for every byte the writer wrote.
        assert!(exact, "{}[{}]", T::FIXTURE, described.index);
        let problems = differences(&object(&decode::<T>(&body)), &expected);
        assert!(
            problems.is_empty(),
            "{}[{}]:\n    {}",
            T::FIXTURE,
            described.index,
            problems.join("\n    ")
        );
    }
}

#[test]
fn table_driven_reader_agrees_on_every_fixture_record() {
    for_every_kind!(table_reader_agrees_on);
}

/// A single-chunk record body: the chunk header for `payload`, then it.
fn chunk(payload: &[u8], more: bool) -> Vec<u8> {
    let size = u32::try_from(payload.len() + 4).unwrap() | if more { 0x8000_0000 } else { 0 };
    let mut body = size.to_le_bytes().to_vec();
    body.extend_from_slice(payload);
    body
}

/// Cut every fixture record of `T` after every byte and compare the decoder
/// with the table-driven reader. Returns how many cuts there were and why
/// some of them are undecodable.
fn truncations_decode_to_defaults<T: Kind>() -> (usize, Vec<ValueProblem>) {
    let (fixture, _) = load::<T>();
    let (mut cuts, mut undecodable) = (0, Vec::new());
    for described in &fixture.records {
        let body = unhex(&described.record_hex);
        let payload = &body[4..];
        for cut in 0..=payload.len() {
            cuts += 1;
            let context = format!("{}[{}] cut after {cut} bytes", T::FIXTURE, described.index);
            // The chunk ends early and with it the record.
            let short = chunk(&payload[..cut], false);
            // The chunk ends early and another chunk follows, holding the
            // rest: a single-chunk record must not read on into it.
            let mut continued = chunk(&payload[..cut], true);
            continued.extend(chunk(&payload[cut..], false));

            let decoded = T::decode(&mut Reader::new(&short).unwrap());
            let again = T::decode(&mut Reader::new(&continued).unwrap());
            assert_eq!(again, decoded, "{context}, with a second chunk");

            match (decoded, try_reference::<T>(&short)) {
                (Ok(decoded), Ok((expected, _))) => {
                    let problems = differences(&object(&decoded), &expected);
                    assert!(
                        problems.is_empty(),
                        "{context}:\n    {}",
                        problems.join("\n    ")
                    );
                    // The object id is there as soon as its bytes are.
                    let oid = if cut >= 4 {
                        described.read["oid"].as_i64().unwrap()
                    } else {
                        0
                    };
                    assert_eq!(i64::from(decoded.oid()), oid, "{context}");
                }
                // A field that is cut in the middle reads as its default and
                // does not move, so the next field reads its bytes (02 §3).
                // Those bytes can be a value no reader accepts: an hour above
                // 23, a text length below zero. Then the record is
                // undecodable, at the same field for the decoder and the
                // table-driven reader.
                (Err(DecodeError::Wire(error)), Err(expected)) => {
                    assert_eq!(failure(&error), failure(&expected), "{context}");
                    undecodable.push(failure(&error).1);
                }
                (decoded, expected) => panic!(
                    "{context}: the decoder gives {:?}, the table-driven reader {:?}",
                    decoded.map(|_| "a record"),
                    expected.map(|_| "a record")
                ),
            }
        }
    }
    (cuts, undecodable)
}

#[test]
fn records_cut_after_every_byte_decode_to_the_documented_defaults() {
    let counts = [
        truncations_decode_to_defaults::<Payee>(),
        truncations_decode_to_defaults::<Payer>(),
        truncations_decode_to_defaults::<Category>(),
        truncations_decode_to_defaults::<Mandate>(),
        truncations_decode_to_defaults::<ExchangeRate>(),
        truncations_decode_to_defaults::<StockInstrument>(),
        truncations_decode_to_defaults::<StockPrice>(),
        truncations_decode_to_defaults::<PortfolioItem>(),
        truncations_decode_to_defaults::<PortfolioTransaction>(),
        truncations_decode_to_defaults::<TaxExemption>(),
        truncations_decode_to_defaults::<Asset>(),
    ];
    let cuts: Vec<usize> = counts.iter().map(|(cuts, _)| *cuts).collect();
    assert_eq!(
        cuts,
        [3807, 437, 2151, 777, 429, 898, 261, 4213, 2438, 584, 1138]
    );
    assert_eq!(cuts.iter().sum::<usize>(), 17_133);

    // 26 of the cuts leave a value that cannot be decoded. In ten cuts of
    // the payee with the largest amount, the text after the cut amount reads
    // a length below zero out of the amount's bytes: b4 gives such a record
    // up where Banking4 reads on (02 §8 rule 7). In sixteen cuts of a
    // portfolio item the time of day reads an hour above 23 out of the cut
    // date before it, which Banking4 fails on as well.
    for (index, (_, undecodable)) in counts.iter().enumerate() {
        let expected = match index {
            0 => vec![ValueProblem::NegativeStringLength; 10],
            7 => vec![ValueProblem::TimeOutOfRange; 16],
            _ => Vec::new(),
        };
        assert_eq!(undecodable, &expected, "record type {index}");
    }
}

fn empty_record_is_all_defaults<T: Kind>() {
    let record = object(&decode::<T>(&chunk(&[], false)));
    // The names the enumerations give the number 0, and the one default
    // that is not a zero: `budget_period` of a category whose
    // `layout_version` (0 here) is below 6 (05 §3.3.1).
    let default_names = ["none", "auto", "undefined", "month"];
    for (field, value) in &record {
        let is_default = match value {
            Value::Null => true,
            Value::Bool(flag) => !flag,
            Value::Number(number) => number.as_i64() == Some(0),
            Value::String(text) => text == "0" || default_names.contains(&text.as_str()),
            Value::Array(items) => items.is_empty(),
            Value::Object(_) => false,
        };
        assert!(
            is_default,
            "{}: {field} of an empty record is {value}",
            T::FIXTURE
        );
    }
    assert_eq!(record.len(), reference::<T>(&chunk(&[], false)).0.len());
}

#[test]
fn a_record_without_any_field_decodes_to_defaults() {
    for_every_kind!(empty_record_is_all_defaults);

    let category = decode::<Category>(&chunk(&[], false));
    assert_eq!(category.budget_period, BudgetPeriod::Month);
    assert_eq!(category.stored_budget_period, BudgetPeriod::None);
    assert_eq!(category.full_name, None);
    let item = decode::<PortfolioItem>(&chunk(&[], false));
    assert_eq!(item.current_price_time, None);
    assert_eq!(item.purchase_price_currency, None);
    assert_eq!(item.isin_kind, SecurityIdKind::None);
}

/// The offset of every field of the table in a record body.
fn field_offsets<T: Kind>(body: &[u8]) -> Vec<(Row, usize)> {
    let mut reader = Reader::new(body).unwrap();
    T::TABLE
        .iter()
        .map(|&row| {
            let offset = reader.cursor();
            row.1.read(&mut reader).unwrap();
            (row, offset)
        })
        .collect()
}

fn undecodable_values_name_their_field<T: Kind>() {
    let (fixture, _) = load::<T>();
    // Record 0 has every field set.
    let body = unhex(&fixture.records[0].record_hex);
    for ((name, prim), offset) in field_offsets::<T>(&body) {
        let mut broken = body.clone();
        match prim {
            // A scale above 28 (02 §4.7).
            Prim::Decimal => broken[offset + 14] = 29,
            // A negative tick count (02 §4.13).
            Ticks => broken[offset..offset + 8].fill(0xFF),
            // Hour 24 (02 §4.14).
            TimeHms => broken[offset] = 24,
            // A negative long-form length (02 §8 rule 7).
            Str if offset + 6 <= broken.len() => broken[offset..offset + 6].fill(0xFF),
            _ => continue,
        }
        let mut reader = Reader::new(&broken).unwrap();
        let error = T::decode(&mut reader).expect_err(name);
        assert!(!error.stops_file(), "{}: {name}", T::FIXTURE);
        // The message names the field and nothing of the record.
        assert!(
            error.to_string().contains(&format!("`{name}`")),
            "{}: corrupt {name} reported as: {error}",
            T::FIXTURE
        );
    }
}

#[test]
fn a_value_banking4_fails_on_makes_the_record_undecodable_and_names_the_field() {
    for_every_kind!(undecodable_values_name_their_field);

    // In a file, the record is skipped and the others are kept.
    let (fixture, mut bytes) = load::<StockPrice>();
    let second = &fixture.records[1];
    let price = field_offsets::<StockPrice>(&unhex(&second.record_hex))[3].1;
    bytes[second.offset + 4 + price + 14] = 29;
    let collection = load_collection::<StockPrice>(&bytes);
    assert_eq!(collection.records.len(), fixture.records.len() - 1);
    assert_eq!(collection.report.undecodable, 1);
    assert_eq!(collection.report.undecodable_samples[0].index, 1);
    assert!(collection.report.stopped.is_none());
    assert_eq!(collection.records[1].oid(), fixture.records[2].read["oid"]);
}

/// One record of a collection file: the size, then the body.
fn framed(body: &[u8]) -> Vec<u8> {
    let mut record = i32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
    record.extend_from_slice(body);
    record
}

fn longer_records_leave_bytes_for_the_counters<T: Kind>() {
    let (fixture, bytes) = load::<T>();
    let original = load_collection_strict::<T>(&bytes).unwrap().records;
    let count = original.len();

    // Three unknown bytes at the end of the chunk, as a newer writer would
    // append fields, and one unknown chunk after it.
    let mut file = Vec::new();
    for described in &fixture.records {
        let mut payload = unhex(&described.record_hex)[4..].to_vec();
        payload.extend([0xAA, 0xBB, 0xCC]);
        let mut body = chunk(&payload, true);
        body.extend(chunk(&[1, 2, 3, 4, 5], false));
        file.extend(framed(&body));
    }
    let collection = load_collection::<T>(&file);
    assert_eq!(collection.records, original, "{}", T::FIXTURE);
    let report = &collection.report;
    assert_eq!(report.unread_bytes.records, count, "{}", T::FIXTURE);
    assert_eq!(report.unread_histogram.len(), 1);
    let (unread, records) = report.unread_histogram.iter().next().unwrap();
    assert_eq!((unread.chunk, unread.bytes, *records), (1, 3, count));
    assert_eq!(report.extra_chunks.records, count);
    assert_eq!(report.extra_chunk_count, count);
    assert_eq!((report.undecodable, report.stopped), (0, None));
    assert!(report.short_chunks.is_zero());
    assert_eq!(report.warnings("file").len(), 2);
}

#[test]
fn records_longer_than_the_table_decode_and_are_counted() {
    for_every_kind!(longer_records_leave_bytes_for_the_counters);
}

/// xorshift64: deterministic bytes for the mutation test.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(bound).unwrap()).unwrap()
    }
}

fn mutated_records_never_panic<T: Kind>() {
    const ROUNDS_PER_RECORD: usize = 300;
    let (fixture, bytes) = load::<T>();
    let mut random = Random(0x9E37_79B9_7F4A_7C15 ^ u64::try_from(bytes.len()).unwrap());
    let mut decoded = 0_usize;
    for described in &fixture.records {
        let body = unhex(&described.record_hex);
        for _ in 0..ROUNDS_PER_RECORD {
            let mut mutated = body.clone();
            for _ in 0..=random.below(3) {
                let at = random.below(mutated.len());
                mutated[at] = random.next().to_le_bytes()[0];
            }
            // Whatever the bytes are: a record or an error, never a panic.
            if let Ok(mut reader) = Reader::new(&mutated) {
                if let Ok(record) = T::decode(&mut reader) {
                    decoded += 1;
                    // And what decodes can be serialised.
                    assert!(!object(&record).is_empty());
                }
            }
        }
    }
    assert!(
        decoded > 0,
        "{}: no mutated record decoded at all",
        T::FIXTURE
    );

    // The same over whole files, through the loader.
    for _ in 0..200 {
        let mut mutated = bytes.clone();
        for _ in 0..=random.below(8) {
            let at = random.below(mutated.len());
            mutated[at] = random.next().to_le_bytes()[0];
        }
        let cut = random.below(mutated.len() + 1);
        let collection = load_collection::<T>(&mutated[..cut]);
        assert_eq!(collection.records.len(), collection.report.decoded);
    }
}

#[test]
fn mutated_records_decode_or_fail_but_never_panic() {
    for_every_kind!(mutated_records_never_panic);
}

// ------------------------------------------------ the texts with a format

#[test]
fn identifiers_are_interpreted_like_banking4_does() {
    // The stock instrument fixture describes each identifier as Banking4
    // holds it: `/DE/123456` for every spelling of a German national number,
    // `/TS/...` for a ticker symbol and so on, with the kind and the
    // canonical ISIN Banking4 derives (05 §5.1).
    let (fixture, _) = load::<StockInstrument>();
    let mut kinds = BTreeSet::new();
    for described in &fixture.records {
        for side in [&described.written, &described.read] {
            let held = &side["isin"];
            let id = parse_security_id(held["text"].as_str());
            let context = format!("stock_instrument[{}] {}", described.index, held["text"]);
            assert_eq!(id.kind, identifier_kind(&held["kind"]), "{context}");
            assert_eq!(json!(id.canonical_isin), held["isin"], "{context}");
            assert_eq!(json!(id.derivation_failed), held["isin_fails"], "{context}");
            assert_eq!(
                json!(!id.kind.is_identifier()),
                held["is_null"],
                "{context}"
            );
            kinds.insert(id.kind.name());
        }
    }
    assert_eq!(
        kinds.into_iter().collect::<Vec<_>>(),
        ["bilateral", "isin", "national", "none", "ticker"]
    );

    // The portfolio item fixture keeps identifier texts as stored.
    let (_, bytes) = load::<PortfolioItem>();
    let items = load_collection_strict::<PortfolioItem>(&bytes)
        .unwrap()
        .records;
    assert_eq!(items[20].stored_isin.as_deref(), Some("WKN 123456"));
    assert_eq!(items[20].isin.as_deref(), Some("DE0001234565"));
    assert_eq!(items[20].isin_kind, SecurityIdKind::National);
    assert_eq!(items[20].security_id().number.as_deref(), Some("123456"));
    assert_eq!(items[0].isin, items[0].stored_isin);
    assert_eq!(items[0].isin_kind, SecurityIdKind::Isin);
}

#[test]
fn category_names_are_normalised_like_a_german_banking4_does() {
    fn check<T: Kind>(field: &str) -> usize {
        let (fixture, _) = load::<T>();
        for described in &fixture.records {
            let mut expected = described.read[field].as_str().map(str::to_owned);
            let special = described
                .read
                .get("special_category")
                .and_then(Value::as_i64);
            let normalised =
                normalise_category_for(described.written[field].as_str(), Language::German);
            // The special categories 1 and 2 keep their last level only,
            // which the `read` value already shows (05 §3.3.1).
            let normalised = match special {
                Some(1 | 2) => normalised.map(|name| name.rsplit(':').next().unwrap().to_owned()),
                _ => normalised,
            };
            assert_eq!(
                normalised,
                expected.take(),
                "{}[{}]",
                T::FIXTURE,
                described.index
            );
        }
        fixture.records.len()
    }
    let checked = check::<Payee>("category")
        + check::<Payer>("category")
        + check::<Mandate>("category")
        + check::<Category>("full_name");
    assert_eq!(checked, 38 + 9 + 11 + 26);
}

#[test]
fn rule_texts_parse_to_the_rules_banking4_holds() {
    let (fixture, bytes) = load::<Category>();
    let categories = load_collection_strict::<Category>(&bytes).unwrap().records;
    let mut with_rules = 0;
    for (category, described) in categories.iter().zip(&fixture.records) {
        let canonical = described.read["rules"].as_str();
        assert_eq!(
            category.parsed_rules,
            rules_of_canonical_text(canonical),
            "category[{}]",
            described.index
        );
        // Where every keyword is made of `a` to `z` the order is the same in
        // every language, and the canonical text must be the fixture's.
        let plain = category
            .parsed_rules
            .iter()
            .flat_map(|rule| &rule.keywords)
            .all(|keyword| keyword.bytes().all(|byte| byte.is_ascii_lowercase()));
        if plain {
            assert_eq!(
                canonical_rules_text(&category.parsed_rules).as_deref(),
                canonical,
                "category[{}]",
                described.index
            );
        }
        with_rules += usize::from(canonical.is_some());
    }
    assert_eq!(with_rules, 5);
}

#[test]
fn category_layout_version_steers_the_budget_period_and_the_rule_markers() {
    // Banking4 always stores version 9, so the fixtures have no
    // other. Patch the version of two of their records instead.
    let (fixture, _) = load::<Category>();
    let with_version = |index: usize, version: i32| {
        let mut body = unhex(&fixture.records[index].record_hex);
        let offsets = field_offsets::<Category>(&body);
        let ((name, _), offset) = offsets[3];
        assert_eq!(name, "layout_version");
        body[offset..offset + 4].copy_from_slice(&version.to_le_bytes());
        decode::<Category>(&body)
    };

    // Record 0 stores a quarterly budget and the rules
    // `flat&rent landlord ~janitor`.
    let rules = |category: &Category| {
        let rules: Vec<String> = category
            .parsed_rules
            .iter()
            .map(|rule| {
                format!(
                    "{}{}",
                    if rule.negative { "~" } else { "" },
                    rule.keywords.join("&")
                )
            })
            .collect();
        rules.join(" ")
    };
    for version in [i32::MIN, -1, 0, 3, 4, 5, 6, 7, 8, 9, 10, i32::MAX] {
        let category = with_version(0, version);
        assert_eq!(category.layout_version, version);
        assert_eq!(category.stored_budget_period, BudgetPeriod::Quarter);
        // Below version 6 the stored period is not used (05 §3.3.1).
        let period = if version < 6 {
            BudgetPeriod::Month
        } else {
            BudgetPeriod::Quarter
        };
        assert_eq!(category.budget_period, period, "version {version}");
        // Below version 4 `&` and `~` separate (05 §4.5).
        let expected = if version < 4 {
            "flat janitor landlord rent"
        } else {
            "flat&rent landlord ~janitor"
        };
        assert_eq!(rules(&category), expected, "version {version}");
        assert_eq!(
            category.rules.as_deref(),
            Some("flat&rent landlord ~janitor")
        );
    }

    // Record 2 is the special category 1 with a yearly budget: it has no
    // budget period whatever the version says.
    for version in [0, 5, 6, 9] {
        let category = with_version(2, version);
        assert_eq!(category.special_category, SpecialCategory::Reclassification);
        assert_eq!(category.stored_budget_period, BudgetPeriod::Year);
        assert_eq!(
            category.budget_period,
            BudgetPeriod::None,
            "version {version}"
        );
    }
}

// --------------------------------------------- what the application shows

#[test]
fn exchange_rate_view_marks_what_the_rate_list_would_drop() {
    let (fixture, bytes) = load::<ExchangeRate>();
    let rates = load_collection_strict::<ExchangeRate>(&bytes)
        .unwrap()
        .records;
    let views = exchange_rate_view(&rates);
    let about: Vec<&str> = fixture.records.iter().map(|r| r.about.as_str()).collect();
    let dropped: Vec<(&str, Option<ExchangeRateDrop>)> = about
        .iter()
        .copied()
        .zip(views.iter().map(|view| view.dropped))
        .collect();
    use ExchangeRateDrop::{Duplicate, InvalidTarget, NotEuroBased};
    assert_eq!(
        dropped,
        [
            ("every field set", None),
            ("no field set", Some(NotEuroBased)),
            // Turned round it is a second rate for the dollar.
            ("pair quoted the other way round", Some(Duplicate)),
            ("gold quoted per euro", None),
            ("gold quoted in ounces per euro", Some(Duplicate)),
            ("neither side is the euro", Some(NotEuroBased)),
            ("target missing", Some(InvalidTarget)),
            (
                "codes in lower case, too short and too long",
                Some(NotEuroBased)
            ),
            ("codes too long", Some(Duplicate)),
            ("negative and very small rate", None),
            ("time marked as UTC", None),
        ]
    );

    // The pair quoted the other way round: turned, and the rate inverted.
    assert_eq!(
        (
            rates[2].base_currency.as_deref(),
            rates[2].target_currency.as_deref()
        ),
        (Some("USD"), Some("EUR"))
    );
    assert_eq!(
        (
            views[2].base_currency.as_deref(),
            views[2].target_currency.as_deref()
        ),
        (Some("EUR"), Some("USD"))
    );
    let tolerance: Decimal = "0.000000000000000001".parse().unwrap();
    let off_by = |product: Decimal| (product - Decimal::ONE).abs();
    assert!(off_by(views[2].quote_rate * rates[2].quote_rate) < tolerance);
    // Gold above 1 is a price per ounce: inverted to ounces per euro.
    assert!(rates[3].quote_rate > Decimal::ONE && views[3].quote_rate < Decimal::ONE);
    assert!(off_by(views[3].quote_rate * rates[3].quote_rate) < tolerance);
    assert_eq!(views[4].quote_rate, rates[4].quote_rate);
    // Everything else is shown as stored.
    for index in [0, 9, 10] {
        assert_eq!(views[index].quote_rate, rates[index].quote_rate);
        assert_eq!(views[index].target_currency, rates[index].target_currency);
    }
}

#[test]
fn mandates_the_list_would_drop_are_marked() {
    let (fixture, bytes) = load::<Mandate>();
    let mandates = load_collection_strict::<Mandate>(&bytes).unwrap().records;
    let marked: Vec<(&str, MandateDropReason)> = mandates
        .iter()
        .zip(&fixture.records)
        .filter_map(|(mandate, described)| {
            Some((described.about.as_str(), mandate.loader_drop_reason()?))
        })
        .collect();
    assert_eq!(
        marked,
        [
            ("no field set", MandateDropReason::MissingId),
            (
                "mandate id that the list would drop",
                MandateDropReason::Offline
            ),
        ]
    );
    assert!(mandates[0].mandate_flags.contains(MandateFlags::HIDDEN));
    assert!(mandates[0].mandate_flags.contains(MandateFlags::REVOKED));
}

#[test]
fn category_loader_view_marks_dropped_records_and_repeated_special_categories() {
    let (fixture, bytes) = load::<Category>();
    let categories = load_collection_strict::<Category>(&bytes).unwrap().records;
    let marks = category_loader_view(&categories);

    let dropped: Vec<&str> = marks
        .iter()
        .zip(&fixture.records)
        .filter(|(mark, _)| mark.dropped)
        .map(|(_, described)| described.about.as_str())
        .collect();
    assert_eq!(
        dropped,
        ["no field set", "name like the pseudo category 'automatic'"]
    );

    // (index, stored role, role in the application) of every record that
    // stores a special role.
    let special: Vec<(usize, u8, u8)> = marks
        .iter()
        .zip(&categories)
        .enumerate()
        .filter(|(_, (_, category))| category.special_category != SpecialCategory::None)
        .map(|(index, (mark, category))| {
            (
                index,
                category.special_category.raw(),
                mark.special_category.raw(),
            )
        })
        .collect();
    assert_eq!(
        special,
        [
            (2, 1, 1),
            (3, 2, 2),
            (4, 3, 3),
            (5, 1, 0),
            (6, 4, 4),
            (7, 2, 0)
        ]
    );
    assert!(missing_special_categories(&categories).is_empty());
    assert_eq!(
        missing_special_categories(&categories[8..]),
        SpecialCategory::SPECIAL
    );
}

// --------------------------------------------------------------- relations

#[test]
fn portfolio_items_find_their_instrument_and_instruments_their_prices() {
    let (_, bytes) = load::<StockInstrument>();
    let instruments = load_collection_strict::<StockInstrument>(&bytes)
        .unwrap()
        .records;
    let (_, bytes) = load::<PortfolioItem>();
    let items = load_collection_strict::<PortfolioItem>(&bytes)
        .unwrap()
        .records;
    let (_, bytes) = load::<PortfolioTransaction>();
    let transactions = load_collection_strict::<PortfolioTransaction>(&bytes)
        .unwrap()
        .records;

    // By canonical ISIN, not by object id (05 §5.2).
    let instrument = items[0].instrument(&instruments).unwrap();
    assert_eq!(instrument.oid(), instruments[0].oid());
    assert_eq!(instrument.isin, items[0].isin);
    assert_ne!(instrument.oid(), items[0].oid());
    // `WKN 123456` stands for the ISIN the fourth instrument has.
    assert_eq!(
        items[20].instrument(&instruments).map(Record::oid),
        Some(instruments[3].oid())
    );
    // No identifier, no instrument.
    assert_eq!(items[1].isin, None);
    assert!(items[1].instrument(&instruments).is_none());
    assert_eq!(
        transactions[0].instrument(&instruments).map(Record::oid),
        Some(instruments[0].oid())
    );

    // The fixture stores two national numbers twice: Banking4 would fail
    // on the second of each when it loads the file (05 §3.6 "Loader").
    assert_eq!(duplicate_isins(&instruments), [5, 8]);

    // Instrument to price series: the directory `Stock<oid>`.
    assert_eq!(instruments[0].price_directory(), "Stock762506001");
    assert_eq!(
        b4::model::masterdata::stock_directory_oid(&instruments[0].price_directory()),
        Some(instruments[0].oid())
    );
}

// ------------------------------------------------------- the tables' source

/// The field tables of section 3 of document 05, as `(section, rows)`, with
/// each row as `(primitive, field name)`.
fn tables_of_the_document(markdown: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut tables: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut in_section = false;
    for line in markdown.lines() {
        if let Some(heading) = line.strip_prefix("### 3.") {
            let number = heading.split(' ').next().unwrap();
            tables.push((format!("3.{number}"), Vec::new()));
            in_section = true;
        } else if line.starts_with('#') {
            // Any other heading, a sub-section included: its tables are not
            // field tables.
            in_section = false;
        } else if in_section {
            // | position | chunk | `primitive` | `field` | ...
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            if cells.len() > 4 && cells[1].parse::<u32>().is_ok() && cells[3].starts_with('`') {
                let unquote = |cell: &str| cell.trim_matches('`').to_owned();
                let rows = &mut tables.last_mut().unwrap().1;
                assert_eq!(cells[1].parse::<usize>().unwrap(), rows.len() + 1, "{line}");
                rows.push((unquote(cells[3]), unquote(cells[4])));
            }
        }
    }
    tables
}

fn table_is_the_one_of_the_document<T: Kind>() {
    // The document is not part of every source tree the tests run in (the
    // Nix build has `src` and `tests` only); without it there is nothing to
    // compare, and the fixtures still pin the tables.
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/format/05-master-data-and-portfolio.md");
    let Ok(markdown) = fs::read_to_string(path) else {
        return;
    };
    let tables = tables_of_the_document(&markdown);
    let (_, rows) = tables
        .iter()
        .find(|(section, _)| section == T::SECTION)
        .unwrap_or_else(|| panic!("no section {} in the document", T::SECTION));
    let transcribed: Vec<(String, String)> = T::TABLE
        .iter()
        .map(|&(name, prim)| (prim.doc_name(), name.to_owned()))
        .collect();
    assert_eq!(&transcribed, rows, "section {}", T::SECTION);
}

#[test]
fn transcribed_tables_are_those_of_the_format_document() {
    for_every_kind!(table_is_the_one_of_the_document);
}
