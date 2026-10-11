//! The record wire format against the fixtures in `tests/fixtures/wire`.
//!
//! They are 40 samples produced with Banking4, with every value and cursor
//! position (`MANIFEST.json`), and 579 reader cases, nearly all on input
//! Banking4 never writes, with their expected results (`reader-cases.tsv`).
//! Both are asserted in full here, together with every byte example printed
//! in docs/format/02-records-wire-format.md, and the codec is then run over
//! truncated and mutated input to show it cannot panic.
//!
//! Where `b4` deliberately differs from Banking4 (02 §8) the difference is
//! spelled out in [`DEVIATIONS`] rather than hidden in the comparison.

// Test helpers outside `#[test]` functions unwrap freely: a failure is a
// broken fixture, and the panic message names it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};

use b4::model::base::{
    load_collection, load_collection_strict, DecodeError, Record, RecordBase, Stop, StrictError,
    Tally,
};
use b4::wire::{
    records, text_lines, ChunkProblem, Date, FramingError, RawDate, Reader, Stamp, Timestamp,
    ValueProblem, WireError,
};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

/// Field name handed to the fallible reads; the tests do not look at it.
const FIELD: &str = "field";

// ---------------------------------------------------------------- fixtures

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read_fixture(relative: &str) -> Vec<u8> {
    let path = fixtures().join(relative);
    fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn unhex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    assert!(
        digits.len() % 2 == 0,
        "odd number of hex digits in {text:?}"
    );
    digits
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        write!(out, "{byte:02x}").unwrap();
        out
    })
}

/// SHA-256, to check the fixture files against the digests in the manifest
/// without a dependency.
fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend((data.len() as u64 * 8).to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0_u32; 64];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes(word.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    state.iter().fold(String::new(), |mut out, word| {
        write!(out, "{word:08x}").unwrap();
        out
    })
}

#[test]
fn sha256_helper_matches_the_standard_test_vectors() {
    assert_eq!(
        sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256(&[b'a'; 1000]),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
    );
}

// ---------------------------------------------------------------- manifest

#[derive(Deserialize)]
struct Manifest {
    samples: Vec<Sample>,
    stamps: Vec<StampCase>,
    times_to_stamps: Vec<TimeToStamp>,
    raw_date_to_timestamp: Vec<RawDateCase>,
    text_lines: Vec<TextLinesCase>,
}

#[derive(Deserialize)]
struct Sample {
    name: String,
    file: String,
    framing: String,
    size: usize,
    sha256: String,
    hex: Option<String>,
    records: Vec<SampleRecord>,
}

#[derive(Deserialize)]
struct SampleRecord {
    #[serde(default)]
    declined: bool,
    ops: Vec<Op>,
}

/// One primitive of a sample. The value handed to the writer (`write`) is not
/// read: for one sample it holds unpaired surrogates, which are not text.
#[derive(Deserialize)]
struct Op {
    op: String,
    #[serde(default)]
    width: usize,
    #[serde(default)]
    read: Value,
    #[serde(default)]
    end: usize,
}

#[derive(Deserialize)]
struct StampCase {
    stamp: i32,
    time: String,
    ticks: i64,
    stamp_of_time: i32,
}

#[derive(Deserialize)]
struct TimeToStamp {
    time: String,
    stamp: i32,
    container_stamp: i32,
}

#[derive(Deserialize)]
struct RawDateCase {
    year: i16,
    month: u8,
    day: u8,
    is_null: bool,
    timestamp: String,
}

#[derive(Deserialize)]
struct TextLinesCase {
    record_hex: String,
    string_read: Option<String>,
    lines_read: Vec<String>,
}

fn manifest() -> Manifest {
    serde_json::from_slice(&read_fixture("wire/MANIFEST.json")).unwrap()
}

/// A manifest string: text, null, or `{repeat, times, suffix}`.
fn expected_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Object(long) => {
            let times = usize::try_from(long["times"].as_u64().unwrap()).unwrap();
            let mut text = long["repeat"].as_str().unwrap().repeat(times);
            text.push_str(long["suffix"].as_str().unwrap());
            Some(text)
        }
        other => panic!("unexpected string notation {other}"),
    }
}

/// A manifest blob: hex, null, or `{pattern: index_mod_251, length}`.
fn expected_blob(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(unhex(text)),
        Value::Object(long) => {
            assert_eq!(long["pattern"], "index_mod_251");
            let length = usize::try_from(long["length"].as_u64().unwrap()).unwrap();
            Some(
                (0..length)
                    .map(|i| u8::try_from(i % 251).unwrap())
                    .collect(),
            )
        }
        other => panic!("unexpected blob notation {other}"),
    }
}

/// A manifest timestamp, `{ticks, iso, marked_as}` or null, against a value
/// that is either a timestamp or the midnight of a date.
fn assert_timestamp(got: Option<Timestamp>, expected: &Value, context: &str) {
    match expected {
        Value::Null => assert_eq!(got, None, "{context}"),
        expected => {
            let got = got.unwrap_or_else(|| panic!("{context}: no value"));
            assert_eq!(
                got.ticks(),
                expected["ticks"].as_i64().unwrap(),
                "{context}"
            );
            assert_eq!(got.to_iso(), expected["iso"].as_str().unwrap(), "{context}");
        }
    }
}

/// Read one primitive of a sample and compare it with the sample's `read`
/// value.
fn assert_op(r: &mut Reader<'_>, op: &Op, context: &str) {
    let read = &op.read;
    match op.op.as_str() {
        "u8" => assert_eq!(u64::from(r.u8()), read.as_u64().unwrap(), "{context}"),
        "bool" => assert_eq!(r.bool(), read.as_bool().unwrap(), "{context}"),
        "tristate" => assert_eq!(r.tristate(), read.as_bool(), "{context}"),
        "u16" => assert_eq!(u64::from(r.u16()), read.as_u64().unwrap(), "{context}"),
        "i32" => assert_eq!(i64::from(r.i32()), read.as_i64().unwrap(), "{context}"),
        "i64" => assert_eq!(r.i64(), read.as_i64().unwrap(), "{context}"),
        "decimal" => {
            let got = r.decimal(FIELD).unwrap();
            let magnitude: u128 = read["magnitude"].as_str().unwrap().parse().unwrap();
            assert_eq!(got.mantissa().unsigned_abs(), magnitude, "{context}");
            assert_eq!(
                u64::from(got.scale()),
                read["scale"].as_u64().unwrap(),
                "{context}"
            );
            // `Decimal` has no negative zero; Banking4 prints it unsigned too.
            let negative = read["negative"].as_bool().unwrap() && magnitude != 0;
            assert_eq!(got.is_sign_negative(), negative, "{context}");
            assert_eq!(got.to_string(), read["text"].as_str().unwrap(), "{context}");
        }
        "string" => assert_eq!(r.string(FIELD).unwrap(), expected_string(read), "{context}"),
        "blob" => assert_eq!(r.blob(FIELD).unwrap(), expected_blob(read), "{context}"),
        "code" => assert_eq!(r.code(op.width), expected_string(read), "{context}"),
        "guid" => assert_eq!(r.guid().to_string(), read.as_str().unwrap(), "{context}"),
        "date_raw" => {
            let expected = (!read.is_null()).then(|| RawDate {
                year: i16::try_from(read["year"].as_i64().unwrap()).unwrap(),
                month: u8::try_from(read["month"].as_u64().unwrap()).unwrap(),
                day: u8::try_from(read["day"].as_u64().unwrap()).unwrap(),
            });
            assert_eq!(r.date_raw(), expected, "{context}");
        }
        "date_clamped" => {
            assert_timestamp(r.date_clamped().map(Date::at_midnight), read, context);
        }
        "date_strict" => {
            let got = r.date_strict(FIELD).unwrap();
            assert_timestamp(got.map(Date::at_midnight), read, context);
        }
        "ticks" => assert_timestamp(r.ticks(FIELD).unwrap(), read, context),
        "time_hms" => {
            let got = r.time_hms(FIELD).unwrap().map(|time| time.to_string());
            assert_eq!(got.as_deref(), read.as_str(), "{context}");
        }
        "next_chunk" => assert_eq!(r.next_chunk(), Ok(true), "{context}"),
        other => panic!("{context}: unknown operation {other}"),
    }
    assert_eq!(r.cursor(), op.end, "{context}: cursor");
}

/// Decode one record Banking4 wrote and require that it is consumed exactly.
fn assert_record(body: &[u8], ops: &[Op], context: &str) -> usize {
    let mut r = Reader::new(body).unwrap_or_else(|error| panic!("{context}: {error}"));
    for (i, op) in ops.iter().enumerate() {
        assert_op(&mut r, op, &format!("{context} op {i} ({})", op.op));
    }
    let stats = r
        .finish()
        .unwrap_or_else(|error| panic!("{context}: {error}"));
    assert!(stats.is_exact(), "{context}: {stats:?}");
    assert_eq!(stats.short_reads, 0, "{context}");
    ops.len()
}

#[test]
fn every_writer_sample_decodes_to_banking4s_values_and_cursors() {
    let manifest = manifest();
    assert_eq!(manifest.samples.len(), 40);
    let mut operations = 0;
    for sample in &manifest.samples {
        let name = &sample.name;
        let data = read_fixture(&format!("wire/{}", sample.file));
        assert_eq!(data.len(), sample.size, "{name}");
        assert_eq!(sha256(&data), sample.sha256, "{name}");
        if let Some(text) = &sample.hex {
            assert_eq!(unhex(text), data, "{name}");
        }
        let written: Vec<&SampleRecord> = sample
            .records
            .iter()
            .filter(|record| !record.declined)
            .collect();
        match sample.framing.as_str() {
            "bare" => {
                assert_eq!(written.len(), 1, "{name}");
                operations += assert_record(&data, &written[0].ops, name);
            }
            "collection" => {
                let mut framing = records(&data);
                let mut expected = written.iter();
                for raw in framing.by_ref() {
                    let raw = raw.unwrap_or_else(|error| panic!("{name}: {error}"));
                    let record = expected
                        .next()
                        .unwrap_or_else(|| panic!("{name}: too many"));
                    let context = format!("{name} record {}", raw.index);
                    operations += assert_record(raw.body, &record.ops, &context);
                }
                assert!(expected.next().is_none(), "{name}: records missing");
                assert_eq!(framing.stray_bytes(), 0, "{name}");
            }
            other => panic!("{name}: unknown framing {other}"),
        }
    }
    // 257 values and 9 chunk changes (02 §10.1).
    assert_eq!(operations, 266);
}

#[test]
fn a_malformed_chunk_the_decoder_never_visits_stops_the_file() {
    // 02 §2.3, §8 rule 2 and the last row of "Coping with other versions":
    // the first record announces a second chunk whose size, 3, is less than
    // a chunk header. The base decoder reads the first chunk only, so it
    // never opens that chunk. It is a corrupt record all the same and not a
    // chunk from a newer program version to be skipped (02 §8 rule 6).
    let file = unhex(
        "10 00 00 00
           0c 00 00 80  01 00 00 00  00 00 00 00
           03 00 00 00
         0c 00 00 00
           0c 00 00 00  02 00 00 00  00 00 00 00",
    );
    let collection = load_collection::<RecordBase>(&file);
    assert_eq!(collection.records, []);
    let report = &collection.report;
    match report.stopped {
        Some(Stop::Record(record)) => {
            assert_eq!((record.index, record.offset), (0, 0));
            assert_eq!(
                record.error,
                DecodeError::Wire(WireError::Chunk {
                    chunk: 2,
                    offset: 12,
                    problem: ChunkProblem::SizeTooSmall
                })
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(report.extra_chunks.is_zero());
    assert_eq!(report.extra_chunk_count, 0);
    assert!(matches!(
        load_collection_strict::<RecordBase>(&file),
        Err(StrictError::Stopped(_))
    ));

    // The reader says so itself when the record is closed.
    let mut r = Reader::new(&file[4..20]).unwrap();
    assert_eq!(r.i32(), 1);
    assert_eq!(
        r.finish(),
        Err(WireError::Chunk {
            chunk: 2,
            offset: 12,
            problem: ChunkProblem::SizeTooSmall
        })
    );

    // With a second chunk that is one, the same file reads to its end and
    // the chunk is counted as one this version does not read.
    let mut sound = file.clone();
    sound[16] = 4;
    let collection = load_collection::<RecordBase>(&sound);
    assert_eq!(collection.records.len(), 2);
    assert_eq!(collection.report.stopped, None);
    assert_eq!(collection.report.extra_chunk_count, 1);
}

#[test]
fn collection_samples_load_through_the_loader() {
    let empty = load_collection_strict::<RecordBase>(&read_fixture("wire/collection_empty.bin"));
    assert_eq!(empty.unwrap().records, []);

    // Five records of four different shapes: the base decoder reads the two
    // leading stamps where a record has them and zeros where it is shorter.
    let collection = load_collection::<RecordBase>(&read_fixture("wire/collection.bin"));
    let report = &collection.report;
    assert_eq!(collection.records.len(), 5);
    assert_eq!(
        (report.decoded, report.undecodable, report.stopped),
        (5, 0, None)
    );
    assert_eq!(
        collection.records[0],
        RecordBase {
            oid: 762_529_530,
            update_stamp: 762_529_531
        }
    );
    assert_eq!(
        collection.records[0].created().to_iso(),
        "2024-02-29T13:45:30Z"
    );
    assert_eq!(
        collection.records[1],
        RecordBase {
            oid: 0,
            update_stamp: 0
        }
    );
    assert_eq!(
        collection.records[2],
        RecordBase {
            oid: 762_529_532,
            update_stamp: -1
        }
    );
    assert_eq!(collection.records[2].updated(), None);
    assert_eq!(
        report.unread_bytes,
        Tally {
            records: 4,
            first: Some(0)
        }
    );
    assert_eq!(
        report.extra_chunks,
        Tally {
            records: 2,
            first: Some(2)
        }
    );
    assert_eq!(
        report.short_chunks,
        Tally {
            records: 3,
            first: Some(1)
        }
    );
    assert_eq!(
        report.duplicate_oids,
        Tally {
            records: 2,
            first: Some(3)
        }
    );
}

#[test]
fn stamps_convert_like_banking4s() {
    let manifest = manifest();
    assert_eq!(manifest.stamps.len(), 9);
    for case in &manifest.stamps {
        let stamp = Stamp::new(case.stamp);
        assert_eq!(
            stamp.timestamp().ticks(),
            case.ticks,
            "stamp {}",
            case.stamp
        );
        assert_eq!(
            format!("{}Z", stamp.timestamp().to_iso()),
            case.time,
            "stamp {}",
            case.stamp
        );
        // The text form drops the fraction, which a stamp never has.
        assert_eq!(
            stamp.to_iso(),
            case.time.replace(".0000000Z", "Z"),
            "stamp {}",
            case.stamp
        );
        assert_eq!(stamp.unix_seconds(), stamp.timestamp().unix_seconds());
        assert_eq!(stamp.unix_micros(), stamp.timestamp().unix_micros());
        // The way back truncates to the epoch for times before 2000 (02 §5.1).
        assert_eq!(
            stamp_of_time(&case.time),
            case.stamp_of_time,
            "stamp {}",
            case.stamp
        );
        assert_eq!(case.stamp_of_time, case.stamp.max(0));
    }
    assert_eq!(manifest.times_to_stamps.len(), 9);
    for case in &manifest.times_to_stamps {
        assert_eq!(stamp_of_time(&case.time), case.stamp, "{}", case.time);
        assert_eq!(case.container_stamp, case.stamp, "{}", case.time);
    }
}

/// Banking4's time-to-stamp conversion for a UTC time in the manifest's
/// notation: whole seconds since 2000, zero before that (02 §5.1). `b4` never
/// writes stamps; this exists to check the calendar arithmetic both ways.
fn stamp_of_time(text: &str) -> i32 {
    let number = |range: std::ops::Range<usize>| text[range].parse::<i64>().unwrap();
    let date = Date::new(
        u16::try_from(number(0..4)).unwrap(),
        u8::try_from(number(5..7)).unwrap(),
        u8::try_from(number(8..10)).unwrap(),
    )
    .unwrap();
    let seconds = number(11..13) * 3600 + number(14..16) * 60 + number(17..19);
    let ticks = date.at_midnight().ticks() + seconds * 10_000_000 + number(20..27);
    assert_eq!(
        format!("{}Z", Timestamp::from_ticks(ticks).unwrap().to_iso()),
        text
    );
    let since_2000 = (ticks - Stamp::EPOCH.timestamp().ticks()).div_euclid(10_000_000);
    i32::try_from(since_2000.max(0)).unwrap()
}

#[test]
fn raw_dates_convert_like_banking4s() {
    let manifest = manifest();
    assert_eq!(manifest.raw_date_to_timestamp.len(), 14);
    for case in &manifest.raw_date_to_timestamp {
        let raw = RawDate {
            year: case.year,
            month: case.month,
            day: case.day,
        };
        assert_eq!(
            case.is_null,
            (raw.year, raw.month, raw.day) == (0, 0, 0),
            "{raw}"
        );
        assert_eq!(stamp_text(raw.timestamp()), case.timestamp, "{raw}");
        // The date is the day of that timestamp, "end of time" included.
        assert_eq!(raw.date(), raw.timestamp().map(Timestamp::date), "{raw}");
    }
}

#[test]
fn multi_line_texts_split_like_banking4s() {
    let manifest = manifest();
    assert_eq!(manifest.text_lines.len(), 13);
    for case in &manifest.text_lines {
        let body = unhex(&case.record_hex);
        let mut r = Reader::new(&body).unwrap();
        let stored = r.string(FIELD).unwrap();
        assert_eq!(stored, case.string_read, "{}", case.record_hex);
        assert_eq!(
            text_lines(stored.as_deref()),
            case.lines_read,
            "{}",
            case.record_hex
        );
        assert!(r.finish().unwrap().is_exact());
    }
}

// ------------------------------------------- the reader cases' notation

/// A string as the reader cases print it: a JSON string over UTF-16 units.
fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for unit in text.encode_utf16() {
        match u8::try_from(unit) {
            Ok(byte @ (b'"' | b'\\')) => {
                out.push('\\');
                out.push(char::from(byte));
            }
            Ok(byte @ 0x20..=0x7E) => out.push(char::from(byte)),
            _ => write!(out, "\\u{unit:04x}").unwrap(),
        }
    }
    out.push('"');
    out
}

fn stamp_text(timestamp: Option<Timestamp>) -> String {
    timestamp.map_or_else(|| "none".to_owned(), Timestamp::to_iso)
}

fn decimal_text(value: Decimal) -> String {
    let sign = if value.is_sign_negative() { "-" } else { "" };
    format!(
        "{sign}{}e-{}",
        value.mantissa().unsigned_abs(),
        value.scale()
    )
}

fn or_null<T>(value: Option<T>, text: impl FnOnce(T) -> String) -> String {
    value.map_or_else(|| "null".to_owned(), text)
}

/// The reader state as the reader cases print it:
/// `@cursor,chunk_start,chunk_end,more_chunks,consumed_size`.
fn state_text(r: &Reader<'_>) -> String {
    format!(
        "@{},{},{},{},{}",
        r.cursor(),
        r.chunk_start(),
        r.chunk_end(),
        u8::from(r.more_chunks()),
        r.consumed_size()
    )
}

/// Apply one read of a reader case and print its value in the notation of
/// the reader cases.
fn run_op(r: &mut Reader<'_>, op: &str) -> Result<String, WireError> {
    Ok(match op {
        "u8" => r.u8().to_string(),
        "bool" => r.bool().to_string(),
        "tristate" => or_null(r.tristate(), |value| value.to_string()),
        "u16" => r.u16().to_string(),
        "i32" => r.i32().to_string(),
        "i64" => r.i64().to_string(),
        "decimal" => decimal_text(r.decimal(FIELD)?),
        "string" => or_null(r.string(FIELD)?, |text| quote(&text)),
        "blob" => or_null(r.blob(FIELD)?, |bytes| format!("x{}", hex(&bytes))),
        "guid" => r.guid().to_string(),
        "date_raw" => or_null(r.date_raw(), |raw| {
            format!(
                "ymd({},{},{})=>{}",
                raw.year,
                raw.month,
                raw.day,
                stamp_text(raw.timestamp())
            )
        }),
        "date_clamped" => stamp_text(r.date_clamped().map(Date::at_midnight)),
        "date_strict" => stamp_text(r.date_strict(FIELD)?.map(Date::at_midnight)),
        "ticks" => stamp_text(r.ticks(FIELD)?),
        "time_hms" => or_null(r.time_hms(FIELD)?, |time| time.to_string()),
        "next_chunk" => r.next_chunk()?.to_string(),
        "more_chunks" => r.more_chunks().to_string(),
        "close" => "ok".to_owned(),
        code => {
            let width = code
                .strip_prefix("code:")
                .unwrap_or_else(|| panic!("operation {code}"));
            or_null(r.code(width.parse().unwrap()), |text| quote(&text))
        }
    })
}

/// How Banking4 fails where `b4` returns this error: with its
/// corrupt-stream error, or with some other failure (02 §7).
fn banking4_error(error: WireError) -> &'static str {
    match error {
        WireError::Chunk { .. }
        | WireError::Value {
            problem: ValueProblem::BlobTooLong,
            ..
        } => "ERR:corrupt",
        WireError::Value { .. } => "ERR:fail",
    }
}

/// Apply the reads of a case to an open record. Returns the steps in the
/// notation of the reader cases and whether a read failed.
fn run_ops(r: &mut Reader<'_>, ops: &[&str]) -> (String, Option<WireError>) {
    let mut steps = vec![format!("open {}", state_text(r))];
    for op in ops.iter().filter(|op| !op.is_empty()) {
        match run_op(r, op) {
            Ok(value) => steps.push(format!("{value} {}", state_text(r))),
            Err(error) => {
                steps.push(banking4_error(error).to_owned());
                return (steps.join(" | "), Some(error));
            }
        }
    }
    (steps.join(" | "), None)
}

/// A record case: open the record and apply the reads.
fn run_record(body: &[u8], ops: &[&str]) -> String {
    match Reader::new(body) {
        Ok(mut r) => run_ops(&mut r, ops).0,
        Err(error) => banking4_error(error).to_owned(),
    }
}

/// A file case with Banking4's rules: the reads are applied to every
/// record, any failure ends the file, and so does a chunk left unread
/// (02 §1.2). `b4`'s own, more tolerant loader is tested separately.
fn run_file(data: &[u8], ops: &[&str]) -> String {
    let mut parts = Vec::new();
    for raw in records(data) {
        let outcome = raw.map_err(|_| "ERR:corrupt").and_then(|raw| {
            let mut r = raw.reader().map_err(banking4_error)?;
            let (steps, failure) = run_ops(&mut r, ops);
            parts.push(steps);
            match failure {
                Some(error) => Err(banking4_error(error)),
                None if r.more_chunks() => Err("ERR:compat"),
                None => Ok(()),
            }
        });
        if let Err(error) = outcome {
            parts.push(error.to_owned());
            return parts.join(" || ");
        }
    }
    parts.push("EOF".to_owned());
    parts.join(" || ")
}

/// Make two results comparable where the documents say they may differ
/// without either being wrong:
///
/// - the number of U+FFFD a malformed UTF-8 sequence decodes to depends on
///   the decoder (02 §4.8), so runs of them are collapsed;
/// - a negative zero decimal keeps its sign in Banking4 and has none in
///   `rust_decimal`; the two are equal as numbers (02 §4.7).
fn normalise(result: &str) -> String {
    let mut text = result.to_owned();
    while text.contains("\\ufffd\\ufffd") {
        text = text.replace("\\ufffd\\ufffd", "\\ufffd");
    }
    text.split(" | ")
        .map(|step| {
            step.strip_prefix("-0e-")
                .map_or_else(|| step.to_owned(), |rest| format!("0e-{rest}"))
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

// ------------------------------------------------------------ reader cases

/// One line of `reader-cases.tsv`.
struct Case {
    id: String,
    is_file: bool,
    offset: usize,
    size: usize,
    buffer: Vec<u8>,
    ops: Vec<String>,
    expected: String,
}

impl Case {
    fn ops(&self) -> Vec<&str> {
        self.ops.iter().map(String::as_str).collect()
    }

    /// The record body of a record case.
    fn body(&self) -> &[u8] {
        &self.buffer[self.offset..self.offset + self.size]
    }

    fn run(&self) -> String {
        if self.is_file {
            run_file(&self.buffer, &self.ops())
        } else {
            run_record(self.body(), &self.ops())
        }
    }
}

fn reader_cases() -> Vec<Case> {
    let text = String::from_utf8(read_fixture("wire/reader-cases.tsv")).unwrap();
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let columns: Vec<&str> = line.split('\t').collect();
            let [id, kind, offset, size, buffer, ops, expected] = columns[..] else {
                panic!("malformed line: {line}");
            };
            Case {
                id: id.to_owned(),
                is_file: match kind {
                    "file" => true,
                    "record" => false,
                    other => panic!("{id}: unknown kind {other}"),
                },
                offset: offset.parse().unwrap(),
                size: size.parse().unwrap(),
                buffer: unhex(buffer),
                ops: ops.split(',').map(str::to_owned).collect(),
                expected: expected.to_owned(),
            }
        })
        .collect()
}

/// The reader cases in which `b4` does not do what Banking4 does, with what
/// it does instead. Everything else must match the expected results exactly.
///
/// All five are the two defects of Banking4's length handling that 02 §8
/// rule 7 tells a reader not to reproduce: a negative long-form string length
/// moves Banking4's cursor backwards, and a length near 2 GiB wraps its
/// 32-bit bounds check.
const DEVIATIONS: [(&str, &str); 5] = [
    // Negative length: an error, where Banking4 returns null and re-reads
    // earlier bytes.
    ("crafted/string_negative_1/next", "open @4,0,12,1,12 | ERR:fail"),
    ("crafted/string_negative_6/next", "open @4,0,14,1,14 | 23130 @6,0,14,1,14 | ERR:fail"),
    // The most negative length: an error at once, where Banking4's cursor
    // wraps and the next read fails.
    ("crafted/string_negative_min/next", "open @4,0,12,1,12 | ERR:fail"),
    // 0x7FFFFFFF bytes: over-long like any other string, so null, the chunk
    // poisoned and the next chunk read normally. Banking4's cursor wraps.
    (
        "crafted/string_huge/next",
        "open @4,0,12,1,12 | null @24,0,12,1,12 | 0 @24,0,12,1,12 | true @16,12,24,0,24 | 238 @17,12,24,0,24",
    ),
    // The same length on a blob: the over-long error, where Banking4 fails
    // with a different one.
    ("crafted/blob_huge/next", "open @4,0,12,1,12 | ERR:corrupt"),
];

#[test]
fn every_reader_case_matches_its_expected_result_or_a_listed_deviation() {
    let cases = reader_cases();
    assert_eq!(cases.len(), 579);
    let deviations: BTreeMap<&str, &str> = DEVIATIONS.into_iter().collect();
    let mut deviating = 0;
    let mut mismatches = Vec::new();
    for case in &cases {
        let got = normalise(&case.run());
        let theirs = normalise(&case.expected);
        let expected = match deviations.get(case.id.as_str()) {
            Some(own) => {
                assert_ne!(*own, theirs, "{}: listed as a deviation but equal", case.id);
                deviating += 1;
                (*own).to_owned()
            }
            None => theirs,
        };
        if got != expected {
            mismatches.push(format!(
                "{}\n  expected {expected}\n  got      {got}",
                case.id
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    assert_eq!(
        deviating,
        DEVIATIONS.len(),
        "a listed deviation is not among the cases"
    );
}

/// The bytes of the file case `id`.
fn file_case(cases: &[Case], id: &str) -> Vec<u8> {
    let case = cases
        .iter()
        .find(|case| case.id == id)
        .unwrap_or_else(|| panic!("no case {id}"));
    assert!(case.is_file, "{id}");
    case.buffer.clone()
}

/// Object ids for the probe records, which have none on the wire: each
/// decoded record gets a fresh one, so that no file seems to repeat an id.
static NEXT_PROBE_OID: AtomicI32 = AtomicI32::new(1);

/// Declare a record type that applies a fixed list of reads, for running the
/// file cases through the loader.
macro_rules! probe_record {
    ($name:ident, |$r:ident| $body:block) => {
        #[derive(Debug, PartialEq)]
        struct $name(i32);

        impl Record for $name {
            const NAME: &'static str = "probe";

            fn decode($r: &mut Reader<'_>) -> Result<Self, DecodeError> {
                $body
                Ok(Self(NEXT_PROBE_OID.fetch_add(1, Ordering::Relaxed)))
            }

            fn oid(&self) -> i32 {
                self.0
            }
        }
    };
}

probe_record!(ProbeU8, |r| {
    r.u8();
});
probe_record!(ProbeU8NextU8U8, |r| {
    r.u8();
    r.next_chunk()?;
    r.u8();
    r.u8();
});
probe_record!(ProbeU8NextU8, |r| {
    r.u8();
    r.next_chunk()?;
    r.u8();
});
probe_record!(ProbeBlob, |r| {
    r.blob("data")?;
});
probe_record!(ProbeStringU8, |r| {
    r.string("text")?;
    r.u8();
});
probe_record!(ProbeDecimal, |r| {
    r.decimal("amount")?;
});

/// The file cases through `b4`'s loader, which follows the tolerance table of
/// docs/format/README.md and not Banking4's stop-at-anything rule.
#[test]
fn loader_applies_the_tolerance_table_to_the_file_cases() {
    let cases = reader_cases();
    let file = |id: &str| file_case(&cases, id);
    let size_error = Some(Stop::Framing(FramingError::SizeOutOfBounds {
        index: 1,
        offset: 9,
    }));

    // Well-formed files, and stray bytes at the end.
    let report = load_collection_strict::<ProbeU8>(&file("file/empty"))
        .unwrap()
        .report;
    assert_eq!((report.decoded, report.file_len), (0, 0));
    let collection = load_collection_strict::<ProbeU8NextU8U8>(&file("file/two_records")).unwrap();
    assert_eq!(collection.report.decoded, 2);
    // The first record has one chunk: the advance is a no-op and the two
    // reads after it find nothing.
    assert_eq!(
        collection.report.short_chunks,
        Tally {
            records: 1,
            first: Some(0)
        }
    );
    for (id, stray) in [
        ("file/stray_1", 1),
        ("file/stray_2", 2),
        ("file/stray_3", 3),
    ] {
        let report = load_collection::<ProbeU8NextU8U8>(&file(id)).report;
        assert_eq!(
            (report.decoded, report.stray_bytes, report.stopped),
            (2, stray, None),
            "{id}"
        );
        assert!(report.is_clean() && report.warnings(id).is_empty(), "{id}");
        assert_eq!(
            load_collection_strict::<ProbeU8NextU8U8>(&file(id)),
            Err(StrictError::StrayBytes(stray)),
            "{id}"
        );
    }
    let report = load_collection::<ProbeU8>(&file("file/only_stray_3")).report;
    assert_eq!(
        (report.decoded, report.stray_bytes, report.stopped),
        (0, 3, None)
    );

    // A record size out of bounds or a missing body stops the file; the
    // record before it is kept and the intact record after it is not sought.
    for id in [
        "file/size_0",
        "file/size_3",
        "file/size_negative",
        "file/size_above_limit",
    ] {
        let report = load_collection::<ProbeU8>(&file(id)).report;
        assert_eq!((report.decoded, report.stopped), (1, size_error), "{id}");
        assert_eq!(
            report.warnings("Payees"),
            [
                "Payees: reading stopped at record 1 at offset 9: record size out of bounds; \
              kept the 1 probe record before it"
            ],
            "{id}"
        );
    }
    for (id, available) in [("file/size_without_body", 0), ("file/body_truncated", 4)] {
        let report = load_collection::<ProbeU8>(&file(id)).report;
        let stop = Stop::Framing(FramingError::Truncated {
            index: 1,
            offset: 9,
            available,
        });
        assert_eq!((report.decoded, report.stopped), (1, Some(stop)), "{id}");
        assert_eq!(
            load_collection_strict::<ProbeU8>(&file(id)),
            Err(StrictError::Stopped(stop)),
            "{id}"
        );
    }
    let report = load_collection::<ProbeU8>(&file("file/bad_chunk_in_second_record")).report;
    assert_eq!(report.decoded, 1);
    match report.stopped {
        Some(Stop::Record(record)) => {
            assert_eq!((record.index, record.offset), (1, 9));
            assert_eq!(
                record.error,
                DecodeError::Wire(WireError::Chunk {
                    chunk: 1,
                    offset: 0,
                    problem: ChunkProblem::PastRecordEnd
                })
            );
        }
        other => panic!("{other:?}"),
    }

    // The smallest record, and slack after the last chunk.
    let report = load_collection_strict::<ProbeU8>(&file("file/size_4"))
        .unwrap()
        .report;
    assert_eq!(report.decoded, 3);
    assert_eq!(
        report.short_chunks,
        Tally {
            records: 1,
            first: Some(1)
        }
    );
    let report = load_collection::<ProbeU8>(&file("file/slack_in_record")).report;
    assert_eq!(
        (report.decoded, report.slack),
        (
            2,
            Tally {
                records: 1,
                first: Some(0)
            }
        )
    );
    assert!(report.is_clean());

    // A chunk the decoder does not read: Banking4 rejects the file, b4
    // keeps all three records and says so (02 §8 rule 6).
    let report = load_collection::<ProbeU8>(&file("file/chunk_left_unread")).report;
    assert_eq!((report.decoded, report.stopped), (3, None));
    assert_eq!(
        report.extra_chunks,
        Tally {
            records: 1,
            first: Some(1)
        }
    );
    assert_eq!(report.extra_chunk_count, 1);
    assert_eq!(
        report.warnings("AcctsNew"),
        ["AcctsNew: chunks this version does not read in 1 of 3 probe records (1 skipped)"]
    );
    let id = "file/chunk_left_unread_after_advance";
    let report = load_collection::<ProbeU8NextU8>(&file(id)).report;
    assert_eq!((report.decoded, report.stopped), (3, None));
    assert_eq!(
        report.extra_chunks,
        Tally {
            records: 1,
            first: Some(1)
        }
    );
    assert!(matches!(
        load_collection_strict::<ProbeU8NextU8>(&file(id)),
        Err(StrictError::Record { index: 1, .. })
    ));

    // Values Banking4 fails on: that record is skipped and the
    // records after it are still read (02 §8 rule 8).
    let report = load_collection::<ProbeBlob>(&file("file/blob_too_long")).report;
    assert_eq!(
        (report.decoded, report.undecodable, report.stopped),
        (2, 1, None)
    );
    assert_eq!(report.undecodable_samples[0].index, 1);
    assert_eq!(
        report.warnings("AcctFiles"),
        [
            "AcctFiles: skipped 1 probe record that cannot be decoded (first: record 1 at offset \
             9: field `data` at byte 4 of the record: byte array longer than its chunk)",
            "AcctFiles: bytes this version does not read in 2 of 2 probe records \
             (chunk 1: 1 byte in 2 records)",
        ]
    );
    let report = load_collection::<ProbeDecimal>(&file("file/bad_decimal")).report;
    assert_eq!(
        (report.decoded, report.undecodable, report.stopped),
        (1, 1, None)
    );
    assert_eq!(report.undecodable_samples[0].index, 0);

    // An over-long string is not an error for Banking4 either.
    let report = load_collection::<ProbeStringU8>(&file("file/string_too_long")).report;
    assert_eq!((report.decoded, report.undecodable), (3, 0));
    assert_eq!(
        report.overlong_strings,
        Tally {
            records: 1,
            first: Some(1)
        }
    );
}

// ------------------------------------------------------- record base file

#[test]
fn record_base_fixture_decodes_to_banking4s_values() {
    #[derive(Deserialize)]
    struct Fixture {
        record_type: String,
        file: String,
        records: Vec<FixtureRecord>,
    }
    #[derive(Deserialize)]
    struct FixtureRecord {
        about: String,
        offset: usize,
        record_size: usize,
        read: Fields,
    }
    #[derive(Deserialize)]
    struct Fields {
        oid: i32,
        update_stamp: i32,
    }

    let fixture: Fixture =
        serde_json::from_slice(&read_fixture("records/record_base.json")).unwrap();
    assert_eq!(fixture.record_type, "record_base");
    let data = read_fixture(&format!("records/{}", fixture.file));
    assert_eq!(fixture.records.len(), 6);

    let framed: Vec<_> = records(&data).map(Result::unwrap).collect();
    let collection = load_collection_strict::<RecordBase>(&data).unwrap();
    assert_eq!(collection.records.len(), fixture.records.len());
    assert!(collection.report.is_clean());
    assert!(collection.report.short_chunks.is_zero());
    for ((expected, raw), record) in fixture.records.iter().zip(&framed).zip(&collection.records) {
        let about = &expected.about;
        assert_eq!(
            (raw.offset, raw.body.len()),
            (expected.offset, expected.record_size),
            "{about}"
        );
        assert_eq!(record.oid, expected.read.oid, "{about}");
        assert_eq!(record.update_stamp, expected.read.update_stamp, "{about}");
        assert_eq!(
            serde_json::to_value(record).unwrap(),
            serde_json::json!({"oid": expected.read.oid, "update_stamp": expected.read.update_stamp}),
            "{about}"
        );
    }
}

// ------------------------------------------------------------ 02 examples

/// One chunk around `payload` (hex), last of its record unless `more`.
fn chunk(payload: &str, more: bool) -> Vec<u8> {
    let payload = unhex(payload);
    let size = u32::try_from(payload.len() + 4).unwrap() | if more { 0x8000_0000 } else { 0 };
    let mut bytes = size.to_le_bytes().to_vec();
    bytes.extend(payload);
    bytes
}

/// The values of a result in the notation of the reader cases, without the
/// states.
fn values(result: &str) -> Vec<String> {
    result
        .split(" | ")
        .skip(1)
        .map(|step| {
            step.rsplit_once(" @")
                .map_or(step, |(value, _)| value)
                .to_owned()
        })
        .collect()
}

/// Every byte example printed in 02 §2.5 and §4, with the value the document
/// gives for it: (section, payload, reads, values).
const EXAMPLES: &[(&str, &str, &str, &[&str])] = &[
    ("2.5", "2A", "u8", &["42"]),
    ("4.1", "AB", "u8", &["171"]),
    (
        "4.2",
        "01 FF 00",
        "bool,bool,bool",
        &["true", "true", "false"],
    ),
    (
        "4.3",
        "00 01 02 07",
        "tristate,tristate,tristate,tristate",
        &["false", "true", "null", "null"],
    ),
    (
        "4.4",
        "34 12 00 80 FF FF 70 11",
        "u16,u16,u16,u16",
        &["4660", "32768", "65535", "4464"],
    ),
    (
        "4.5",
        "78 56 34 12 FE FF FF FF",
        "i32,i32",
        &["305419896", "-2"],
    ),
    (
        "4.6",
        "08 07 06 05 04 03 02 01",
        "i64",
        &["72623859790382856"],
    ),
    (
        "4.7",
        "D2 04 00 00 00 00 00 00 00 00 00 00 00 00 02 00",
        "decimal",
        &["1234e-2"],
    ),
    (
        "4.7",
        "05 00 00 00 00 00 00 00 00 00 00 00 00 00 02 80",
        "decimal",
        &["-5e-2"],
    ),
    (
        "4.7",
        "6E 00 00 00 00 00 00 00 00 00 00 00 00 00 02 00",
        "decimal",
        &["110e-2"],
    ),
    (
        "4.7",
        "00 00 00 00 01 00 00 00 00 00 00 00 00 00 00 00",
        "decimal",
        &["4294967296e-0"],
    ),
    (
        "4.7",
        "01 00 00 00 00 00 00 00 00 00 00 00 00 00 1C 00",
        "decimal",
        &["1e-28"],
    ),
    ("4.8", "00 00", "string", &["null"]),
    (
        "4.8",
        "07 00 47 72 C3 BC C3 9F 65",
        "string",
        &["\"Gr\\u00fc\\u00dfe\""],
    ),
    ("4.8", "03 00 E2 82 AC", "string", &["\"\\u20ac\""]),
    ("4.8", "FF FF 02 00 00 00 41 42", "string", &["\"AB\""]),
    ("4.8", "FF FF 00 00 00 00", "string", &["null"]),
    ("4.8", "02 00 C3 28", "string", &["\"\\ufffd(\""]),
    ("4.8", "04 00 EF BB BF 41", "string", &["\"\\ufeffA\""]),
    (
        "4.8",
        "02 00 C3 BC 02 00 C3 9F",
        "string,string",
        &["\"\\u00fc\"", "\"\\u00df\""],
    ),
    ("4.9", "45 55 52", "code:3", &["\"EUR\""]),
    ("4.9", "44 45 00", "code:3", &["\"DE\""]),
    ("4.9", "00 00", "code:2", &["null"]),
    ("4.9", "20 20 20", "code:3", &["null"]),
    ("4.9", "20 41 20", "code:3", &["\"A\""]),
    ("4.9", "41 00 42", "code:3", &["\"A\\u0000B\""]),
    ("4.9", "41 00 20", "code:3", &["\"A\\u0000\""]),
    ("4.9", "00 20 41", "code:3", &["\"\\u0000 A\""]),
    ("4.9", "41 A0", "code:2", &["\"A\""]),
    ("4.9", "C4 D6", "code:2", &["\"\\u00c4\\u00d6\""]),
    ("4.9", "45 55 52", "code:0,code:3", &["null", "\"EUR\""]),
    ("4.10", "04 00 DE AD BE EF", "blob", &["xdeadbeef"]),
    ("4.10", "00 00", "blob", &["null"]),
    (
        "4.11",
        "33 22 11 00 55 44 77 66 88 99 AA BB CC DD EE FF",
        "guid",
        &["00112233-4455-6677-8899-aabbccddeeff"],
    ),
    (
        "4.12",
        "00 00 00 00 01 00 01 01",
        "date_clamped,date_clamped",
        &["none", "none"],
    ),
    (
        "4.12",
        "00 00 00 00 01 00 01 01",
        "date_strict,date_strict",
        &["none", "none"],
    ),
    (
        "4.12 raw",
        "E8 07 02 1D 00 00 00 00 0F 27 0C 1F E8 07 00 00",
        "date_raw,date_raw,date_raw,date_raw",
        &[
            "ymd(2024,2,29)=>2024-02-29T00:00:00.0000000",
            "null",
            "ymd(9999,12,31)=>9999-12-31T23:59:59.9999999",
            "ymd(2024,0,0)=>2024-01-01T00:00:00.0000000",
        ],
    ),
    (
        "4.12 raw",
        "01 00 01 01 00 00 01 01 00 00 00 01",
        "date_raw,date_raw,date_raw",
        &["ymd(1,1,1)=>none", "ymd(0,1,1)=>none", "ymd(0,0,1)=>none"],
    ),
    (
        "4.12 clamped",
        "E8 07 02 1F E8 07 0D 05 E8 07 00 00 10 27 01 01 01 00 01 01 FF FF 01 01",
        "date_clamped,date_clamped,date_clamped,date_clamped,date_clamped,date_clamped",
        &[
            "2024-02-29T00:00:00.0000000",
            "2024-12-05T00:00:00.0000000",
            "none",
            "9999-01-01T00:00:00.0000000",
            "none",
            "9999-01-01T00:00:00.0000000",
        ],
    ),
    (
        "4.12 strict",
        "E7 07 0B 05 E7 07 00 05",
        "date_strict,date_strict",
        &["2023-11-05T00:00:00.0000000", "none"],
    ),
    (
        "4.13",
        "00 39 8E B1 2C 39 DC 08",
        "ticks",
        &["2024-02-29T13:45:30.0000000"],
    ),
    (
        "4.13",
        "FF 3F 37 F4 75 28 CA 2B",
        "ticks",
        &["9999-12-31T23:59:59.9999999"],
    ),
    ("4.13", "00 00 00 00 00 00 00 00", "ticks", &["none"]),
    (
        "4.14",
        "0D 2D 1E FF FF FF 00 00 00",
        "time_hms,time_hms,time_hms",
        &["13:45:30", "null", "00:00:00"],
    ),
    ("4.14", "FF 00 00", "time_hms", &["null"]),
    (
        "5.1",
        "FA 46 73 2D FF FF FF FF",
        "i32,i32",
        &["762529530", "-1"],
    ),
    ("5.2", "00 00", "string", &["null"]),
    ("5.2", "01 00 0A", "string", &["\"\\u000a\""]),
];

#[test]
fn every_byte_example_of_the_format_document_decodes_as_printed() {
    for (section, payload, ops, expected) in EXAMPLES {
        let body = chunk(payload, false);
        let ops: Vec<&str> = ops.split(',').collect();
        let result = run_record(&body, &ops);
        assert_eq!(values(&result), *expected, "02 §{section}: {payload}");
        // Each example is consumed to its last byte.
        let mut r = Reader::new(&body).unwrap();
        let _ = run_ops(&mut r, &ops);
        assert_eq!(r.remaining(), 0, "02 §{section}: {payload}");
    }
}

#[test]
fn framing_and_chunk_examples_of_the_format_document() {
    // 02 §1.2: a file with two records.
    let file = unhex("05000000 0500000010 0B000000 0500008012 060000000201");
    assert_eq!(
        run_file(&file, &["u8", "next_chunk", "u8", "u8"]),
        "open @4,0,5,0,5 | 16 @5,0,5,0,5 | false @5,0,5,0,5 | 0 @5,0,5,0,5 | 0 @5,0,5,0,5 || \
         open @4,0,5,1,5 | 18 @5,0,5,1,5 | true @9,5,11,0,11 | 2 @10,5,11,0,11 | 1 @11,5,11,0,11 \
         || EOF"
    );
    // 02 §2.5: one chunk, two chunks, and the record without fields.
    assert_eq!(values(&run_record(&unhex("05000000 2A"), &["u8"])), ["42"]);
    assert_eq!(
        values(&run_record(
            &unhex("05000080 01 05000000 02"),
            &["u8", "next_chunk", "u8"]
        )),
        ["1", "true", "2"]
    );
    assert_eq!(
        run_record(&unhex("04000000"), &["u8"]),
        "open @4,0,4,0,4 | 0 @4,0,4,0,4"
    );
    // 02 §2.1: a header with only bit 30 set is a chunk of size 0.
    assert_eq!(run_record(&unhex("00000040"), &["u8"]), "ERR:corrupt");

    // 02 §3: a string claiming 5 bytes with 3 left, in the first of two
    // chunks: null at 21, 0 at 21, the advance to 13, 0xEE at 14.
    let record = unhex("09000080 0500 4142 5A 0C000000 EEEEEEEEEEEEEEEE");
    assert_eq!(
        run_record(&record, &["string", "u8", "next_chunk", "u8"]),
        "open @4,0,9,1,9 | null @21,0,9,1,9 | 0 @21,0,9,1,9 | true @13,9,21,0,21 | 238 @14,9,21,0,21"
    );

    // 02 §3: date_strict in a short chunk advances over the parts that fit.
    for (payload, cursor) in [("", 4), ("E8", 5), ("E8 07", 6), ("E8 07 02", 7)] {
        let body = chunk(payload, false);
        let mut r = Reader::new(&body).unwrap();
        assert_eq!(r.date_strict(FIELD), Ok(None), "{payload}");
        assert_eq!(r.cursor(), cursor, "{payload}");
    }

    // 02 §4.8: the long form starts at 65535 bytes; `FF FF FF FF 00 00 61 ...`.
    let mut payload = unhex("FF FF FF FF 00 00");
    payload.extend([0x61; 65_535]);
    let body = chunk(&hex(&payload), false);
    let mut r = Reader::new(&body).unwrap();
    assert_eq!(r.string(FIELD).unwrap(), Some("a".repeat(65_535)));
    assert_eq!(r.remaining(), 0);

    // 02 §4.8: Banking4 can read `E0 80 80` as two U+FFFD, Rust reads
    // three; either way nothing but replacement characters.
    let body = chunk("03 00 E0 80 80", false);
    let text = Reader::new(&body).unwrap().string(FIELD).unwrap().unwrap();
    assert!(
        !text.is_empty() && text.chars().all(|c| c == '\u{FFFD}'),
        "{text:?}"
    );

    // 02 §4.4: the two signed fields see the same bytes as a negative number.
    let body = chunk("FF FF 00 80", false);
    let mut r = Reader::new(&body).unwrap();
    assert_eq!((r.u16_signed(), r.u16_signed()), (-1, -32_768));

    // 02 §4.7: the scale survives, and 1.10 equals 1.1 all the same.
    let body = chunk(
        "6E 00 00 00 00 00 00 00 00 00 00 00 00 00 02 00 \
         0B 00 00 00 00 00 00 00 00 00 00 00 00 00 01 00",
        false,
    );
    let mut r = Reader::new(&body).unwrap();
    let (a, b) = (r.decimal(FIELD).unwrap(), r.decimal(FIELD).unwrap());
    assert_eq!(
        (a.to_string().as_str(), b.to_string().as_str(), a == b),
        ("1.10", "1.1", true)
    );

    // 02 §5.1: `FA 46 73 2D` is 2024-02-29T13:45:30Z, and -1 is one second
    // before the epoch.
    assert_eq!(Stamp::new(762_529_530).to_iso(), "2024-02-29T13:45:30Z");
    assert_eq!(Stamp::new(-1).to_iso(), "1999-12-31T23:59:59Z");
    assert_eq!(Stamp::new(i32::MAX).to_iso(), "2068-01-19T03:14:07Z");

    // 02 §5.2: two empty lines are stored as one LF and read back as no lines.
    assert_eq!(text_lines(Some("\n")), Vec::<&str>::new());
}

// -------------------------------------------------------------- robustness

/// A small deterministic generator (xorshift64*), so that a failure can be
/// reproduced from the seed in the test.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number below `bound`, which must not be zero.
    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next_u64() % u64::try_from(bound).unwrap()).unwrap()
    }
}

/// The invariants every reader state must satisfy, whatever the input.
fn assert_reader_invariants(r: &Reader<'_>, context: &str) {
    assert!(r.chunk_start() <= r.chunk_end(), "{context}: {r:?}");
    assert!(r.chunk_end() <= r.record_len(), "{context}: {r:?}");
    assert!(r.cursor() <= r.record_len(), "{context}: {r:?}");
    assert!(r.cursor() >= r.chunk_start() + 4, "{context}: {r:?}");
    assert!(r.consumed_size() <= r.record_len(), "{context}: {r:?}");
    assert!(r.remaining() <= r.record_len(), "{context}: {r:?}");
}

/// Apply reads to a record body of any content. Must not panic.
fn exercise_record(body: &[u8], ops: &[&str], context: &str) {
    let Ok(mut r) = Reader::new(body) else {
        return;
    };
    assert_reader_invariants(&r, context);
    for op in ops {
        if run_op(&mut r, op).is_err() {
            break;
        }
        assert_reader_invariants(&r, context);
    }
    // A chunk the reads never got to can be broken: that is an error of the
    // chunk structure, never a panic.
    match r.finish() {
        Ok(stats) => {
            assert!(stats.chunks_visited >= 1, "{context}");
            assert!(stats.slack_bytes <= body.len(), "{context}");
        }
        Err(error) => assert!(error.stops_file(), "{context}: {error}"),
    }
}

/// A record that reads one of everything, for running arbitrary bytes
/// through the loader.
#[derive(Debug)]
struct OneOfEverything {
    base: RecordBase,
}

impl Record for OneOfEverything {
    const NAME: &'static str = "everything";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let base = RecordBase::read(r);
        r.u8();
        r.bool();
        r.code(3);
        r.string("text")?;
        r.decimal("amount")?;
        r.date_raw();
        r.date_clamped();
        if r.more_chunks() {
            r.skip_string("skipped")?;
        }
        r.next_chunk()?;
        r.date_strict("day")?;
        r.ticks("time")?;
        r.time_hms("clock")?;
        r.blob("data")?;
        r.guid();
        r.u16_signed();
        r.tristate();
        r.i64();
        let count = r.i32();
        if count > 100_000 {
            return Err(DecodeError::field("count", "too many entries"));
        }
        for _ in 0..r.list_len("count", count, 2)? {
            r.string("entry")?;
        }
        r.next_chunk()?;
        Ok(Self { base })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }

    fn is_legacy(&self) -> bool {
        self.base.update_stamp & 1 == 1
    }
}

/// Run any bytes through the framing and the loader. Must not panic, and the
/// report must add up.
fn exercise_file(data: &[u8], context: &str) {
    let mut framing = records(data);
    let mut end = 0;
    let mut framed = 0;
    for raw in framing.by_ref().flatten() {
        assert_eq!(raw.index, framed, "{context}");
        assert_eq!(raw.offset, end, "{context}");
        assert!((4..=data.len()).contains(&raw.body.len()), "{context}");
        end = raw.offset + 4 + raw.body.len();
        framed += 1;
    }
    assert!(end <= data.len() && framing.stray_bytes() <= 3, "{context}");
    assert!(framing.next().is_none(), "{context}");

    let collection = load_collection::<OneOfEverything>(data);
    let report = &collection.report;
    assert_eq!(collection.records.len(), report.decoded, "{context}");
    // Every record before a stop was decoded or skipped, and none after it
    // was looked at.
    let before_stop = report.stopped.map_or(framed, |stop| stop.index());
    assert_eq!(report.framed(), before_stop, "{context}");
    assert!(
        report.undecodable_samples.len() <= report.undecodable,
        "{context}"
    );
    assert!(report.unread_bytes.records <= report.decoded, "{context}");
    assert_eq!(report.file_len, data.len(), "{context}");
    for warning in report.warnings("File") {
        assert!(warning.starts_with("File: "), "{context}");
    }
    let exact = report.stopped.is_none()
        && report.undecodable == 0
        && report.unread_bytes.is_zero()
        && report.extra_chunks.is_zero()
        && report.overlong_strings.is_zero()
        && report.slack.is_zero()
        && report.stray_bytes == 0;
    assert_eq!(
        load_collection_strict::<OneOfEverything>(data).is_ok(),
        exact,
        "{context}"
    );
}

/// The positions and sizes of the chunks of a well-formed record body.
fn chunk_spans(body: &[u8]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut at = 0;
    loop {
        let header = u32::from_le_bytes(body[at..at + 4].try_into().unwrap());
        let size = usize::try_from(header & 0x3FFF_FFFF).unwrap();
        spans.push((at, size));
        at += size;
        if header & 0x8000_0000 == 0 {
            return spans;
        }
    }
}

/// Cut a sample record after every byte, in two ways: plainly, and with the
/// chunk header patched so that the cut looks like a record an older writer
/// ended there. Nothing may panic, and in the patched form every value that
/// lies before the cut must still decode to the sample's `read` value.
fn truncate_record(data: &[u8], ops: &[Op], name: &str) -> usize {
    let op_names: Vec<String> = ops
        .iter()
        .map(|op| {
            if op.op == "code" {
                format!("code:{}", op.width)
            } else {
                op.op.clone()
            }
        })
        .collect();
    let op_names: Vec<&str> = op_names.iter().map(String::as_str).collect();
    let spans = chunk_spans(data);
    let mut buffer = data.to_vec();
    for cut in 0..data.len() {
        let context = format!("{name} cut at {cut}");
        exercise_record(&data[..cut], &op_names, &context);

        // The chunk the cut falls into ends at the cut and becomes the last
        // one; if not even its header survives, the chunk before it does.
        let (index, &(start, _)) = spans
            .iter()
            .enumerate()
            .rfind(|&(_, &(start, _))| start <= cut)
            .unwrap();
        let (last_start, end) = if cut >= start + 4 {
            (start, cut)
        } else if index > 0 {
            (spans[index - 1].0, start)
        } else {
            continue;
        };
        let saved: [u8; 4] = buffer[last_start..last_start + 4].try_into().unwrap();
        let patched = u32::try_from(end - last_start).unwrap();
        buffer[last_start..last_start + 4].copy_from_slice(&patched.to_le_bytes());
        {
            let body = &buffer[..end];
            let mut r = Reader::new(body).unwrap_or_else(|error| panic!("{context}: {error}"));
            let mut intact = true;
            for (i, op) in ops.iter().enumerate() {
                intact = intact && op.end <= end;
                if intact {
                    assert_op(&mut r, op, &format!("{context} op {i} ({})", op.op));
                } else if run_op(&mut r, op_names[i]).is_err() {
                    break;
                }
                assert_reader_invariants(&r, &context);
            }
            exercise_file(body, &context);
        }
        buffer[last_start..last_start + 4].copy_from_slice(&saved);
    }
    data.len()
}

#[test]
fn truncating_every_sample_at_every_byte_never_panics() {
    let manifest = manifest();
    let mut cuts = 0;
    for sample in &manifest.samples {
        let data = read_fixture(&format!("wire/{}", sample.file));
        let written: Vec<&SampleRecord> = sample
            .records
            .iter()
            .filter(|record| !record.declined)
            .collect();
        if sample.framing == "bare" {
            cuts += truncate_record(&data, &written[0].ops, &sample.name);
        } else {
            for cut in 0..data.len() {
                exercise_file(&data[..cut], &format!("{} cut at {cut}", sample.name));
                // The records before the cut are unharmed.
                let whole = records(&data).map(Result::unwrap);
                for (raw, complete) in records(&data[..cut]).flatten().zip(whole) {
                    assert_eq!(raw, complete, "{} cut at {cut}", sample.name);
                }
            }
            for (raw, record) in records(&data).map(Result::unwrap).zip(&written) {
                let name = format!("{} record {}", sample.name, raw.index);
                cuts += truncate_record(raw.body, &record.ops, &name);
            }
        }
    }
    // Every byte of every sample was a cut point.
    assert!(cuts > 500_000, "{cuts}");
}

#[test]
fn truncating_every_reader_case_at_every_byte_never_panics() {
    for case in reader_cases() {
        let ops = case.ops();
        for cut in 0..=case.buffer.len() {
            let context = format!("{} cut at {cut}", case.id);
            let data = &case.buffer[..cut];
            exercise_file(data, &context);
            let _ = run_file(data, &ops);
            let body = data.get(case.offset..).unwrap_or_default();
            exercise_record(body, &ops, &context);
        }
    }
}

/// All operations the mutation run draws from.
const ALL_OPS: [&str; 22] = [
    "u8",
    "bool",
    "tristate",
    "u16",
    "i32",
    "i64",
    "decimal",
    "string",
    "blob",
    "guid",
    "code:0",
    "code:1",
    "code:3",
    "code:11",
    "date_raw",
    "date_clamped",
    "date_strict",
    "ticks",
    "time_hms",
    "next_chunk",
    "more_chunks",
    "close",
];

/// 32-bit values that sit on the edges of the format's length checks.
const EDGE_WORDS: [u32; 12] = [
    0,
    1,
    3,
    4,
    5,
    0xFFFF,
    0x3FFF_FFFF,
    0x4000_0000,
    0x4000_0001,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFF,
];

/// Damage `data` in one of several ways.
fn mutate(data: &mut Vec<u8>, rng: &mut Rng) {
    if data.is_empty() {
        data.push(u8::try_from(rng.below(256)).unwrap());
        return;
    }
    let at = rng.below(data.len());
    match rng.below(7) {
        0 => data[at] ^= 1 << rng.below(8),
        1 => data[at] = u8::try_from(rng.below(256)).unwrap(),
        2 => data.insert(at, u8::try_from(rng.below(256)).unwrap()),
        3 => {
            data.remove(at);
        }
        4 => data.truncate(at),
        5 => {
            // An edge value where a length or header might be.
            let word = EDGE_WORDS[rng.below(EDGE_WORDS.len())].to_le_bytes();
            for (slot, byte) in data[at..].iter_mut().zip(word) {
                *slot = byte;
            }
        }
        _ => {
            // The long-form marker, so that the next four bytes become a length.
            for slot in data[at..].iter_mut().take(2) {
                *slot = 0xFF;
            }
        }
    }
}

#[test]
fn mutated_input_never_panics() {
    // The seeds: every sample and reader case small enough to mutate quickly.
    let manifest = manifest();
    let mut seeds: Vec<Vec<u8>> = manifest
        .samples
        .iter()
        .filter(|sample| sample.size <= 2048)
        .map(|sample| read_fixture(&format!("wire/{}", sample.file)))
        .collect();
    seeds.extend(reader_cases().into_iter().map(|case| case.buffer));
    seeds.push(read_fixture("records/record_base.bin"));
    assert!(seeds.len() > 600);

    let mut rng = Rng(0x00B4_5EED_2026_1011);
    for round in 0..150_000 {
        let mut data = seeds[rng.below(seeds.len())].clone();
        for _ in 0..=rng.below(4) {
            mutate(&mut data, &mut rng);
        }
        let ops: Vec<&str> = (0..=rng.below(24))
            .map(|_| ALL_OPS[rng.below(ALL_OPS.len())])
            .collect();
        let context = format!("round {round}");
        exercise_record(&data, &ops, &context);
        exercise_file(&data, &context);
        // The same bytes as the only record of a file, and as a record with a
        // valid single chunk around them.
        let mut framed = i32::try_from(data.len()).unwrap().to_le_bytes().to_vec();
        framed.extend(&data);
        exercise_file(&framed, &context);
        let mut wrapped = u32::try_from(data.len() + 4)
            .unwrap()
            .to_le_bytes()
            .to_vec();
        wrapped.extend(&data);
        exercise_record(&wrapped, &ops, &context);
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0x0BAD_C0FF_EE00_B4B4);
    for round in 0..50_000 {
        let length = rng.below(96);
        let mut data: Vec<u8> = (0..length)
            .map(|_| u8::try_from(rng.below(256)).unwrap())
            .collect();
        // Half of the time a plausible first header, so that reads happen.
        if round % 2 == 0 && length >= 4 {
            let size = u32::try_from(4 + rng.below(length - 3)).unwrap();
            let flag = if rng.below(3) == 0 { 0x8000_0000 } else { 0 };
            data[..4].copy_from_slice(&(size | flag).to_le_bytes());
        }
        let ops: Vec<&str> = (0..=rng.below(32))
            .map(|_| ALL_OPS[rng.below(ALL_OPS.len())])
            .collect();
        let context = format!("round {round}");
        exercise_record(&data, &ops, &context);
        exercise_file(&data, &context);
    }
}

// ------------------------------------------------------------- real data

/// All files under `dir`, one level of directories deep, as a vault has them.
fn vault_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).unwrap().map(Result::unwrap) {
        let path = entry.path();
        if path.is_dir() {
            files.extend(
                fs::read_dir(&path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| path.is_file()),
            );
        } else {
            files.push(path);
        }
    }
    files.sort();
    files
}

/// Local smoke test on real data, skipped unless `B4_TEST_RAW_DIR` names a
/// directory holding the decrypted files of a vault (root files, and one
/// directory per account and security).
///
/// Prints counts and entry names only, never a value.
#[test]
fn real_collection_files_frame_cleanly() {
    let Some(raw_dir) = std::env::var_os("B4_TEST_RAW_DIR") else {
        return;
    };
    let mut totals: BTreeMap<String, (usize, LoadTotals)> = BTreeMap::new();
    for path in vault_files(Path::new(&raw_dir)) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let data = fs::read(&path).unwrap();
        let report = load_collection::<RecordBase>(&data).report;
        // Never the path: directory names carry object ids.
        assert_eq!(report.stopped, None, "a file named {name} does not frame");
        assert_eq!(report.undecodable, 0, "a file named {name}");
        let (files, total) = totals.entry(name).or_default();
        *files += 1;
        total.records += report.decoded;
        total.bytes += data.len();
        total.stray_files += usize::from(report.stray_bytes > 0);
        total.slack_records += report.slack.records;
        total.extra_chunk_records += report.extra_chunks.records;
        total.duplicate_oids += report.duplicate_oids.records;
    }
    println!("entry\tfiles\tbytes\trecords\tmulti-chunk\tslack\tstray\trepeated-oid");
    for (name, (files, total)) in &totals {
        println!(
            "{name}\t{files}\t{}\t{}\t{}\t{}\t{}\t{}",
            total.bytes,
            total.records,
            total.extra_chunk_records,
            total.slack_records,
            total.stray_files,
            total.duplicate_oids
        );
    }
}

/// Sums over all files of one entry name.
#[derive(Default)]
struct LoadTotals {
    records: usize,
    bytes: usize,
    stray_files: usize,
    slack_records: usize,
    extra_chunk_records: usize,
    duplicate_oids: usize,
}
