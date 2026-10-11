//! Hand-built account records for the unit tests: a small writer for the
//! wire primitives of 02 and an account whose every stored field can be set.

use rust_decimal::Decimal;

/// Marker in every bank access field of a [`Sample`]. No decoded account may
/// contain it.
pub(super) const ACCESS_MARKER: &str = "ACCESS";

/// The bytes of both contact GUIDs of a [`Sample`], and their text form.
pub(super) const ACCESS_GUID: [u8; 16] = [0xA5; 16];
pub(super) const ACCESS_GUID_TEXT: &str = "a5a5a5a5-a5a5-a5a5-a5a5-a5a5a5a5a5a5";

/// A day as stored: year, month, day.
pub(super) type Day = (u16, u8, u8);

/// Writer for the primitives of 02 §4.
#[derive(Default)]
pub(super) struct Wire(pub Vec<u8>);

impl Wire {
    pub fn u8(&mut self, value: u8) {
        self.0.push(value);
    }

    pub fn bool(&mut self, value: bool) {
        self.0.push(u8::from(value));
    }

    pub fn u16(&mut self, value: u16) {
        self.0.extend(value.to_le_bytes());
    }

    pub fn i32(&mut self, value: i32) {
        self.0.extend(value.to_le_bytes());
    }

    pub fn ticks(&mut self, value: i64) {
        self.0.extend(value.to_le_bytes());
    }

    pub fn decimal(&mut self, value: Decimal) {
        let magnitude = value.mantissa().unsigned_abs();
        let flags = (value.scale() << 16) | (u32::from(value.is_sign_negative()) << 31);
        self.0.extend(&magnitude.to_le_bytes()[..12]);
        self.0.extend(flags.to_le_bytes());
    }

    pub fn string(&mut self, value: Option<&str>) {
        let bytes = value.unwrap_or_default().as_bytes();
        self.u16(u16::try_from(bytes.len()).unwrap());
        self.0.extend(bytes);
    }

    pub fn code(&mut self, value: &str, width: usize) {
        let mut bytes = value.as_bytes().to_vec();
        bytes.resize(width, 0);
        self.0.extend(bytes);
    }

    pub fn day(&mut self, value: Option<Day>) {
        let (year, month, day) = value.unwrap_or_default();
        self.u16(year);
        self.u8(month);
        self.u8(day);
    }

    pub fn guid(&mut self, value: [u8; 16]) {
        self.0.extend(value);
    }
}

/// One chunk: header and payload (02 §2).
pub(super) fn chunk(payload: &[u8], more: bool) -> Vec<u8> {
    let size = u32::try_from(payload.len() + 4).unwrap();
    let mut bytes = (size | (u32::from(more) << 31)).to_le_bytes().to_vec();
    bytes.extend(payload);
    bytes
}

/// A collection file holding the given records (02 §1.2).
pub(super) fn file(records: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(i32::try_from(record.len()).unwrap().to_le_bytes());
        bytes.extend(record);
    }
    bytes
}

/// An account record as Banking4 lays it out, with the values a test wants
/// to vary. The defaults describe a bank account of the documented version
/// in which every bank access field is set.
#[derive(Clone)]
pub(super) struct Sample {
    pub oid: i32,
    pub kind: i32,
    pub bank_code: Option<&'static str>,
    pub currency: &'static str,
    pub version: u8,
    pub sync_flags: i32,
    pub statement_retention_days: i32,
    pub statement_balance: Decimal,
    pub online_balance: Decimal,
    pub last_download_day: Option<Day>,
    pub last_download_ticks: i64,
    pub last_sync_day: Option<Day>,
    pub last_sync_ticks: i64,
    pub statement_balance_day: Option<Day>,
    pub statement_balance_ticks: i64,
    pub online_balance_day: Option<Day>,
    pub online_balance_ticks: i64,
    pub flags: i32,
    pub opening_day: Option<Day>,
    pub sevdesk_day: Option<Day>,
    pub holder_address: Option<&'static str>,
    /// The stored count and the entries that follow it. `None` writes the
    /// presence flag as false.
    pub transfer_list: Option<(i32, Vec<Option<&'static str>>)>,
    pub position: u16,
    pub payments_retention: Option<&'static str>,
    pub files_retention: Option<&'static str>,
    pub unused_63: i64,
    /// Bytes after the last known field of chunk 1.
    pub trailing: Vec<u8>,
    pub second_chunk: bool,
    pub third_chunk: bool,
}

impl Default for Sample {
    fn default() -> Self {
        Self {
            oid: 800_000_001,
            kind: 0x01,
            bank_code: Some("99999901"),
            currency: "EUR",
            version: 26,
            sync_flags: 0x06,
            statement_retention_days: 365,
            statement_balance: Decimal::new(102_121, 2),
            online_balance: Decimal::new(-2828, 2),
            last_download_day: None,
            last_download_ticks: 0,
            last_sync_day: None,
            last_sync_ticks: 0,
            statement_balance_day: None,
            statement_balance_ticks: 0,
            online_balance_day: None,
            online_balance_ticks: 0,
            flags: 0x1200,
            opening_day: Some((2001, 2, 3)),
            sevdesk_day: Some((2023, 11, 5)),
            holder_address: Some("1 Sample Street\nSampletown"),
            transfer_list: Some((1, vec![Some("XX00TARGET")])),
            position: 3,
            payments_retention: Some("M6"),
            files_retention: Some("D90"),
            unused_63: 0,
            trailing: Vec::new(),
            second_chunk: true,
            third_chunk: false,
        }
    }
}

impl Sample {
    /// The record without its size prefix: fields 1 to 69 in the order of the
    /// table of 03 "Account".
    pub fn record(&self) -> Vec<u8> {
        let mut w = Wire::default();
        w.i32(self.oid);
        w.i32(self.oid + 1);
        w.i32(self.kind);
        w.code("DE", 2);
        w.string(self.bank_code);
        w.string(Some("1234567"));
        w.code(self.currency, 3);
        w.string(Some("Sample account"));
        w.string(Some(ACCESS_MARKER));
        w.string(Some("XX00SAMPLE"));
        w.string(Some("SAMPXX01"));
        w.string(Some("Sample Holder"));
        w.string(Some("a description"));
        w.day(self.last_download_day);
        w.bool(false);
        w.i32(self.statement_retention_days);
        w.u8(self.version);
        w.i32(self.sync_flags);
        w.day(self.last_sync_day);
        w.day(self.statement_balance_day);
        w.decimal(self.statement_balance);
        for _ in 0..4 {
            w.bool(false);
        }
        w.i32(0);
        w.day(self.online_balance_day);
        w.decimal(self.online_balance);
        w.string(Some(ACCESS_MARKER));
        w.i32(self.flags);
        w.day(self.opening_day);
        w.string(self.holder_address);
        w.decimal(Decimal::new(33, 3));
        w.decimal(Decimal::new(34, 1));
        w.decimal(Decimal::new(35, 5));
        w.decimal(Decimal::new(3600, 0));
        w.string(Some(ACCESS_MARKER));
        w.ticks(self.last_sync_ticks);
        w.string(Some("creditor"));
        w.string(Some("XX00REFERENCE"));
        w.bool(self.transfer_list.is_some());
        if let Some((count, entries)) = &self.transfer_list {
            w.i32(*count);
            for entry in entries {
                w.string(*entry);
            }
        }
        w.string(Some("originator"));
        w.string(Some(ACCESS_MARKER));
        w.string(Some("reserved"));
        w.u16(self.position);
        w.i32(800_200_001);
        w.u16(0x0101);
        w.decimal(Decimal::new(5050, 2));
        w.ticks(self.statement_balance_ticks);
        w.ticks(self.online_balance_ticks);
        w.ticks(self.last_download_ticks);
        w.u16(0x1);
        w.decimal(Decimal::new(555, 1));
        w.decimal(Decimal::new(5656, 2));
        w.decimal(Decimal::new(-57570, 3));
        w.decimal(Decimal::new(58000, 0));
        w.decimal(Decimal::new(590_059, 4));
        w.string(Some(ACCESS_MARKER));
        w.string(self.payments_retention);
        w.string(self.files_retention);
        w.ticks(self.unused_63);
        w.guid(ACCESS_GUID);
        w.guid(ACCESS_GUID);
        w.day(self.sevdesk_day);
        w.i32(0x1);
        w.string(Some("icon"));
        w.0.extend(&self.trailing);

        let mut record = chunk(&w.0, self.second_chunk);
        if self.second_chunk {
            record.extend(chunk(&[1], self.third_chunk));
        }
        if self.third_chunk {
            record.extend(chunk(&[7, 7, 7], false));
        }
        record
    }

    /// A collection file holding just this record.
    pub fn file(&self) -> Vec<u8> {
        file(&[self.record()])
    }
}
