//! Decode the master data and portfolio collections of a directory of
//! decrypted vault entries and write them as JSON lines.
//!
//! A development aid for comparing the output of this crate's decoders with
//! data exported from Banking4. It reads the directory named by `B4_RAW_DIR`,
//! laid out like a vault (root files, `Acct<oid>/...`, `Stock<oid>/...`), and
//! writes one file per record type into the directory named by
//! `B4_DUMP_DIR`. Records of a per-account or per-instrument file get the
//! owner's object id as `account_oid` or `stock_oid`.
//!
//! ```console
//! $ B4_RAW_DIR=raw B4_DUMP_DIR=dump cargo run --example dump_masterdata
//! ```
//!
//! What it prints is counts only: per vault entry what the loader met, per
//! decimal field the scales that occur, and what the rules of Banking4's
//! lists would hide. The dump files hold the data of the vault: keep them
//! where the vault is kept.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use b4::model::base::{load_collection, LoadReport, Record};
use b4::model::masterdata::{
    category_loader_view, duplicate_isins, exchange_rate_view, missing_special_categories,
    stock_directory_oid, Asset, Category, ExchangeRate, Mandate, Payee, Payer, PortfolioItem,
    PortfolioTransaction, StockInstrument, StockPrice, TaxExemption,
};
use serde::Serialize;
use serde_json::{Map, Value};

/// Integer digits a `DECIMAL(18,4)` column holds.
const MONEY_INTEGER_DIGITS: usize = 14;
/// Decimals a `DECIMAL(18,4)` column holds.
const MONEY_SCALE: usize = 4;
/// Integer digits a `DECIMAL(28,10)` column holds.
const RATE_INTEGER_DIGITS: usize = 18;
/// Decimals a `DECIMAL(28,10)` column holds.
const RATE_SCALE: usize = 10;

/// What the loader met in all files of one vault entry name.
#[derive(Debug, Default)]
struct EntryTotals {
    files: usize,
    records: usize,
    undecodable: usize,
    records_with_unread_bytes: usize,
    /// (chunk, bytes left) -> records.
    unread_histogram: BTreeMap<(u32, usize), usize>,
    records_with_extra_chunks: usize,
    extra_chunks: usize,
    legacy_records: usize,
    records_with_short_chunks: usize,
    records_with_repeated_oid: usize,
    files_stopped_early: usize,
    files_with_stray_bytes: usize,
    files_with_slack: usize,
    records_with_overlong_text: usize,
}

impl EntryTotals {
    fn add(&mut self, report: &LoadReport) {
        self.files += 1;
        self.records += report.decoded;
        self.undecodable += report.undecodable;
        self.records_with_unread_bytes += report.unread_bytes.records;
        for (unread, records) in &report.unread_histogram {
            *self
                .unread_histogram
                .entry((unread.chunk, unread.bytes))
                .or_default() += records;
        }
        self.records_with_extra_chunks += report.extra_chunks.records;
        self.extra_chunks += report.extra_chunk_count;
        self.legacy_records += report.legacy.records;
        self.records_with_short_chunks += report.short_chunks.records;
        self.records_with_repeated_oid += report.duplicate_oids.records;
        self.files_stopped_early += usize::from(report.stopped.is_some());
        self.files_with_stray_bytes += usize::from(report.stray_bytes > 0);
        self.files_with_slack += usize::from(!report.slack.is_zero());
        self.records_with_overlong_text += report.overlong_strings.records;
    }
}

/// The scales of one decimal field and whether its values fit the column
/// types the database plans to use.
#[derive(Debug, Default)]
struct ScaleCount {
    /// Number of decimals -> values.
    by_scale: BTreeMap<usize, usize>,
    zero_values: usize,
    beyond_money_column: usize,
    beyond_rate_column: usize,
}

impl ScaleCount {
    fn add(&mut self, text: &str) {
        let digits = text.trim_start_matches('-');
        let (integer, fraction) = digits.split_once('.').unwrap_or((digits, ""));
        *self.by_scale.entry(fraction.len()).or_default() += 1;
        if digits.chars().all(|c| c == '0' || c == '.') {
            self.zero_values += 1;
        }
        // Trailing zeros can be dropped without rounding.
        let needed = fraction.trim_end_matches('0').len();
        let integer = integer.trim_start_matches('0').len();
        if integer > MONEY_INTEGER_DIGITS || needed > MONEY_SCALE {
            self.beyond_money_column += 1;
        }
        if integer > RATE_INTEGER_DIGITS || needed > RATE_SCALE {
            self.beyond_rate_column += 1;
        }
    }
}

/// Everything the run collects besides the dump files.
#[derive(Debug, Default)]
struct Summary {
    entries: BTreeMap<String, EntryTotals>,
    /// "record.field" -> scales.
    scales: BTreeMap<String, ScaleCount>,
    /// Entries of the raw directory that are none of this module's.
    other_entries: usize,
}

/// One output file per record type, created when its first record arrives.
struct Dump {
    directory: PathBuf,
    files: BTreeMap<&'static str, BufWriter<File>>,
}

impl Dump {
    fn writer(&mut self, name: &'static str) -> Result<&mut BufWriter<File>, Box<dyn Error>> {
        if !self.files.contains_key(name) {
            let path = self.directory.join(format!("{name}.jsonl"));
            self.files.insert(name, BufWriter::new(File::create(path)?));
        }
        self.files
            .get_mut(name)
            .ok_or_else(|| "output file vanished".into())
    }
}

/// The decimal fields of a record type whose scales are counted.
fn decimal_fields(output: &str) -> &'static [&'static str] {
    match output {
        "payees" => &["amount"],
        "categories" => &["budget_amount", "vat_percent"],
        "exchange_rates" => &["quote_rate"],
        "stock_prices" => &["price"],
        "portfolio_items" => &[
            "quantity",
            "stored_interest_rate",
            "stored_purchase_price",
            "stored_current_price",
            "stored_alert_lower_limit",
            "stored_alert_upper_limit",
        ],
        "portfolio_transactions" => &[
            "quantity",
            "settlement_amount",
            "interest_amount",
            "trading_price",
        ],
        "tax_exemptions" => &["amount", "used_amount", "remaining_amount"],
        "assets" => &["amount", "interest_rate"],
        _ => &[],
    }
}

/// Decode one collection file, append its records to the dump and add what
/// the loader met to the summary. Returns the decoded records.
fn dump_file<T: Record + Serialize>(
    path: &Path,
    entry: &str,
    output: &'static str,
    owner: Option<(&str, i32)>,
    dump: &mut Dump,
    summary: &mut Summary,
) -> Result<Vec<T>, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    let collection = load_collection::<T>(&bytes);
    summary
        .entries
        .entry(entry.to_owned())
        .or_default()
        .add(&collection.report);

    let writer = dump.writer(output)?;
    for record in &collection.records {
        let Value::Object(fields) = serde_json::to_value(record)? else {
            return Err("a record does not serialise as an object".into());
        };
        for &field in decimal_fields(output) {
            if let Some(text) = fields.get(field).and_then(Value::as_str) {
                summary
                    .scales
                    .entry(format!("{output}.{field}"))
                    .or_default()
                    .add(text);
            }
        }
        let mut line = Map::new();
        if let Some((key, oid)) = owner {
            line.insert(key.to_owned(), Value::from(oid));
        }
        line.extend(fields);
        serde_json::to_writer(&mut *writer, &line)?;
        writer.write_all(b"\n")?;
    }
    Ok(collection.records)
}

/// The object id in a directory name `<prefix><oid>`.
fn directory_oid(name: &str, prefix: &str) -> Option<i32> {
    let digits = name.strip_prefix(prefix)?;
    let oid: i32 = digits.parse().ok()?;
    (oid.to_string() == digits).then_some(oid)
}

fn count<T>(label: &str, items: impl IntoIterator<Item = T>) {
    println!("  {label}: {}", items.into_iter().count());
}

fn run() -> Result<(), Box<dyn Error>> {
    let raw = PathBuf::from(std::env::var_os("B4_RAW_DIR").ok_or("B4_RAW_DIR is not set")?);
    let directory = PathBuf::from(std::env::var_os("B4_DUMP_DIR").ok_or("B4_DUMP_DIR is not set")?);
    fs::create_dir_all(&directory)?;
    let mut dump = Dump {
        directory,
        files: BTreeMap::new(),
    };
    let mut summary = Summary::default();

    let mut names: Vec<(String, PathBuf, bool)> = Vec::new();
    for entry in fs::read_dir(&raw)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        names.push((name, entry.path(), entry.file_type()?.is_dir()));
    }
    names.sort();

    let mut categories: Vec<Category> = Vec::new();
    let mut mandates: Vec<Mandate> = Vec::new();
    let mut rates: Vec<ExchangeRate> = Vec::new();
    let mut instruments: Vec<StockInstrument> = Vec::new();
    let mut items: Vec<PortfolioItem> = Vec::new();
    let mut payees: Vec<Payee> = Vec::new();
    let mut price_directories: Vec<i32> = Vec::new();

    for (name, path, is_dir) in &names {
        let (d, s) = (&mut dump, &mut summary);
        match (name.as_str(), is_dir) {
            ("Payees", false) => payees = dump_file(path, name, "payees", None, d, s)?,
            ("Payers", false) => {
                dump_file::<Payer>(path, name, "payers", None, d, s)?;
            }
            ("Categories", false) => {
                categories = dump_file(path, name, "categories", None, d, s)?;
            }
            ("Mandates", false) => mandates = dump_file(path, name, "mandates", None, d, s)?,
            ("ExchangeRatesBIN", false) => {
                rates = dump_file(path, name, "exchange_rates", None, d, s)?;
            }
            ("StockInstruments", false) => {
                instruments = dump_file(path, name, "stock_instruments", None, d, s)?;
            }
            (_, true) if stock_directory_oid(name).is_some() => {
                let Some(oid) = stock_directory_oid(name) else {
                    continue;
                };
                let file = path.join("StockPrices");
                if file.is_file() {
                    let owner = Some(("stock_oid", oid));
                    dump_file::<StockPrice>(&file, "StockPrices", "stock_prices", owner, d, s)?;
                    price_directories.push(oid);
                }
            }
            (_, true) if directory_oid(name, "Acct").is_some() => {
                let Some(oid) = directory_oid(name, "Acct") else {
                    continue;
                };
                let owner = Some(("account_oid", oid));
                let file = |entry: &str| Some(path.join(entry)).filter(|file| file.is_file());
                if let Some(file) = file("Portf") {
                    items.extend(dump_file(&file, "Portf", "portfolio_items", owner, d, s)?);
                }
                if let Some(file) = file("PortfTxns") {
                    let output = "portfolio_transactions";
                    dump_file::<PortfolioTransaction>(&file, "PortfTxns", output, owner, d, s)?;
                }
                if let Some(file) = file("AcctTaxExemptions") {
                    let entry = "AcctTaxExemptions";
                    dump_file::<TaxExemption>(&file, entry, "tax_exemptions", owner, d, s)?;
                }
                if let Some(file) = file("Assets") {
                    dump_file::<Asset>(&file, "Assets", "assets", owner, d, s)?;
                }
            }
            _ => summary.other_entries += 1,
        }
    }
    for writer in dump.files.values_mut() {
        writer.flush()?;
    }

    println!("loader counters per vault entry");
    for (entry, totals) in &summary.entries {
        println!("  {entry}: {totals:?}");
    }
    println!(
        "entries of the directory that are not master data: {}",
        summary.other_entries
    );

    println!("decimal scales per field (decimals: values)");
    for (field, scales) in &summary.scales {
        println!("  {field}: {scales:?}");
    }

    println!("what Banking4's lists would do (counts)");
    let marks = category_loader_view(&categories);
    count(
        "categories dropped for having no name",
        marks.iter().filter(|m| m.dropped),
    );
    count(
        "categories whose special role a later record repeats",
        marks.iter().zip(&categories).filter(|(mark, category)| {
            !mark.dropped && mark.special_category != category.special_category
        }),
    );
    count(
        "special categories missing from the file",
        missing_special_categories(&categories),
    );
    count(
        "categories with rules",
        categories.iter().filter(|c| !c.parsed_rules.is_empty()),
    );
    count(
        "categories with a layout version other than 9",
        categories.iter().filter(|c| c.layout_version != 9),
    );
    count(
        "categories whose stored values the reader adjusts",
        categories.iter().filter(|c| {
            c.full_name != c.stored_full_name
                || c.totals_bucket != c.stored_totals_bucket
                || c.budget_period != c.stored_budget_period
                || c.budget_amount != c.stored_budget_amount
                || c.icon_name != c.stored_icon_name
        }),
    );
    let mut drops: BTreeMap<String, usize> = BTreeMap::new();
    for view in exchange_rate_view(&rates) {
        let reason = view
            .dropped
            .map_or("shown".to_owned(), |reason| format!("{reason:?}"));
        *drops.entry(reason).or_default() += 1;
    }
    println!("  exchange rates: {drops:?}");
    let mut drops: BTreeMap<String, usize> = BTreeMap::new();
    for mandate in &mandates {
        let reason = mandate
            .loader_drop_reason()
            .map_or("not dropped by the two known rules".to_owned(), |reason| {
                format!("{reason:?}")
            });
        *drops.entry(reason).or_default() += 1;
    }
    println!("  mandates: {drops:?}");
    count(
        "payees with a legacy mandate",
        payees.iter().filter(|p| p.has_legacy_mandate()),
    );
    count(
        "payees whose stored kind the reader replaces",
        payees
            .iter()
            .filter(|p| p.payee_kind != p.stored_payee_kind),
    );

    println!("securities (counts)");
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for instrument in &instruments {
        *kinds.entry(instrument.isin_kind.name()).or_default() += 1;
    }
    println!("  stock instruments by identifier kind: {kinds:?}");
    count(
        "stock instruments with an ISIN an earlier one has",
        duplicate_isins(&instruments),
    );
    count(
        "stock instruments with splits",
        instruments.iter().filter(|i| i.splits.is_some()),
    );
    count(
        "stock instruments with malformed split lines",
        instruments.iter().filter(|i| i.malformed_split_lines > 0),
    );
    count(
        "stock instruments with a price directory",
        instruments
            .iter()
            .filter(|i| price_directories.contains(&i.oid())),
    );
    count(
        "price directories without a stock instrument",
        price_directories
            .iter()
            .filter(|oid| instruments.iter().all(|i| i.oid() != **oid)),
    );
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for item in &items {
        *kinds.entry(item.isin_kind.name()).or_default() += 1;
    }
    println!("  portfolio items by identifier kind: {kinds:?}");
    count(
        "portfolio items with a stock instrument",
        items
            .iter()
            .filter(|i| i.instrument(&instruments).is_some()),
    );
    count(
        "portfolio items whose stored values the reader adjusts",
        items.iter().filter(|i| {
            i.interest_rate != i.stored_interest_rate
                || i.purchase_price != i.stored_purchase_price
                || i.current_price != i.stored_current_price
                || i.alert_lower_limit != i.stored_alert_lower_limit
                || i.alert_upper_limit != i.stored_alert_upper_limit
                || i.quantity_currency != i.stored_quantity_currency
                || i.purchase_price_currency != i.stored_purchase_price_currency
        }),
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
