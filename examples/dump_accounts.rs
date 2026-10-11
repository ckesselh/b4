//! Decode the account files of a decrypted vault and write them as JSON
//! lines, for comparing `b4`'s output with data exported from Banking4.
//!
//! ```console
//! $ B4_RAW_DIR=raw B4_DUMP_DIR=dump cargo run --example dump_accounts
//! ```
//!
//! `B4_RAW_DIR` names a directory with the decrypted entries of a vault
//! under their own names (`AcctsNew` or `Accts`, `AcctGroups`). Into
//! `B4_DUMP_DIR` go `accounts.jsonl` and `account_groups.jsonl`, one decoded
//! record per line in file order, and next to each a `*.undecodable.jsonl`
//! naming the records that could not be decoded by index, offset and reason.
//!
//! The dump files hold the data of the vault and belong wherever the vault's
//! data may be. What this program prints is counts and entry names only.

use std::error::Error;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use b4::model::accounts::{
    authoritative_accounts_file, load_accounts, Account, AccountGroup, ACCOUNTS_FILE,
    ACCOUNT_GROUPS_FILE, LEGACY_ACCOUNTS_FILE,
};
use b4::model::base::{load_collection, DecodeError, LoadReport, Record};
use b4::wire::records;
use serde::Serialize;
use serde_json::json;

fn main() -> Result<(), Box<dyn Error>> {
    let raw_dir = directory_from("B4_RAW_DIR")?;
    let dump_dir = directory_from("B4_DUMP_DIR")?;
    fs::create_dir_all(&dump_dir)?;

    let accounts_file = authoritative_accounts_file(
        raw_dir.join(ACCOUNTS_FILE).is_file(),
        raw_dir.join(LEGACY_ACCOUNTS_FILE).is_file(),
    );
    match accounts_file {
        Some(name) => {
            let bytes = fs::read(raw_dir.join(name))?;
            dump::<Account>(&bytes, &dump_dir, "accounts")?;
            let loaded = load_accounts(&bytes);
            print_counts(name, &loaded.report);
            println!(
                "  records of kind none (dropped by a loader): {}",
                loaded.placeholders
            );
            println!(
                "  records of a version above the documented one: {} (highest version: {})",
                loaded.newer_versions, loaded.highest_version
            );
            for warning in loaded.warnings(name) {
                println!("  warning: {warning}");
            }
        }
        None => println!("no account file"),
    }

    let groups_path = raw_dir.join(ACCOUNT_GROUPS_FILE);
    if groups_path.is_file() {
        let bytes = fs::read(groups_path)?;
        dump::<AccountGroup>(&bytes, &dump_dir, "account_groups")?;
        let report = load_collection::<AccountGroup>(&bytes).report;
        print_counts(ACCOUNT_GROUPS_FILE, &report);
        for warning in report.warnings(ACCOUNT_GROUPS_FILE) {
            println!("  warning: {warning}");
        }
    } else {
        println!("{ACCOUNT_GROUPS_FILE}: no such entry (an empty list)");
    }
    Ok(())
}

/// The directory an environment variable names.
fn directory_from(variable: &str) -> Result<PathBuf, Box<dyn Error>> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .ok_or_else(|| format!("set {variable} to a directory").into())
}

/// Write every record of a collection file as one JSON line, and the records
/// that cannot be decoded to a second file.
fn dump<T: Record + Serialize>(
    bytes: &[u8],
    dump_dir: &Path,
    stem: &str,
) -> Result<(), Box<dyn Error>> {
    let mut decoded = BufWriter::new(File::create(dump_dir.join(format!("{stem}.jsonl")))?);
    let mut undecodable = BufWriter::new(File::create(
        dump_dir.join(format!("{stem}.undecodable.jsonl")),
    )?);
    for raw in records(bytes) {
        // A file that cannot be cut into records any further ends here; the
        // loader's report, printed by the caller, says so.
        let Ok(raw) = raw else { break };
        let outcome = raw
            .reader()
            .map_err(DecodeError::from)
            .and_then(|mut reader| T::decode(&mut reader));
        match outcome {
            Ok(record) => {
                serde_json::to_writer(&mut decoded, &record)?;
                decoded.write_all(b"\n")?;
            }
            Err(error) => {
                let line = json!({
                    "index": raw.index,
                    "offset": raw.offset,
                    "error": error.to_string(),
                    "stops_file": error.stops_file(),
                });
                serde_json::to_writer(&mut undecodable, &line)?;
                undecodable.write_all(b"\n")?;
                if error.stops_file() {
                    break;
                }
            }
        }
    }
    decoded.flush()?;
    undecodable.flush()?;
    Ok(())
}

/// The loader's counters for one file. Counts only.
fn print_counts(file: &str, report: &LoadReport) {
    println!(
        "{file}: {} bytes, {} records",
        report.file_len,
        report.framed()
    );
    println!("  decoded: {}", report.decoded);
    println!("  undecodable: {}", report.undecodable);
    println!(
        "  records with leftover bytes: {}",
        report.unread_bytes.records
    );
    for (unread, count) in &report.unread_histogram {
        println!(
            "    chunk {}: {} bytes in {count} records",
            unread.chunk, unread.bytes
        );
    }
    println!(
        "  records with extra chunks: {} ({} chunks)",
        report.extra_chunks.records, report.extra_chunk_count
    );
    println!("  legacy single-chunk records: {}", report.legacy.records);
    println!(
        "  records with a short chunk: {}",
        report.short_chunks.records
    );
    println!("  repeated object ids: {}", report.duplicate_oids.records);
    println!("  stray bytes at the end: {}", report.stray_bytes);
    println!(
        "  stopped early: {}",
        if report.stopped.is_some() {
            "yes"
        } else {
            "no"
        }
    );
}
