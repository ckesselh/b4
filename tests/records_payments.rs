//! Bookings, payments and standing orders against the fixtures in
//! `tests/fixtures/records/{booking,payment,standing_order}.*`, described in
//! `README-bookings.md` next to them: records produced with Banking4, and
//! cases in forms Banking4 does not write as a whole record, with their
//! expected results.
//!
//! Every record is decoded and serialised to JSON, and every value the
//! fixture gives for it is compared with that JSON: the in-memory fields
//! expected after reading (for each account the fixture gives them for), the
//! values Banking4 computes from them, and the expected result of loading
//! the file. A fixture value that is not compared one to one has an explicit
//! rule in [`rule`] that says why, and a key of `b4`'s JSON that no fixture
//! value stands for is listed in [`Family::B4_ONLY`] and checked against the
//! stored values instead.
//!
//! Not asserted, because `b4` does not write: `rewritten` and the
//! `*.rewritten.bin` files.

// Test helpers outside `#[test]` functions unwrap freely: a failure is a
// broken fixture, and the panic message names it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use b4::model::base::{load_collection, load_collection_strict, DecodeError, Record};
use b4::model::payments::{
    apply_payment_rules, banking4_booking_order, banking4_payment_order, regroup_bookings,
    BatchBooking, Booking, BookingStatus, ColorFlag, CostBearer, EntryBits,
    PayeeVerificationChoice, Payment, PaymentBase, PaymentFlags, PaymentState, PeriodUnit,
    SchemeClass, SchemeType, SepaStatus, SplitRole, StandingOrder, StandingOrderState,
    TransactionCode, VatRateType,
};
use b4::wire::{records, Date, Reader};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{json, Map, Value};

/// The date that stands in for "today" where Banking4 sorts undated records
/// (04 §6.4). Any day after the dated fixture records gives the fixture's
/// order.
fn today() -> Date {
    Date::new(2026, 10, 11).unwrap()
}

// ---------------------------------------------------------------- fixtures

struct Fixture {
    json: Value,
    bin: Vec<u8>,
}

impl Fixture {
    fn load(kind: &str) -> Self {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/records");
        let read = |name: String| {
            let path = directory.join(name);
            fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        };
        let json: Value = serde_json::from_slice(&read(format!("{kind}.json"))).unwrap();
        let bin = read(format!("{kind}.bin"));
        assert_eq!(json["size"].as_u64().unwrap(), bin.len() as u64, "{kind}");
        Self { json, bin }
    }

    fn records(&self) -> &[Value] {
        self.json["records"].as_array().unwrap()
    }

    /// The application default currency that applies to accounts without
    /// one.
    fn default_currency(&self) -> &str {
        self.json["default_currency"].as_str().unwrap()
    }
}

/// The account a fixture gives the values of a record for.
#[derive(Debug, Clone)]
struct Account {
    kind: u8,
    /// The account's currency, or the pinned default if it has none.
    currency: String,
}

impl Account {
    fn of(context: &Value, default_currency: &str) -> Self {
        Self {
            kind: u8::try_from(context["account_kind"].as_u64().unwrap()).unwrap(),
            currency: context["account_currency"]
                .as_str()
                .unwrap_or(default_currency)
                .to_owned(),
        }
    }
}

// ------------------------------------------------------------ record types

/// One primitive of the wire format, for the tables below.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Prim {
    U8,
    Bool,
    U16,
    I32,
    Decimal,
    Code(usize),
    Str,
    DateRaw,
    DateClamped,
    Time,
    Ticks,
}

use Prim::{Bool, Code, DateClamped, DateRaw, Str, Ticks, Time, I32, U16, U8};

/// 04 §4 and §5.1, transcribed a second time: the decoder is checked against
/// these tables in the truncation test.
const HEAD: &[(&str, Prim)] = &[
    ("oid", I32),
    ("update_stamp", I32),
    ("class_and_direction", U8),
    ("amount", Prim::Decimal),
    ("currency", Code(3)),
    ("counterparty_name", Str),
    ("counterparty_account", Str),
    ("counterparty_bank_code", Str),
    ("counterparty_country", Code(2)),
    ("purpose_text", Str),
    ("scheduled_date", DateRaw),
    ("order_id", Str),
    ("category", Str),
];

/// 04 §5.3.
const EXTENSION: &[(&str, Prim)] = &[
    ("counterparty_street", Str),
    ("counterparty_town", Str),
    ("end_to_end_id", Str),
    ("payment_info_id", Str),
    ("purpose_code", Str),
    ("category_purpose_code", Str),
    ("mandate_id", Str),
    ("mandate_signed_date", DateRaw),
    ("creditor_id", Str),
    ("local_instrument_code", Str),
    ("sequence_type", Str),
    ("ultimate_counterparty_name", Str),
    ("cost_bearer", U8),
    ("cost_bearer_account_oid", I32),
    ("intl_remittance_method", Str),
    ("intl_remittance_instruction", Str),
    ("counterparty_bank_name", Str),
    ("counterparty_bank_street", Str),
    ("counterparty_bank_town", Str),
    ("scheme_type", I32),
    ("scheduled_time", Time),
    ("regulatory_reporting_code", Str),
];

/// 04 §6.1.
const BOOKING: &[(&str, Prim)] = &[
    ("booking_date", DateRaw),
    ("value_date", DateRaw),
    ("transaction_code", I32),
    ("booking_text", Str),
    ("primanota", Str),
    ("entry_bits", U8),
    ("flag", I32),
    ("split", U8),
    ("batch_oid", I32),
    ("bank_reference", Str),
    ("notes", Str),
    ("booking_status", U8),
    ("gvc", U16),
    ("gvc_extension", U16),
    ("analysis_date", DateClamped),
    ("transaction_date", DateClamped),
    ("swift_transaction_code", Str),
    ("vat_amount", Prim::Decimal),
    ("vat_rate_type", U8),
    ("instructed_amount", Prim::Decimal),
    ("instructed_currency", Code(3)),
    ("charges_amount", Prim::Decimal),
    ("charges_currency", Code(3)),
    ("debit_return_order_id", Str),
    ("debit_return_date", DateRaw),
    ("debit_return_time", Time),
    ("virtual_iban", Str),
];

/// 04 §8.1.
const PAYMENT: &[(&str, Prim)] = &[
    ("state", U16),
    ("flags", U16),
    ("outbox_oid", I32),
    ("reserved_a", I32),
    ("batch_oid", I32),
    ("color_flag", I32),
    ("initiator_name", Str),
    ("batch_booking", U8),
    ("used_tan", Str),
    ("import_id", Str),
    ("reserved_b", Ticks),
    ("reconciled_booking_oid", I32),
    ("order_reference_tan", Str),
    ("payment_reference", Str),
    ("payee_verification_json", Str),
    ("sepa_status_code", Str),
    ("sepa_reason_code", Str),
    ("sepa_additional_info", Str),
    ("reserved_c", Bool),
    ("payee_verification_choice", U8),
];

/// 04 §9.1.
const STANDING_ORDER: &[(&str, Prim)] = &[
    ("state", I32),
    ("outbox_oid", I32),
    ("first_execution_date", DateRaw),
    ("period_unit", I32),
    ("period_length", I32),
    ("execution_day", I32),
    ("last_execution_date", DateRaw),
    ("next_execution_date", DateRaw),
    ("reserved_a", I32),
    ("reserved_b", Bool),
    ("reserved_c", I32),
    ("reserved_d", Ticks),
    ("execution_time", Time),
];

/// What the three record types have in common for these tests.
trait Family: Record + Serialize + Clone + Debug {
    /// Name of the fixture files.
    const KIND: &'static str;
    /// The fields after the payment base, in wire order.
    const OWN: &'static [(&'static str, Prim)];
    /// Keys of the serialised record that no fixture field stands for. They
    /// are checked against the stored values in [`assert_b4_only_keys`].
    const B4_ONLY: &'static [&'static str];

    fn base(&self) -> &PaymentBase;

    /// The record as it is for an account of this kind (04 §6.2 step 4).
    fn for_account(&self, account: &Account) -> Self;

    /// The record as JSON, with the values Banking4 shows where the record
    /// keeps the stored ones next to a mark (04 §6.4 step 3).
    fn view(&self) -> Map<String, Value>;

    /// The `currency` field as Banking4 holds it in memory.
    fn memory_currency(&self, account: &Account) -> Option<String>;
    fn displayed_currency(&self, account: &Account) -> Option<String>;
    fn displayed_category(&self) -> Option<String>;
    /// The record kind of 04 §11.21.
    fn record_kind(&self) -> i64;
    fn net_amount(&self) -> Option<Decimal> {
        None
    }
}

fn object<T: Serialize>(record: &T) -> Map<String, Value> {
    match serde_json::to_value(record).unwrap() {
        Value::Object(object) => object,
        other => panic!("a record serialises as an object, not as {other}"),
    }
}

impl Family for Booking {
    const KIND: &'static str = "booking";
    const OWN: &'static [(&'static str, Prim)] = BOOKING;
    const B4_ONLY: &'static [&'static str] = &[
        "legacy",
        "stored_scheme_class",
        "stored_scheme_type",
        "stored_flag",
        "ungrouped_by_banking4",
    ];

    fn base(&self) -> &PaymentBase {
        &self.base
    }

    fn for_account(&self, account: &Account) -> Self {
        let mut booking = self.clone();
        booking.apply_account_kind(account.kind);
        booking
    }

    fn view(&self) -> Map<String, Value> {
        let mut view = object(self);
        view.insert("batch_oid".into(), json!(self.effective_batch_oid()));
        view.insert("split".into(), json!(self.effective_split()));
        view
    }

    fn memory_currency(&self, account: &Account) -> Option<String> {
        self.adjusted_currency(account.kind, Some(&account.currency))
            .map(str::to_owned)
    }

    fn displayed_currency(&self, account: &Account) -> Option<String> {
        // 04 §6.2: always the account's.
        Booking::displayed_currency(self, Some(&account.currency)).map(str::to_owned)
    }

    fn displayed_category(&self) -> Option<String> {
        Booking::displayed_category(self).map(str::to_owned)
    }

    fn record_kind(&self) -> i64 {
        0x100
    }

    fn net_amount(&self) -> Option<Decimal> {
        Booking::net_amount(self)
    }
}

impl Family for Payment {
    const KIND: &'static str = "payment";
    const OWN: &'static [(&'static str, Prim)] = PAYMENT;
    const B4_ONLY: &'static [&'static str] = &[
        "legacy",
        "stored_scheme_class",
        "stored_scheme_type",
        "sepa_status_code",
        "hidden_by_banking4",
        "ungrouped_by_banking4",
    ];

    fn base(&self) -> &PaymentBase {
        &self.base
    }

    fn for_account(&self, _: &Account) -> Self {
        self.clone()
    }

    fn view(&self) -> Map<String, Value> {
        let mut view = object(self);
        view.insert("batch_oid".into(), json!(self.effective_batch_oid()));
        view
    }

    fn memory_currency(&self, _: &Account) -> Option<String> {
        self.base.currency.clone()
    }

    fn displayed_currency(&self, account: &Account) -> Option<String> {
        Payment::displayed_currency(self, Some(&account.currency)).map(str::to_owned)
    }

    fn displayed_category(&self) -> Option<String> {
        Payment::displayed_category(self).map(str::to_owned)
    }

    fn record_kind(&self) -> i64 {
        if self.is_debit() {
            0x01
        } else {
            0x02
        }
    }
}

impl Family for StandingOrder {
    const KIND: &'static str = "standing_order";
    const OWN: &'static [(&'static str, Prim)] = STANDING_ORDER;
    const B4_ONLY: &'static [&'static str] = &[
        "legacy",
        "stored_scheme_class",
        "stored_scheme_type",
        "stored_next_execution_date",
    ];

    fn base(&self) -> &PaymentBase {
        &self.base
    }

    fn for_account(&self, _: &Account) -> Self {
        self.clone()
    }

    fn view(&self) -> Map<String, Value> {
        object(self)
    }

    fn memory_currency(&self, _: &Account) -> Option<String> {
        self.base.currency.clone()
    }

    fn displayed_currency(&self, account: &Account) -> Option<String> {
        StandingOrder::displayed_currency(self, Some(&account.currency)).map(str::to_owned)
    }

    fn displayed_category(&self) -> Option<String> {
        // A standing order is never a master (04 §9.1).
        self.base.category.clone()
    }

    fn record_kind(&self) -> i64 {
        if self.is_debit() {
            0x10
        } else {
            0x20
        }
    }
}

// ------------------------------------------- fixture notation to b4's JSON

/// How a value of the fixture's `fields` relates to the serialised record.
enum Rule {
    /// The same JSON value under the same key.
    Same,
    /// The fixture has the number of an enumeration or bit set; `b4`
    /// serialises the name or the list of names. Compared under `key`.
    Coded {
        key: &'static str,
        name: fn(i64) -> Value,
    },
    /// A decimal as text. Equal up to the sign of zero, which `b4`'s decimal
    /// type does not keep (Banking4 prints `-0.00`).
    Decimal,
    /// The fixture writes a validated date as a timestamp at midnight.
    Midnight,
    /// The account decides (04 §6.2 step 3): compared with
    /// [`Family::memory_currency`].
    MemoryCurrency,
    /// Banking4 keeps the parsed text and prints it again in its own
    /// spelling (04 §8.5); `b4` keeps the stored text. Compared by content.
    PayeeVerification,
    /// A value that exists in Banking4's memory only (04 §5.10, §6.1). The
    /// fixture must show its initial value; `b4` has no such field.
    MemoryOnly(Value),
    /// Bank access material: the fixture has it, `b4` must not.
    Dropped,
    /// Banking4 loses the stored value while reading (04 §8.2), so the
    /// fixture shows nothing; `b4` keeps it and it is checked separately.
    LostByBanking4,
    /// Standing orders have no batch link; Banking4's in-memory value stays
    /// at -1 (04 §9.1) and `b4` has no field.
    NoBatchLink,
}

fn number<T: TryFrom<i64>>(value: i64) -> T
where
    T::Error: Debug,
{
    T::try_from(value).unwrap()
}

fn rule<T: Family>(key: &str) -> Rule {
    let coded = |key, name| Rule::Coded { key, name };
    match (T::KIND, key) {
        (_, "amount" | "vat_amount" | "instructed_amount" | "charges_amount") => Rule::Decimal,
        (_, "analysis_date" | "transaction_date") => Rule::Midnight,
        (_, "currency") => Rule::MemoryCurrency,
        (_, "scheme_type") => coded("scheme_type", |n| json!(SchemeType::from_raw(number(n)))),
        (_, "cost_bearer") => coded("cost_bearer", |n| json!(CostBearer::from_raw(number(n)))),
        (_, "transaction_code") => coded("transaction_code", |n| {
            json!(TransactionCode::from_raw(number(n)))
        }),
        (_, "entry_bits") => coded("entry_bits", |n| json!(EntryBits::from_raw(number(n)))),
        (_, "flag") => coded("flag", |n| json!(ColorFlag::from_raw(number(n)))),
        (_, "color_flag") => coded("color_flag", |n| json!(ColorFlag::from_raw(number(n)))),
        (_, "split") => coded("split", |n| json!(SplitRole::from_raw(number(n)))),
        (_, "booking_status") => coded("booking_status", |n| {
            json!(BookingStatus::from_raw(number(n)))
        }),
        (_, "vat_rate_type") => coded("vat_rate_type", |n| json!(VatRateType::from_raw(number(n)))),
        ("payment", "state") => coded("state", |n| json!(PaymentState::from_raw(number(n)))),
        ("payment", "flags") => coded("flags", |n| json!(PaymentFlags::from_raw(number(n)))),
        ("standing_order", "state") => {
            coded("state", |n| json!(StandingOrderState::from_raw(number(n))))
        }
        (_, "batch_booking") => coded("batch_booking", |n| {
            json!(BatchBooking::from_raw(number(n)))
        }),
        // Banking4 holds the number of the stored code (04 §11.13).
        (_, "sepa_status_code") => coded("sepa_status", |n| json!(SepaStatus::from_raw(number(n)))),
        (_, "payee_verification_choice") => coded("payee_verification_choice", |n| {
            json!(PayeeVerificationChoice::from_raw(number(n)))
        }),
        (_, "period_unit") => coded("period_unit", |n| json!(PeriodUnit::from_raw(number(n)))),
        (_, "payee_verification_json") => Rule::PayeeVerification,
        (_, "used_tan" | "order_reference_tan") => Rule::Dropped,
        (_, "sepa_additional_info") => Rule::LostByBanking4,
        (_, "counterparty_name_digest" | "running_balance" | "day_balance") => {
            Rule::MemoryOnly(Value::Null)
        }
        (_, "batch_expanded") => Rule::MemoryOnly(json!(false)),
        (_, "cross_entry_oid") => Rule::MemoryOnly(json!(0)),
        ("standing_order", "batch_oid") => Rule::NoBatchLink,
        ("standing_order", "batch_index") => Rule::MemoryOnly(json!(0)),
        // For bookings and payments `batch_index` is memory-only as well,
        // but `b4` computes it; before the regrouping it is 0 in the fixture
        // and in `b4`.
        _ => Rule::Same,
    }
}

/// A decimal text without the sign of a zero.
fn unsigned_zero(text: &str) -> &str {
    match text.strip_prefix('-') {
        Some(digits) if digits.bytes().all(|byte| matches!(byte, b'0' | b'.')) => digits,
        _ => text,
    }
}

fn decimal_text(value: Decimal) -> Value {
    json!(unsigned_zero(&value.to_string()))
}

/// What Banking4 keeps of a payee verification text (04 §8.5), or `Err` if
/// it rejects the record over it. Only as much of Banking4's lenient JSON
/// handling as the fixtures show.
fn payee_verification(text: &str) -> Result<BTreeMap<&'static str, String>, &'static str> {
    const STATUS: [&str; 5] = ["RCVC", "RVMC", "RVNM", "RVNA", "PDNG"];
    const REASON: [&str; 2] = ["AG03", "AB11"];
    const TEXTS: [&str; 5] = [
        "vopNotApplicableReasonAdditionalInfo",
        "vopCreditorName",
        "originalCreditorName",
        "originalEndToEndId",
        "originalCreditorAccountIBAN",
    ];
    const INDEXES: [&str; 3] = [
        "originalPaymentInformationIndex",
        "originalTransactionInformationIndex",
        "originalTransactionIndex",
    ];
    let parsed: Value = serde_json::from_str(text).map_err(|_| "not JSON")?;
    let members = parsed.as_object().ok_or("not a JSON object")?;
    let present = |key: &str| {
        members
            .get(key)
            .filter(|value| !value.is_null() && value.as_str() != Some(""))
    };
    let mut kept = BTreeMap::new();
    for (key, codes) in [
        ("vopTransactionStatus", &STATUS[..]),
        ("vopNotApplicableReasonCode", &REASON[..]),
    ] {
        if let Some(value) = present(key) {
            let code = value.as_str().filter(|code| codes.contains(code));
            kept.insert(key, code.ok_or("unknown code")?.to_owned());
        }
    }
    for key in TEXTS {
        if let Some(value) = present(key) {
            let text = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            kept.insert(key, text);
        }
    }
    if let Some(value) = members.get("originalAmount").filter(|v| !v.is_null()) {
        let text = value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned);
        let amount = Decimal::from_str(&text).map_err(|_| "amount is not a number")?;
        if !amount.is_zero() {
            kept.insert("originalAmount", amount.normalize().to_string());
        }
    }
    for key in INDEXES {
        if let Some(value) = members.get(key) {
            let index = match value {
                Value::Number(number) => number
                    .as_i64()
                    .or_else(|| number.as_f64().map(|float| float.trunc() as i64))
                    .unwrap_or(0),
                Value::String(digits) => digits.parse().unwrap_or(0),
                _ => 0,
            };
            kept.insert(key, index.to_string());
        }
    }
    Ok(kept)
}

/// Compare the fixture's `fields` of one record with the record as `b4`
/// serialises it. Returns the keys of the serialised record that a fixture
/// value was compared with.
fn compare_fields<T: Family>(
    record: &T,
    account: &Account,
    fields: &Map<String, Value>,
    problems: &mut Vec<String>,
) -> BTreeSet<String> {
    let actual = record.view();
    let mut compared = BTreeSet::new();
    let mut check = |key: &str, ours: &Value, theirs: &Value, by_content: bool| {
        let equal = if by_content {
            let content = |value: &Value| value.as_str().map(payee_verification);
            content(ours) == content(theirs)
        } else {
            ours == theirs
        };
        if !equal {
            problems.push(format!("{key}: b4 {ours}, fixture {theirs}"));
        }
    };
    for (key, theirs) in fields {
        let mut ours_under = |key: &str| {
            compared.insert(key.to_owned());
            actual
                .get(key)
                .cloned()
                .unwrap_or_else(|| panic!("{}: the serialised record has no key `{key}`", T::KIND))
        };
        match rule::<T>(key) {
            Rule::Same => check(key, &ours_under(key), theirs, false),
            Rule::Coded {
                key: ours_key,
                name,
            } => {
                check(
                    key,
                    &ours_under(ours_key),
                    &name(theirs.as_i64().unwrap()),
                    false,
                );
            }
            Rule::Decimal => {
                let ours = ours_under(key);
                check(
                    key,
                    &json!(unsigned_zero(ours.as_str().unwrap())),
                    &json!(unsigned_zero(theirs.as_str().unwrap())),
                    false,
                );
            }
            Rule::Midnight => {
                let date = theirs.as_str().map(|text| {
                    text.strip_suffix("T00:00:00.0000000")
                        .unwrap_or_else(|| panic!("{key}: `{text}` is not a date at midnight"))
                });
                check(key, &ours_under(key), &json!(date), false);
            }
            Rule::MemoryCurrency => {
                // The stored code under this key is compared for PayPal
                // accounts, where it survives, and for payments and standing
                // orders.
                compared.insert(key.clone());
                check(key, &json!(record.memory_currency(account)), theirs, false);
            }
            Rule::PayeeVerification => check(key, &ours_under(key), theirs, true),
            Rule::MemoryOnly(initial) => {
                assert!(!actual.contains_key(key), "{key} is not stored");
                check(key, &initial, theirs, false);
            }
            Rule::Dropped => {
                assert!(!actual.contains_key(key), "{key} must be dropped");
            }
            Rule::LostByBanking4 => {
                compared.insert(key.clone());
                assert!(actual.contains_key(key));
                check(key, &Value::Null, theirs, false);
            }
            Rule::NoBatchLink => {
                assert!(!actual.contains_key(key));
                check(key, &json!(-1), theirs, false);
            }
        }
    }
    compared
}

/// Compare the values Banking4 computes from a record (`derived`) with
/// `b4`'s methods. Every key of the fixture must be known here.
fn compare_derived<T: Family>(
    record: &T,
    account: &Account,
    derived: &Map<String, Value>,
    problems: &mut Vec<String>,
) {
    let base = record.base();
    for (key, theirs) in derived {
        let ours = match key.as_str() {
            "effective_debit" => json!(base.is_debit()),
            "signed_value" => decimal_text(base.signed_amount()),
            "record_kind" => json!(record.record_kind()),
            // The class Banking4 would write, not the stored nibble.
            "scheme_class" => json!(base.scheme_type.class().raw()),
            "displayed_currency" => json!(record.displayed_currency(account)),
            "displayed_category" => json!(record.displayed_category()),
            "reported_scheduled_time" => json!(base.reported_scheduled_time()),
            "net_amount" => record.net_amount().map_or(Value::Null, decimal_text),
            other => panic!("{}: unknown derived value `{other}`", T::KIND),
        };
        let theirs = match theirs.as_str() {
            Some(text) if matches!(key.as_str(), "signed_value" | "net_amount") => {
                json!(unsigned_zero(text))
            }
            _ => theirs.clone(),
        };
        if ours != theirs {
            problems.push(format!("derived {key}: b4 {ours}, fixture {theirs}"));
        }
    }
}

/// A value `b4` deliberately decodes differently from Banking4.
struct Deviation {
    kind: &'static str,
    record: &'static str,
    /// Key in `fields`, or in `derived` when `derived` is set.
    key: &'static str,
    derived: bool,
    banking4: &'static str,
    /// What `b4` gives instead: no value.
    why: &'static str,
}

/// The one rule where `b4` knowingly departs from Banking4's values:
/// Banking4 treats the pseudo category names of the language it runs in as "no
/// category" (04 §5.7 step 4). A vault does not say which language that was,
/// so `b4` follows the document's advice and treats the names of all three
/// shipped languages that way. The fixture's values hold for German and so
/// keep an English pseudo name as a real category.
const DEVIATIONS: &[Deviation] = &[
    Deviation {
        kind: "booking",
        record: "category_pseudo_other_language",
        key: "category",
        derived: false,
        banking4: "(automatic)",
        why: "pseudo category name of another language",
    },
    Deviation {
        kind: "booking",
        record: "category_pseudo_other_language",
        key: "displayed_category",
        derived: true,
        banking4: "(automatic)",
        why: "pseudo category name of another language",
    },
];

/// Replace the fixture's value by `b4`'s in the expectations of one record,
/// for the listed deviations. The fixture's value is asserted first, so a
/// deviation cannot outlive a change of the fixture.
fn apply_deviations(
    kind: &str,
    record: &str,
    fields: &mut Map<String, Value>,
    derived: &mut Map<String, Value>,
) -> usize {
    let mut applied = 0;
    for deviation in DEVIATIONS {
        if deviation.kind != kind || deviation.record != record {
            continue;
        }
        let expectations = if deviation.derived {
            &mut *derived
        } else {
            &mut *fields
        };
        assert_eq!(
            expectations[deviation.key],
            json!(deviation.banking4),
            "{record}: {}",
            deviation.why
        );
        expectations.insert(deviation.key.to_owned(), Value::Null);
        applied += 1;
    }
    applied
}

/// A fixture object with the changes of another reading applied.
fn overlay(first: &Value, changes: &Value) -> Map<String, Value> {
    let mut merged = first.as_object().unwrap().clone();
    for (key, value) in changes.as_object().unwrap() {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

/// The four-letter code Banking4 writes for a status number (04 §8.4).
fn sepa_status_text(number: i64) -> Option<&'static str> {
    [
        "RCVD", "PDNG", "PART", "ACTC", "ACCP", "ACWC", "ACSP", "ACCC", "ACSC", "RJCT",
    ]
    .get(usize::try_from(number.checked_sub(1)?).ok()?)
    .copied()
}

/// Check the keys of the serialised record that Banking4's in-memory
/// fields do not show ([`Family::B4_ONLY`], and `sepa_additional_info`)
/// against what the fixture says was stored: the `wire` list of a record
/// that is given by its wire values, or the `set` values of a record Banking4
/// writes as a whole, by the writer rules of 04 §5.9, §8.4 and §9.3.
fn assert_b4_only_keys<T: Family>(record: &T, sample: &Value, context: &str) {
    let actual = object(record);
    let chunks = sample["chunk_sizes"].as_array().unwrap().len();
    assert_eq!(actual["legacy"], json!(chunks == 1), "{context}: legacy");
    for mark in ["hidden_by_banking4", "ungrouped_by_banking4"] {
        if let Some(value) = actual.get(mark) {
            assert_eq!(value, &json!(false), "{context}: {mark} before any rule");
        }
    }

    // What was stored, by wire field name.
    let mut stored: BTreeMap<&str, Value> = BTreeMap::new();
    if let Some(wire) = sample["wire"].as_array() {
        for item in wire {
            if let Some(field) = item["field"].as_str() {
                stored.insert(field, item["value"].clone());
            }
        }
    } else {
        let set = &sample["set"];
        let scheme = SchemeType::from_raw(number(set["scheme_type"].as_i64().unwrap()));
        let direction = scheme
            .debit()
            .unwrap_or_else(|| set["debit_flag"].as_bool().unwrap());
        stored.insert(
            "class_and_direction",
            json!(i64::from(scheme.class().raw()) << 4 | i64::from(direction)),
        );
        stored.insert("scheme_type", set["scheme_type"].clone());
        for key in ["flag", "next_execution_date"] {
            if let Some(value) = set.get(key) {
                stored.insert(key, value.clone());
            }
        }
        if let Some(status) = set.get("sepa_status_code") {
            stored.insert(
                "sepa_status_code",
                json!(sepa_status_text(status.as_i64().unwrap())),
            );
            let lines: Vec<&str> = set["sepa_additional_info"]
                .as_array()
                .map(|lines| lines.iter().map(|line| line.as_str().unwrap()).collect())
                .unwrap_or_default();
            stored.insert(
                "sepa_additional_info",
                if lines.is_empty() {
                    Value::Null
                } else {
                    json!(lines.join("\n"))
                },
            );
        }
    }

    let class = stored
        .get("class_and_direction")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    assert_eq!(
        actual["stored_scheme_class"],
        json!(SchemeClass::from_raw(number(class >> 4))),
        "{context}: stored_scheme_class"
    );
    assert_eq!(
        actual["debit_flag"],
        json!(class & 1 != 0),
        "{context}: debit_flag"
    );
    let scheme = stored.get("scheme_type").and_then(Value::as_i64);
    assert_eq!(
        actual["stored_scheme_type"],
        json!(SchemeType::from_raw(number(scheme.unwrap_or(0)))),
        "{context}: stored_scheme_type"
    );
    if let Some(ours) = actual.get("stored_flag") {
        let flag = stored.get("flag").and_then(Value::as_i64).unwrap_or(0);
        assert_eq!(
            ours,
            &json!(ColorFlag::from_raw(number(flag))),
            "{context}: stored_flag"
        );
    }
    for key in ["sepa_status_code", "sepa_additional_info"] {
        if let Some(ours) = actual.get(key) {
            // An empty text is stored like none (02 §4.8).
            let theirs = stored
                .get(key)
                .filter(|value| value.as_str() != Some(""))
                .cloned()
                .unwrap_or(Value::Null);
            assert_eq!(ours, &theirs, "{context}: {key}");
        }
    }
    if let Some(ours) = actual.get("stored_next_execution_date") {
        let theirs = stored
            .get("next_execution_date")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(ours, &theirs, "{context}: stored_next_execution_date");
    }
}

/// Decode one fixture file and compare every record with everything the
/// fixture says about reading it. Returns the number of values compared.
fn assert_fixture<T: Family>(expected_records: usize, rejected_by_banking4: usize) -> usize {
    let fixture = Fixture::load(T::KIND);
    let collection = load_collection_strict::<T>(&fixture.bin)
        .unwrap_or_else(|error| panic!("{}: {error}", T::KIND));
    assert_eq!(collection.records.len(), expected_records, "{}", T::KIND);
    assert_eq!(fixture.records().len(), expected_records, "{}", T::KIND);
    assert!(collection.report.duplicate_oids.is_zero());

    let framed: Vec<_> = records(&fixture.bin).map(Result::unwrap).collect();
    let (mut compared, mut rejected, mut legacy, mut deviations) = (0, 0, 0, 0);
    let mut problems = Vec::new();
    for ((record, sample), raw) in collection.records.iter().zip(fixture.records()).zip(framed) {
        let name = sample["name"].as_str().unwrap();
        assert_eq!(
            raw.offset as u64,
            sample["offset"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(
            raw.body.len() as u64,
            sample["record_size"].as_u64().unwrap(),
            "{name}"
        );
        assert_b4_only_keys(record, sample, name);
        legacy += usize::from(record.is_legacy());

        let first = &sample["read"];
        if first.get("error").is_some() {
            // Banking4 rejects the record over its payee verification text
            // (04 §8.5). `b4` must not: it decoded above. The text must be
            // one the documented rules reject, or the rules are incomplete.
            let text = object(record)["payee_verification_json"].clone();
            assert!(
                payee_verification(text.as_str().unwrap()).is_err(),
                "{name}: Banking4 rejects this text, 04 §8.5 as transcribed here does not"
            );
            rejected += 1;
            continue;
        }
        // Strict loading already showed that no chunk is left over.
        assert_eq!(first["more_chunks_left"], json!(false), "{name}");

        let mut contexts = vec![(
            Account::of(first, fixture.default_currency()),
            first["fields"].as_object().unwrap().clone(),
            first["derived"].as_object().unwrap().clone(),
        )];
        for other in sample["read_other_accounts"].as_array().unwrap() {
            contexts.push((
                Account::of(other, fixture.default_currency()),
                overlay(&first["fields"], &other["changed_fields"]),
                overlay(&first["derived"], &other["changed_derived"]),
            ));
        }
        for (account, mut fields, mut derived) in contexts {
            deviations += apply_deviations(T::KIND, name, &mut fields, &mut derived);
            let in_account = record.for_account(&account);
            let mut found = Vec::new();
            let seen = compare_fields(&in_account, &account, &fields, &mut found);
            compare_derived(&in_account, &account, &derived, &mut found);
            compared += fields.len() + derived.len();
            problems.extend(
                found
                    .into_iter()
                    .map(|problem| format!("{name} (account kind {}): {problem}", account.kind)),
            );

            // Every key of the serialised record is either compared with a
            // fixture value or listed as b4's own.
            let unaccounted: Vec<String> = in_account
                .view()
                .keys()
                .filter(|key| !seen.contains(*key) && !T::B4_ONLY.contains(&key.as_str()))
                .cloned()
                .collect();
            assert!(
                unaccounted.is_empty(),
                "{name}: not compared: {unaccounted:?}"
            );
        }
    }
    assert!(
        problems.is_empty(),
        "{}: {} differences\n{}",
        T::KIND,
        problems.len(),
        problems.join("\n")
    );
    assert_eq!(rejected, rejected_by_banking4, "{}", T::KIND);
    assert_eq!(collection.report.legacy.records, legacy, "{}", T::KIND);
    // Every listed deviation was met, once per account the fixture gives the
    // record for.
    let listed = DEVIATIONS.iter().filter(|d| d.kind == T::KIND).count();
    let accounts = 1 + fixture.records()[0]["read_other_accounts"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(deviations, listed * accounts, "{}", T::KIND);
    compared
}

#[test]
fn every_booking_of_the_fixture_decodes_to_banking4s_values() {
    // 81 records, each given for five accounts.
    let compared = assert_fixture::<Booking>(81, 0);
    assert_eq!(compared, 30_780, "81 records, 5 accounts, 76 values");
}

#[test]
fn every_payment_of_the_fixture_decodes_to_banking4s_values() {
    // 131 records; Banking4 rejects 8 of them, b4 none.
    let compared = assert_fixture::<Payment>(131, 8);
    assert_eq!(compared, 22_878, "123 records, 3 accounts, 62 values");
}

#[test]
fn every_standing_order_of_the_fixture_decodes_to_banking4s_values() {
    let compared = assert_fixture::<StandingOrder>(43, 0);
    assert_eq!(compared, 7_095, "43 records, 3 accounts, 55 values");
}

#[test]
fn the_fixtures_hold_the_legacy_single_chunk_form() {
    // 04 §5.8 is only exercised if such records are there.
    for (kind, legacy) in [("booking", 7), ("payment", 5), ("standing_order", 2)] {
        let fixture = Fixture::load(kind);
        let single = fixture
            .records()
            .iter()
            .filter(|sample| sample["chunk_sizes"].as_array().unwrap().len() == 1)
            .count();
        assert_eq!(single, legacy, "{kind}");
    }
}

// ------------------------------------------------------- collection rules

/// The decoded records of a fixture file that Banking4's list holds, with
/// their names: all but those Banking4 rejects on reading.
fn listed<T: Family>(fixture: &Fixture) -> (Vec<T>, Vec<String>) {
    let collection = load_collection::<T>(&fixture.bin);
    let mut kept = Vec::new();
    let mut names = Vec::new();
    for (record, sample) in collection.records.into_iter().zip(fixture.records()) {
        if sample["read"].get("error").is_none() {
            kept.push(record);
            names.push(sample["name"].as_str().unwrap().to_owned());
        }
    }
    (kept, names)
}

/// Compare the fixture's expected result of loading the file with the
/// records after `b4`'s rules: order, and every field of every listed record.
/// `unreplicated` names changes of Banking4 that `b4` deliberately does
/// not make, per record name and field.
fn assert_collection<T: Family>(
    fixture: &Fixture,
    records: &[T],
    names: &[String],
    order: &[usize],
    unreplicated: &dyn Fn(&T, &str) -> bool,
) -> usize {
    let load = &fixture.json["collection_load"];
    let account = Account::of(load, fixture.default_currency());
    let items = load["items"].as_array().unwrap();

    let ours: Vec<&str> = order.iter().map(|&index| names[index].as_str()).collect();
    let theirs: Vec<&str> = items
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert_eq!(ours, theirs, "{}: order of the list", T::KIND);

    let by_name: BTreeMap<&str, &Value> = fixture
        .records()
        .iter()
        .map(|sample| (sample["name"].as_str().unwrap(), sample))
        .collect();
    let mut problems = Vec::new();
    let mut compared = 0;
    for (item, &index) in items.iter().zip(order) {
        let name = item["name"].as_str().unwrap();
        let record = records[index].for_account(&account);
        let read = &by_name[name]["read"];
        let mut fields = overlay(&read["fields"], &item["changed_fields"]);
        fields.retain(|key, _| {
            !(item["changed_fields"].get(key).is_some() && unreplicated(&record, key))
        });
        let mut derived = overlay(&read["derived"], &item["changed_derived"]);
        apply_deviations(T::KIND, name, &mut fields, &mut derived);
        let mut found = Vec::new();
        compare_fields(&record, &account, &fields, &mut found);
        compare_derived(&record, &account, &derived, &mut found);
        compared += fields.len() + derived.len();
        problems.extend(
            found
                .into_iter()
                .map(|problem| format!("{name}: {problem}")),
        );
    }
    assert!(
        problems.is_empty(),
        "{}: {} differences after the collection rules\n{}",
        T::KIND,
        problems.len(),
        problems.join("\n")
    );
    compared
}

#[test]
fn bookings_are_sorted_and_regrouped_like_banking4s_list() {
    let fixture = Fixture::load("booking");
    let (mut bookings, names) = listed::<Booking>(&fixture);
    let before = bookings.clone();
    let counts = regroup_bookings(&mut bookings, today());
    let order = banking4_booking_order(&bookings, today());
    assert_eq!(order.len(), 81);

    let compared = assert_collection(&fixture, &bookings, &names, &order, &|_, _| false);
    assert_eq!(compared, 81 * 76);

    // The fixture has two groups that hold (a split and a batch, five slaves
    // in all) and nine records that are taken out of theirs.
    assert_eq!((counts.groups, counts.slaves, counts.ungrouped), (2, 5, 9));
    let ungrouped: BTreeSet<&str> = bookings
        .iter()
        .zip(&names)
        .filter(|(booking, _)| booking.ungrouped_by_banking4)
        .map(|(_, name)| name.as_str())
        .collect();
    let reset_by_banking4: BTreeSet<&str> = fixture.json["collection_load"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["changed_fields"].get("batch_oid") == Some(&json!(0)))
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert_eq!(ungrouped, reset_by_banking4);

    // Marks only: what the file holds is untouched, and file order is kept.
    for (after, before) in bookings.iter().zip(&before) {
        let mut unmarked = after.clone();
        unmarked.batch_index = 0;
        unmarked.ungrouped_by_banking4 = false;
        assert_eq!(&unmarked, before);
    }
    // Doing it again changes nothing.
    let again = bookings.clone();
    assert_eq!(regroup_bookings(&mut bookings, today()), counts);
    assert_eq!(bookings, again);
}

#[test]
fn payments_get_the_rules_of_banking4s_list() {
    let fixture = Fixture::load("payment");
    let (mut payments, names) = listed::<Payment>(&fixture);
    assert_eq!(payments.len(), 123);
    let before = payments.clone();
    let rules = apply_payment_rules(&mut payments, today());
    let order = banking4_payment_order(&payments, today());

    // Banking4 drops three payments; b4 keeps and marks them.
    assert_eq!(payments.len(), 123);
    let hidden: BTreeSet<&str> = payments
        .iter()
        .zip(&names)
        .filter(|(payment, _)| payment.hidden_by_banking4)
        .map(|(_, name)| name.as_str())
        .collect();
    let dropped: BTreeSet<&str> = fixture.json["collection_load"]["dropped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert_eq!(hidden, dropped);
    assert_eq!(rules.hidden, 3);
    assert_eq!(order.len(), 120);
    for (payment, original) in payments.iter().zip(&before) {
        if payment.hidden_by_banking4 {
            let mut unmarked = payment.clone();
            unmarked.hidden_by_banking4 = false;
            assert_eq!(
                &unmarked, original,
                "a hidden payment is otherwise untouched"
            );
        }
    }

    // Banking4 marks a converted legacy batch master as modified by
    // setting its update stamp to -1 in memory. That is not data: b4 leaves
    // the stored stamp.
    let legacy_masters: BTreeSet<i32> = before
        .iter()
        .filter(|payment| payment.flags.contains(PaymentFlags::LEGACY_BATCH_MASTER))
        .map(|payment| payment.base.record.oid)
        .collect();
    let stamp_of_legacy_master = |payment: &Payment, key: &str| {
        key == "update_stamp" && legacy_masters.contains(&payment.base.record.oid)
    };
    let compared = assert_collection(&fixture, &payments, &names, &order, &stamp_of_legacy_master);
    // 62 values each, less the five update stamps Banking4 resets.
    assert_eq!(compared, 120 * 62 - 5);
    let stamps_reset_by_banking4 = fixture.json["collection_load"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["changed_fields"].get("update_stamp").is_some())
        .inspect(|item| assert_eq!(item["changed_fields"]["update_stamp"], json!(-1)))
        .count();
    assert_eq!(stamps_reset_by_banking4, legacy_masters.len());

    assert_eq!(rules.legacy_masters, 5);
    assert_eq!(rules.legacy_master_conflicts, 0);
    assert_eq!(rules.legacy_slaves, 3);
    assert!(rules.legacy_states > 0 && rules.legacy_flags > 0);

    // Doing it again changes nothing.
    let again = payments.clone();
    let second = apply_payment_rules(&mut payments, today());
    assert_eq!(payments, again);
    assert_eq!(second.hidden, rules.hidden);
    assert_eq!(second.regrouping, rules.regrouping);
    assert_eq!(
        (
            second.legacy_states,
            second.legacy_flags,
            second.legacy_masters
        ),
        (0, 0, 0)
    );
}

#[test]
fn standing_orders_are_not_changed_by_banking4s_list() {
    // Banking4 only sorts them, by a date it computes from the schedule
    // (04 §6.4, §9.2); b4 has no rule for standing orders at all.
    let fixture = Fixture::load("standing_order");
    let items = fixture.json["collection_load"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 43);
    for item in items {
        assert_eq!(item["changed_fields"], json!({}), "{}", item["name"]);
        assert_eq!(item["changed_derived"], json!({}), "{}", item["name"]);
    }
    assert_eq!(fixture.json["collection_load"]["dropped"], json!([]));
}

#[test]
fn a_second_legacy_master_of_the_same_batch_does_not_stop_anything() {
    // Banking4 stops loading the file at such a record (04 §8.3 step 3).
    let fixture = Fixture::load("payment");
    let (payments, names) = listed::<Payment>(&fixture);
    let position = |name: &str| names.iter().position(|n| n == name).unwrap();
    let master = payments[position("legacy_batch_master_plain")].clone();
    let slave = payments[position("legacy_batch_slave_3")].clone();
    let mut second = master.clone();
    second.base.record.oid += 1_000;

    let mut list = vec![master.clone(), second.clone(), slave];
    let rules = apply_payment_rules(&mut list, today());
    assert_eq!(rules.legacy_masters, 1);
    assert_eq!(rules.legacy_master_conflicts, 1);
    assert_eq!(rules.legacy_slaves, 1);
    // The first master keeps the batch; the second is a master on its own.
    assert_eq!(list[2].batch_oid, master.base.record.oid);
    assert!(!list[0].ungrouped_by_banking4 && !list[2].ungrouped_by_banking4);
    assert_eq!(list[1].batch_oid, second.base.record.oid);
    assert!(list[1].ungrouped_by_banking4);
}

// --------------------------------------------------------------- defaults

/// The record that an empty chunk decodes to.
fn empty_record<T: Family>() -> Map<String, Value> {
    let body = 4_u32.to_le_bytes();
    let mut reader = Reader::new(&body).unwrap();
    let record = T::decode(&mut reader).unwrap();
    assert!(record.is_legacy(), "a single chunk is the legacy form");
    object(&record)
}

/// The value a key has when its field is missing from a short chunk
/// (04 §2), where that does not depend on other fields.
fn default_of(key: &str) -> Value {
    match key {
        "oid"
        | "update_stamp"
        | "cost_bearer_account_oid"
        | "batch_oid"
        | "outbox_oid"
        | "reconciled_booking_oid"
        | "gvc"
        | "gvc_extension"
        | "period_length"
        | "execution_day"
        | "batch_index" => json!(0),
        "legacy" | "debit_flag" | "hidden_by_banking4" | "ungrouped_by_banking4" => json!(false),
        "amount" | "vat_amount" | "instructed_amount" | "charges_amount" => json!("0"),
        "purpose_lines" | "entry_bits" | "split" | "vat_rate_type" | "state" | "flags" => {
            json!([])
        }
        "stored_scheme_class" => json!("any"),
        "scheme_type" | "stored_scheme_type" | "cost_bearer" | "flag" | "stored_flag"
        | "color_flag" | "sepa_status" | "period_unit" => json!("none"),
        "transaction_code" => json!("other"),
        "booking_status" => json!("booked"),
        "batch_booking" | "payee_verification_choice" => json!("default"),
        _ => Value::Null,
    }
}

#[test]
fn an_empty_record_decodes_to_the_documented_defaults() {
    let differing = |record: Map<String, Value>| -> BTreeMap<String, Value> {
        record
            .into_iter()
            .filter(|(key, value)| *value != default_of(key))
            .collect()
    };
    // What differs from the plain defaults is what the reader derives:
    // a single chunk is the legacy form; a booking without a date is pending
    // (04 §6.2 step 1); a payment or standing order without a scheme gets
    // one from class 0 and a clear direction bit (04 §5.4).
    assert_eq!(
        differing(empty_record::<Booking>()),
        BTreeMap::from([
            ("legacy".to_owned(), json!(true)),
            ("booking_status".to_owned(), json!("pending")),
        ])
    );
    for record in [empty_record::<Payment>(), empty_record::<StandingOrder>()] {
        assert_eq!(
            differing(record),
            BTreeMap::from([
                ("legacy".to_owned(), json!(true)),
                ("scheme_type".to_owned(), json!("user_income")),
            ])
        );
    }
}

// ------------------------------------------------------------- truncation

/// Where one field of the tables lies in a record.
#[derive(Debug, Clone)]
struct Span {
    field: &'static str,
    /// First byte after the field.
    end: usize,
    /// Number of the chunk, counted from 1.
    chunk: u32,
}

/// Read one primitive, for its cursor movement only.
fn skip(r: &mut Reader<'_>, prim: Prim) -> Result<(), DecodeError> {
    match prim {
        Prim::U8 => drop(r.u8()),
        Prim::Bool => drop(r.bool()),
        Prim::U16 => drop(r.u16()),
        Prim::I32 => drop(r.i32()),
        Prim::Decimal => drop(r.decimal("field")?),
        Prim::Code(width) => drop(r.code(width)),
        Prim::Str => r.skip_string("field")?,
        Prim::DateRaw => drop(r.date_raw()),
        Prim::DateClamped => drop(r.date_clamped()),
        Prim::Time => drop(r.time_hms("field")?),
        Prim::Ticks => drop(r.ticks("field")?),
    }
    Ok(())
}

/// Walk a record by the tables of 04 as transcribed in this file and note
/// where every field ends. `None` if a value in it cannot be read.
fn spans<T: Family>(body: &[u8]) -> Option<Vec<Span>> {
    let mut r = Reader::new(body).ok()?;
    let mut spans = Vec::new();
    let mut walk = |r: &mut Reader<'_>, table: &[(&'static str, Prim)]| {
        for &(field, prim) in table {
            skip(r, prim).ok()?;
            spans.push(Span {
                field,
                end: r.cursor(),
                chunk: r.chunk_number(),
            });
        }
        Some(())
    };
    walk(&mut r, HEAD)?;
    if r.more_chunks() {
        walk(&mut r, EXTENSION)?;
    }
    r.next_chunk().ok()?;
    walk(&mut r, T::OWN)?;
    // The tables must account for every byte, like the decoder: nothing
    // unread in a chunk, no chunk unvisited.
    r.finish().ok()?.is_exact().then_some(spans)
}

/// The keys of the serialised record that are settled once a field has been
/// read: they depend on that field and on earlier ones only. Empty for the
/// fields `b4` reads and drops.
fn keys_settled_by(field: &str) -> &'static [&'static str] {
    macro_rules! same {
        ($($name:literal)*) => {
            match field {
                $($name => return &[$name],)*
                _ => {}
            }
        };
    }
    same!("oid" "update_stamp" "amount" "currency" "counterparty_name" "counterparty_account"
        "counterparty_bank_code" "counterparty_country" "scheduled_date" "order_id" "category"
        "counterparty_street" "counterparty_town" "end_to_end_id" "payment_info_id"
        "purpose_code" "category_purpose_code" "mandate_id" "mandate_signed_date" "creditor_id"
        "local_instrument_code" "sequence_type" "ultimate_counterparty_name" "cost_bearer"
        "cost_bearer_account_oid" "intl_remittance_method" "intl_remittance_instruction"
        "counterparty_bank_name" "counterparty_bank_street" "counterparty_bank_town"
        "scheduled_time" "regulatory_reporting_code" "booking_date" "value_date"
        "transaction_code" "booking_text" "primanota" "entry_bits" "split" "batch_oid"
        "bank_reference" "notes" "booking_status" "gvc" "gvc_extension" "analysis_date"
        "transaction_date" "swift_transaction_code" "vat_amount" "vat_rate_type"
        "instructed_amount" "instructed_currency" "charges_amount" "charges_currency"
        "debit_return_order_id" "debit_return_date" "debit_return_time" "virtual_iban" "state"
        "flags" "outbox_oid" "color_flag" "initiator_name" "batch_booking" "import_id"
        "reconciled_booking_oid" "payment_reference" "payee_verification_json"
        "sepa_reason_code" "sepa_additional_info" "payee_verification_choice"
        "first_execution_date" "period_unit" "period_length" "execution_day"
        "last_execution_date" "execution_time");
    match field {
        "class_and_direction" => &["stored_scheme_class", "debit_flag"],
        "purpose_text" => &["purpose_lines"],
        "scheme_type" => &["stored_scheme_type", "scheme_type"],
        "flag" => &["flag", "stored_flag"],
        "sepa_status_code" => &["sepa_status_code", "sepa_status"],
        "next_execution_date" => &["next_execution_date", "stored_next_execution_date"],
        "used_tan"
        | "order_reference_tan"
        | "reserved_a"
        | "reserved_b"
        | "reserved_c"
        | "reserved_d" => &[],
        other => panic!("no keys listed for field `{other}`"),
    }
}

/// Head fields whose value does not depend on whether the record is in the
/// legacy form: the tagged lines of 04 §5.8 change the purpose and fill the
/// extension, nothing else.
const HEAD_KEYS_IN_ANY_FORM: &[&str] = &[
    "oid",
    "update_stamp",
    "stored_scheme_class",
    "debit_flag",
    "amount",
    "currency",
    "counterparty_name",
    "counterparty_account",
    "counterparty_bank_code",
    "counterparty_country",
    "scheduled_date",
    "order_id",
    "category",
];

/// The chunks of a well-formed record as `(start, end)`.
fn chunk_bounds(body: &[u8]) -> Vec<(usize, usize)> {
    let mut bounds = Vec::new();
    let mut start = 0;
    loop {
        let header = u32::from_le_bytes(body[start..start + 4].try_into().unwrap());
        let end = start + (header & 0x3FFF_FFFF) as usize;
        bounds.push((start, end));
        if header & 0x8000_0000 == 0 {
            return bounds;
        }
        start = end;
    }
}

/// A chunk header for `size` bytes.
fn header(size: usize, more: bool) -> [u8; 4] {
    (u32::try_from(size).unwrap() | if more { 0x8000_0000 } else { 0 }).to_le_bytes()
}

/// Decode a cut record. A value error is allowed (bytes of another field
/// can be taken for a time of day or a timestamp once a field is missing); a
/// broken chunk structure is not, because the cut keeps the chunks sound.
fn decode_cut<T: Family>(body: &[u8], context: &str) -> Option<Map<String, Value>> {
    let mut reader = Reader::new(body).unwrap_or_else(|error| panic!("{context}: {error}"));
    match T::decode(&mut reader) {
        Ok(record) => Some(object(&record)),
        Err(error) => {
            assert!(!error.stops_file(), "{context}: {error}");
            None
        }
    }
}

/// Every record of a fixture file cut after every byte, in the three forms a
/// shorter record can take, compared with the complete record: a field that
/// lies before the cut must keep its value, and a cut at a field boundary
/// must give the defaults for everything after it.
fn assert_truncations<T: Family>() -> (usize, usize) {
    let fixture = Fixture::load(T::KIND);
    let (mut cuts, mut settled_values) = (0, 0);
    for (raw, sample) in records(&fixture.bin)
        .map(Result::unwrap)
        .zip(fixture.records())
    {
        let name = sample["name"].as_str().unwrap();
        let body = raw.body;

        // Cut anywhere, without repairing anything: must not panic.
        for length in 0..body.len() {
            let mut file = u32::try_from(length).unwrap().to_le_bytes().to_vec();
            file.extend(&body[..length]);
            let _ = load_collection::<T>(&file);
            let mut file = u32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
            file.extend(&body[..length]);
            assert!(load_collection::<T>(&file).report.stopped.is_some());
        }

        let complete = decode_cut::<T>(body, name).unwrap();
        let spans = spans::<T>(body).unwrap_or_else(|| panic!("{name}: the tables do not fit"));
        let bounds = chunk_bounds(body);
        let (last_start, last_end) = *bounds.last().unwrap();
        let last_chunk = u32::try_from(bounds.len()).unwrap();
        assert_eq!(last_end, body.len(), "{name}: no slack in a fixture record");

        let assert_settled = |cut: &Map<String, Value>, upto: usize, chunk: u32, context: &str| {
            let mut count = 0;
            for span in spans.iter().filter(|s| s.chunk == chunk && s.end <= upto) {
                for key in keys_settled_by(span.field) {
                    assert_eq!(cut[*key], complete[*key], "{context}: {key}");
                    count += 1;
                }
            }
            count
        };

        // 1. The last chunk ends early: an older writer.
        for end in last_start + 4..last_end {
            let context = format!("{name}: last chunk cut at byte {end}");
            let mut cut_body = body[..end].to_vec();
            cut_body[last_start..last_start + 4].copy_from_slice(&header(end - last_start, false));
            cuts += 1;
            let Some(cut) = decode_cut::<T>(&cut_body, &context) else {
                continue;
            };
            // Earlier chunks are untouched.
            for chunk in 1..last_chunk {
                settled_values += assert_settled(&cut, body.len(), chunk, &context);
            }
            settled_values += assert_settled(&cut, end, last_chunk, &context);

            // At a field boundary nothing is misread, so every field after
            // it has its default. Three keys are left out because their
            // default depends on fields that were read: see
            // `an_empty_record_decodes_to_the_documented_defaults`.
            let boundary = end == last_start + 4
                || spans.iter().any(|s| s.chunk == last_chunk && s.end == end);
            if boundary {
                for span in spans
                    .iter()
                    .filter(|s| s.chunk == last_chunk && s.end > end)
                {
                    for key in keys_settled_by(span.field) {
                        if matches!(*key, "booking_status" | "batch_oid" | "scheme_type") {
                            continue;
                        }
                        assert_eq!(cut[*key], default_of(key), "{context}: default of {key}");
                        settled_values += 1;
                    }
                }
            }
        }

        if bounds.len() == 2 {
            let (_, first_end) = bounds[0];
            for end in 4..first_end {
                // 2. The first chunk ends early, the second is complete.
                let context = format!("{name}: first chunk cut at byte {end}");
                let mut cut_body = header(end, true).to_vec();
                cut_body.extend(&body[4..end]);
                cut_body.extend(&body[first_end..]);
                cuts += 1;
                if let Some(cut) = decode_cut::<T>(&cut_body, &context) {
                    assert_eq!(cut["legacy"], json!(false), "{context}");
                    settled_values += assert_settled(&cut, end, 1, &context);
                    // The second chunk reads as before; only the master's
                    // link looks at the object id of the first (04 §6.2).
                    if end >= 12 {
                        settled_values += assert_settled(&cut, usize::MAX, 2, &context);
                    }
                }

                // 3. The same cut without a second chunk: the legacy form,
                // in which the record's own fields follow in the same chunk.
                let context = format!("{name}: cut at byte {end} as a single chunk");
                let mut cut_body = header(end, false).to_vec();
                cut_body.extend(&body[4..end]);
                cuts += 1;
                if let Some(cut) = decode_cut::<T>(&cut_body, &context) {
                    assert_eq!(cut["legacy"], json!(true), "{context}");
                    for span in spans.iter().filter(|s| s.chunk == 1 && s.end <= end) {
                        for key in keys_settled_by(span.field) {
                            if HEAD_KEYS_IN_ANY_FORM.contains(key) {
                                assert_eq!(cut[*key], complete[*key], "{context}: {key}");
                                settled_values += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    (cuts, settled_values)
}

#[test]
fn every_booking_truncated_after_every_byte_keeps_what_was_read() {
    let (cuts, values) = assert_truncations::<Booking>();
    assert_eq!((cuts, values), (92_854, 3_044_858));
}

#[test]
fn every_payment_truncated_after_every_byte_keeps_what_was_read() {
    let (cuts, values) = assert_truncations::<Payment>();
    assert_eq!((cuts, values), (113_436, 3_122_760));
}

#[test]
fn every_standing_order_truncated_after_every_byte_keeps_what_was_read() {
    let (cuts, values) = assert_truncations::<StandingOrder>();
    assert_eq!((cuts, values), (44_040, 772_829));
}

// --------------------------------------------------------------- mutation

/// xorshift64*, so that the mutations are the same in every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

#[test]
fn damaged_files_never_panic_the_decoders_or_the_collection_rules() {
    let mut rng = Rng(0x0B4_0B4_0B4);
    let mut decoded = 0;
    for kind in ["booking", "payment", "standing_order"] {
        let fixture = Fixture::load(kind);
        for _ in 0..1_500 {
            let mut data = fixture.bin.clone();
            for _ in 0..=rng.below(8) {
                let at = rng.below(data.len());
                data[at] = match rng.below(4) {
                    0 => 0,
                    1 => 0xFF,
                    2 => data[at].wrapping_add(1),
                    _ => rng.next() as u8,
                };
            }
            if rng.below(4) == 0 {
                data.truncate(rng.below(data.len()));
            }
            // Each file as each record type: the layouts differ after the
            // base, so this also feeds every decoder bytes meant for another.
            let mut bookings = load_collection::<Booking>(&data).records;
            regroup_bookings(&mut bookings, today());
            let _ = banking4_booking_order(&bookings, today());
            let mut payments = load_collection::<Payment>(&data).records;
            apply_payment_rules(&mut payments, today());
            let _ = banking4_payment_order(&payments, today());
            let orders = load_collection::<StandingOrder>(&data).records;
            decoded += bookings.len() + payments.len() + orders.len();
            for booking in &bookings {
                let _ = (booking.signed_amount(), booking.net_amount());
                let _ = serde_json::to_string(booking).unwrap();
            }
        }
    }
    assert!(decoded > 100_000, "only {decoded} records decoded");
}
