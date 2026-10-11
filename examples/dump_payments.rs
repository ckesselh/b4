//! Decode the bookings, payments and standing orders of a directory of
//! decrypted collection files and write them as JSON lines, for comparing
//! the output of `b4`'s decoders with data exported from Banking4.
//!
//! ```text
//! B4_RAW_DIR=<dir> B4_DUMP_DIR=<dir> cargo run --example dump_payments
//! ```
//!
//! `B4_RAW_DIR` is laid out like a vault: `Acct<oid>/Stmt`,
//! `Acct<oid>/StmtMasterEntries`, `Acct<oid>/Paymts`, `Acct<oid>/Stos`.
//! `B4_DUMP_DIR` receives `bookings.jsonl`, `payments.jsonl` and
//! `standing_orders.jsonl`, one object per record:
//!
//! - `account_oid`, `file`, `index`: where the record is;
//! - `record`: the record as decoded, before anything that depends on the
//!   account or on the other records of the file;
//! - `computed`: what the record's methods give (direction, signed amount,
//!   joined purpose, ...);
//! - `listed`: the values that the account-dependent adjustments and the
//!   collection rules change or add.
//!
//! The account-dependent values need the kind and currency of each account.
//! `B4_ACCOUNTS` may name a file with one line per account: object id,
//! account kind as a number and currency, separated by tabs. Without it
//! every account counts as a bank account without a currency.
//!
//! `B4_TODAY` (`YYYY-MM-DD`) sets the date that stands in for undated
//! records when Banking4's order is derived; the default is the current
//! date.
//!
//! Standard output gets counts only, never a value from a record: what the
//! loader tolerated per file type, the scales of the amounts, and how many
//! records the collection rules mark.

use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use b4::model::base::{load_collection, LoadReport, Record};
use b4::model::payments::{
    apply_payment_rules, regroup_bookings, Booking, Payment, PaymentBase, PaymentRules, Regrouping,
    SchemeType, StandingOrder,
};
use b4::wire::{Date, UnreadBytes};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{json, Value};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// What is known about the account a file belongs to.
#[derive(Debug, Clone, Default)]
struct Account {
    /// Account kind (04 §11.22); 1, a bank account, if unknown.
    kind: u8,
    currency: Option<String>,
}

/// The loader's counters summed over all files of one type.
#[derive(Debug, Default)]
struct Totals {
    files: usize,
    stopped_files: usize,
    decoded: usize,
    undecodable: usize,
    unread_records: usize,
    unread_histogram: BTreeMap<UnreadBytes, usize>,
    extra_chunk_records: usize,
    extra_chunks: usize,
    legacy: usize,
    short_chunks: usize,
    overlong_strings: usize,
    slack: usize,
    duplicate_oids: usize,
    stray_bytes: usize,
    /// Number of amounts per decimal scale, by field name.
    scales: BTreeMap<&'static str, BTreeMap<u32, usize>>,
    /// Other counts by name.
    facts: BTreeMap<&'static str, usize>,
}

impl Totals {
    fn add_report(&mut self, report: &LoadReport) {
        self.files += 1;
        self.stopped_files += usize::from(report.stopped.is_some());
        self.decoded += report.decoded;
        self.undecodable += report.undecodable;
        self.unread_records += report.unread_bytes.records;
        for (unread, records) in &report.unread_histogram {
            *self.unread_histogram.entry(*unread).or_default() += records;
        }
        self.extra_chunk_records += report.extra_chunks.records;
        self.extra_chunks += report.extra_chunk_count;
        self.legacy += report.legacy.records;
        self.short_chunks += report.short_chunks.records;
        self.overlong_strings += report.overlong_strings.records;
        self.slack += report.slack.records;
        self.duplicate_oids += report.duplicate_oids.records;
        self.stray_bytes += report.stray_bytes;
    }

    fn scale(&mut self, field: &'static str, amount: Decimal) {
        *self
            .scales
            .entry(field)
            .or_default()
            .entry(amount.scale())
            .or_default() += 1;
    }

    fn fact(&mut self, name: &'static str, count: usize) {
        *self.facts.entry(name).or_default() += count;
    }

    fn base_facts(&mut self, base: &PaymentBase) {
        self.scale("amount", base.amount);
        self.fact(
            "amount is negative",
            usize::from(base.amount.is_sign_negative()),
        );
        self.fact(
            "stored scheme type is not none",
            usize::from(base.stored_scheme_type != SchemeType::None),
        );
        self.fact(
            "scheme type was derived",
            usize::from(base.stored_scheme_type != base.scheme_type),
        );
        self.fact("no stored currency", usize::from(base.currency.is_none()));
        self.fact(
            "direction differs from the stored bit",
            usize::from(base.is_debit() != base.debit_flag),
        );
    }

    fn print(&self, name: &str) {
        println!("{name}:");
        println!("  files                          {}", self.files);
        println!("  files that stopped early       {}", self.stopped_files);
        println!("  records decoded                {}", self.decoded);
        println!("  records undecodable            {}", self.undecodable);
        println!("  records with leftover bytes    {}", self.unread_records);
        for (unread, records) in &self.unread_histogram {
            println!(
                "    chunk {}: {} bytes in {} records",
                unread.chunk, unread.bytes, records
            );
        }
        println!(
            "  records with extra chunks      {} ({} chunks)",
            self.extra_chunk_records, self.extra_chunks
        );
        println!("  legacy single-chunk records    {}", self.legacy);
        println!("  records with short chunks      {}", self.short_chunks);
        println!("  records with an over-long text {}", self.overlong_strings);
        println!("  records with slack             {}", self.slack);
        println!("  repeated object ids            {}", self.duplicate_oids);
        println!("  stray bytes                    {}", self.stray_bytes);
        for (field, scales) in &self.scales {
            let text: Vec<String> = scales
                .iter()
                .map(|(scale, count)| format!("scale {scale}: {count}"))
                .collect();
            println!("  decimal scales of {field}: {}", text.join(", "));
        }
        for (fact, count) in &self.facts {
            println!("  {fact}: {count}");
        }
    }
}

/// One line of a dump file.
#[derive(Serialize)]
struct Line<'a, T: Serialize> {
    account_oid: i64,
    file: &'a str,
    index: usize,
    record: &'a T,
    computed: Value,
    listed: Value,
}

fn write_line<T: Serialize>(out: &mut impl Write, line: &Line<'_, T>) -> Result<()> {
    serde_json::to_writer(&mut *out, line)?;
    out.write_all(b"\n")?;
    Ok(())
}

fn computed_of_base(base: &PaymentBase) -> serde_json::Map<String, Value> {
    let mut computed = serde_json::Map::new();
    computed.insert("is_debit".into(), json!(base.is_debit()));
    computed.insert(
        "signed_amount".into(),
        json!(base.signed_amount().to_string()),
    );
    computed.insert("purpose".into(), json!(base.purpose()));
    computed.insert(
        "reported_scheduled_time".into(),
        json!(base.reported_scheduled_time()),
    );
    computed.insert(
        "written_scheme_class".into(),
        json!(base.scheme_type.class()),
    );
    computed
}

/// The `Acct<oid>` directories of `raw`, with their object ids, in id order.
fn account_directories(raw: &Path) -> Result<Vec<(i64, PathBuf)>> {
    let mut directories = Vec::new();
    for entry in fs::read_dir(raw)? {
        let entry = entry?;
        let name = entry.file_name();
        let oid = name
            .to_str()
            .and_then(|name| name.strip_prefix("Acct"))
            .and_then(|digits| digits.parse::<i64>().ok());
        if let (Some(oid), true) = (oid, entry.path().is_dir()) {
            directories.push((oid, entry.path()));
        }
    }
    directories.sort();
    Ok(directories)
}

fn read_accounts(path: &Path) -> Result<HashMap<i64, Account>> {
    let mut accounts = HashMap::new();
    for line in fs::read_to_string(path)?.lines() {
        let mut parts = line.split('\t');
        let (Some(oid), Some(kind)) = (parts.next(), parts.next()) else {
            continue;
        };
        let currency = parts.next().filter(|code| !code.is_empty());
        accounts.insert(
            oid.parse()?,
            Account {
                kind: kind.parse()?,
                currency: currency.map(str::to_owned),
            },
        );
    }
    Ok(accounts)
}

fn today() -> Result<Date> {
    if let Ok(text) = std::env::var("B4_TODAY") {
        let mut parts = text.split('-');
        let mut part = || parts.next().ok_or("B4_TODAY is not YYYY-MM-DD");
        let date = Date::new(part()?.parse()?, part()?.parse()?, part()?.parse()?);
        return Ok(date.ok_or("B4_TODAY is not a date")?);
    }
    let days = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() / 86_400;
    let date = i32::try_from(days)
        .ok()
        .and_then(Date::from_days_since_unix_epoch);
    Ok(date.ok_or("the system clock is out of range")?)
}

/// Load one file of an account directory, or nothing if it does not exist.
fn load<T: Record>(directory: &Path, file: &str, totals: &mut Totals) -> Result<Vec<T>> {
    let path = directory.join(file);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let collection = load_collection::<T>(&fs::read(path)?);
    totals.add_report(&collection.report);
    Ok(collection.records)
}

fn add_regrouping(sum: &mut Regrouping, counts: Regrouping) {
    sum.groups += counts.groups;
    sum.slaves += counts.slaves;
    sum.ungrouped += counts.ungrouped;
}

fn main() -> Result<()> {
    let raw = PathBuf::from(std::env::var_os("B4_RAW_DIR").ok_or("B4_RAW_DIR is not set")?);
    let dump = PathBuf::from(std::env::var_os("B4_DUMP_DIR").ok_or("B4_DUMP_DIR is not set")?);
    let accounts = match std::env::var_os("B4_ACCOUNTS") {
        Some(path) => read_accounts(Path::new(&path))?,
        None => HashMap::new(),
    };
    let today = today()?;
    fs::create_dir_all(&dump)?;
    let create = |name: &str| File::create(dump.join(name)).map(BufWriter::new);
    let mut bookings_out = create("bookings.jsonl")?;
    let mut payments_out = create("payments.jsonl")?;
    let mut orders_out = create("standing_orders.jsonl")?;

    let (mut stmt, mut legacy_stmt, mut paymts, mut stos) = (
        Totals::default(),
        Totals::default(),
        Totals::default(),
        Totals::default(),
    );
    let mut booking_groups = Regrouping::default();
    let mut payment_rules = PaymentRules::default();
    let mut accounts_known = 0;

    for (oid, directory) in account_directories(&raw)? {
        let account = accounts.get(&oid).cloned().unwrap_or(Account {
            kind: 1,
            currency: None,
        });
        accounts_known += usize::from(accounts.contains_key(&oid));
        let currency = account.currency.as_deref();

        // Bookings: `Stmt` and, if there is one, the legacy file with more
        // bookings of the same account (04 §1). The list rules run over both.
        let from_stmt: Vec<Booking> = load(&directory, "Stmt", &mut stmt)?;
        let from_legacy: Vec<Booking> = load(&directory, "StmtMasterEntries", &mut legacy_stmt)?;
        let split_at = from_stmt.len();
        let decoded: Vec<Booking> = from_stmt.into_iter().chain(from_legacy).collect();
        let mut listed = decoded.clone();
        for booking in &mut listed {
            booking.apply_account_kind(account.kind);
        }
        add_regrouping(&mut booking_groups, regroup_bookings(&mut listed, today));
        for (index, (record, listed)) in decoded.iter().zip(&listed).enumerate() {
            stmt.base_facts(&record.base);
            stmt.scale("vat_amount", record.vat_amount);
            stmt.scale("instructed_amount", record.instructed_amount);
            stmt.scale("charges_amount", record.charges_amount);
            stmt.fact(
                "stored flag is out of range",
                usize::from(record.flag != record.stored_flag),
            );
            stmt.fact("masters (not counted)", usize::from(!listed.is_counted()));
            stmt.fact(
                "stored currency differs from the account's",
                usize::from(currency.is_some() && record.base.currency.as_deref() != currency),
            );
            let mut computed = computed_of_base(&record.base);
            computed.insert(
                "net_amount".into(),
                json!(record.net_amount().map(|n| n.to_string())),
            );
            computed.insert(
                "displayed_category".into(),
                json!(record.displayed_category()),
            );
            computed.insert(
                "adjusted_currency".into(),
                json!(record.adjusted_currency(account.kind, currency)),
            );
            computed.insert("displayed_currency".into(), json!(currency));
            let (file, index) = if index < split_at {
                ("Stmt", index)
            } else {
                ("StmtMasterEntries", index - split_at)
            };
            write_line(
                &mut bookings_out,
                &Line {
                    account_oid: oid,
                    file,
                    index,
                    record,
                    computed: Value::Object(computed),
                    listed: json!({
                        "value_date": listed.value_date,
                        "analysis_date": listed.analysis_date,
                        "transaction_date": listed.transaction_date,
                        "batch_index": listed.batch_index,
                        "ungrouped_by_banking4": listed.ungrouped_by_banking4,
                        "batch_oid": listed.effective_batch_oid(),
                        "split": listed.effective_split(),
                        "is_counted": listed.is_counted(),
                        "displayed_category": listed.displayed_category(),
                    }),
                },
            )?;
        }

        // Payments.
        let decoded: Vec<Payment> = load(&directory, "Paymts", &mut paymts)?;
        let mut listed = decoded.clone();
        let rules = apply_payment_rules(&mut listed, today);
        payment_rules.hidden += rules.hidden;
        payment_rules.legacy_states += rules.legacy_states;
        payment_rules.legacy_flags += rules.legacy_flags;
        payment_rules.legacy_masters += rules.legacy_masters;
        payment_rules.legacy_master_conflicts += rules.legacy_master_conflicts;
        payment_rules.legacy_slaves += rules.legacy_slaves;
        add_regrouping(&mut payment_rules.regrouping, rules.regrouping);
        for (index, (record, listed)) in decoded.iter().zip(&listed).enumerate() {
            paymts.base_facts(&record.base);
            let mut computed = computed_of_base(&record.base);
            computed.insert(
                "displayed_category".into(),
                json!(record.displayed_category()),
            );
            computed.insert(
                "displayed_currency".into(),
                json!(record.displayed_currency(currency)),
            );
            write_line(
                &mut payments_out,
                &Line {
                    account_oid: oid,
                    file: "Paymts",
                    index,
                    record,
                    computed: Value::Object(computed),
                    listed: json!({
                        "state": listed.state,
                        "flags": listed.flags,
                        "scheduled_date": listed.base.scheduled_date,
                        "scheduled_time": listed.base.scheduled_time,
                        "hidden_by_banking4": listed.hidden_by_banking4,
                        "batch_index": listed.batch_index,
                        "ungrouped_by_banking4": listed.ungrouped_by_banking4,
                        "batch_oid": listed.effective_batch_oid(),
                        "displayed_category": listed.displayed_category(),
                    }),
                },
            )?;
        }

        // Standing orders: no list rules.
        let decoded: Vec<StandingOrder> = load(&directory, "Stos", &mut stos)?;
        for (index, record) in decoded.iter().enumerate() {
            stos.base_facts(&record.base);
            stos.fact(
                "stored next execution date is discarded",
                usize::from(record.next_execution_date != record.stored_next_execution_date),
            );
            let mut computed = computed_of_base(&record.base);
            computed.insert("displayed_category".into(), json!(record.base.category));
            computed.insert(
                "displayed_currency".into(),
                json!(record.displayed_currency(currency)),
            );
            write_line(
                &mut orders_out,
                &Line {
                    account_oid: oid,
                    file: "Stos",
                    index,
                    record,
                    computed: Value::Object(computed),
                    listed: Value::Null,
                },
            )?;
        }
    }
    bookings_out.flush()?;
    payments_out.flush()?;
    orders_out.flush()?;

    println!("accounts with a known kind and currency: {accounts_known}");
    stmt.print("Stmt (bookings)");
    if legacy_stmt.files > 0 {
        legacy_stmt.print("StmtMasterEntries (bookings, legacy file)");
    }
    paymts.print("Paymts (payments)");
    stos.print("Stos (standing orders)");
    println!("collection rules, bookings (04 §6.4):");
    println!(
        "  groups that hold                {}",
        booking_groups.groups
    );
    println!(
        "  slaves in them                  {}",
        booking_groups.slaves
    );
    println!(
        "  records taken out of a group    {}",
        booking_groups.ungrouped
    );
    println!("collection rules, payments (04 §8.3):");
    println!("  hidden by Banking4              {}", payment_rules.hidden);
    println!(
        "  legacy states replaced          {}",
        payment_rules.legacy_states
    );
    println!(
        "  legacy flag bits cleared        {}",
        payment_rules.legacy_flags
    );
    println!(
        "  legacy batch masters            {}",
        payment_rules.legacy_masters
    );
    println!(
        "  legacy batch master conflicts   {}",
        payment_rules.legacy_master_conflicts
    );
    println!(
        "  legacy batch slaves relinked    {}",
        payment_rules.legacy_slaves
    );
    println!(
        "  groups that hold                {}",
        payment_rules.regrouping.groups
    );
    println!(
        "  slaves in them                  {}",
        payment_rules.regrouping.slaves
    );
    println!(
        "  records taken out of a group    {}",
        payment_rules.regrouping.ungrouped
    );
    Ok(())
}
