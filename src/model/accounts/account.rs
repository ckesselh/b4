//! The account record (03 "Account").

use rust_decimal::Decimal;
use serde::Serialize;

use super::kinds::{
    AccountFlags, AccountKind, BalanceFlags, BookkeepingFlags, NotifyFlags, SyncFlags,
};
use super::retention::RetentionPeriod;
use crate::model::base::{DecodeError, Record, RecordBase};
use crate::wire::{Date, Reader, Timestamp};

/// The record version the format document describes; the documented Banking4
/// version writes it (03 "Account", field 17). A higher version is decoded as
/// far as this layout goes and counted, see [`super::load_accounts`].
pub const DOCUMENTED_RECORD_VERSION: u8 = 26;

/// Below this version the record ends after `statement_balance` and none of
/// the reader's adjustments apply (03 "Account", after field 21).
const FIRST_VERSION_WITH_EXTENSION: u8 = 2;

/// Bank account records below this version gain two sync flags on reading
/// (03 "Processing after the read", step 3).
const FIRST_VERSION_WITH_ALL_SYNC_FLAGS: u8 = 19;

/// The currency codes that turn a cash account into a crypto account
/// (03 "Processing after the read", step 4): the primary codes of the crypto
/// entries in the currency table of the documented Banking4 version. The
/// table is part of the application, not of the vault, and may differ between
/// program versions (03 open question 6); current versions store the crypto
/// kind directly, so this only matters for old records.
const CRYPTO_CURRENCIES: [&str; 4] = ["ETH", "LTC", "XBT", "XRP"];

/// One account of `AcctsNew` (or of the legacy `Accts`).
///
/// The fields hold what Banking4 has in memory after reading the record: the
/// reader changes several values while it reads (03 "Processing after the
/// read"), and those changes are applied here. `stored_account_kind` keeps
/// the one stored value that a query may want next to its adjusted form.
///
/// Seven fields of the record are bank access material: `contact_name`,
/// `order_contact_name`, `fints_account_sub_id`, `fints_customer_id`,
/// `last_download_token`, `contact_guid` and `order_contact_guid`. They are
/// read past and never stored, so they do not exist in this struct
/// (docs/format/database-mapping-notes.md, "What is left out").
///
/// The four times `last_download_time`, `last_sync_time`,
/// `statement_balance_time` and `online_balance_time` are each stored twice,
/// as a day and as a time stamp. The time stamp wins whenever it is set;
/// otherwise the field is the day at midnight. Like all `ticks` values they
/// carry no time zone (03 open question 3).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Account {
    /// Object id and update stamp. The id also names the account's directory
    /// in the vault, see [`super::account_directory_name`].
    #[serde(flatten)]
    pub base: RecordBase,
    /// What the account is, after the reader's adjustment: a cash account in
    /// a crypto currency counts as a crypto account.
    pub account_kind: AccountKind,
    /// The kind as stored: the low eight bits of the stored number.
    pub stored_account_kind: AccountKind,
    /// Version of the record layout the writer used. 0 if the record ends
    /// before the version byte.
    pub record_version: u8,
    /// ISO country of the bank.
    pub country_code: Option<String>,
    /// National bank code. Kinds without a bank carry a fixed pseudo code.
    pub bank_code: Option<String>,
    /// National account number.
    pub account_number: Option<String>,
    /// ISO currency code.
    pub currency: Option<String>,
    /// Display name.
    pub name: Option<String>,
    /// IBAN.
    pub iban: Option<String>,
    /// BIC of the bank.
    pub bic: Option<String>,
    /// Account holder.
    pub holder_name: Option<String>,
    /// Free text.
    pub description: Option<String>,
    /// When statements were last downloaded.
    pub last_download_time: Option<Timestamp>,
    /// Retention period for bookings as stored: a number of days.
    pub statement_retention_days: i32,
    /// Retention period for bookings as the application reads that number:
    /// one of the preset periods, "not set" or "unlimited".
    pub statement_retention: RetentionPeriod,
    /// What a send/receive run fetches for this account.
    pub sync_flags: SyncFlags,
    /// When the account last took part in a send/receive run.
    pub last_sync_time: Option<Timestamp>,
    /// When the booked balance was valid.
    pub statement_balance_time: Option<Timestamp>,
    /// Booked balance according to the statements. Zero for kinds without
    /// bookings.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub statement_balance: Decimal,
    /// When the bank reported `online_balance`.
    pub online_balance_time: Option<Timestamp>,
    /// Balance reported by the bank.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub online_balance: Decimal,
    /// Properties of the account, `hidden` among them.
    pub flags: AccountFlags,
    /// Date the account was opened.
    pub opening_date: Option<Date>,
    /// Street of the account holder: the first line of the stored address.
    pub holder_street: Option<String>,
    /// Town of the account holder: the rest of the stored address.
    pub holder_town: Option<String>,
    /// Interest rate on a credit balance.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub credit_interest_rate: Decimal,
    /// Interest rate on a debit balance.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub debit_interest_rate: Decimal,
    /// Interest rate beyond the overdraft limit.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub overdraft_interest_rate: Decimal,
    /// Overdraft limit.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub overdraft_limit: Decimal,
    /// SEPA creditor identifier.
    pub creditor_id: Option<String>,
    /// IBAN of a linked reference account.
    pub reference_account_iban: Option<String>,
    /// IBANs the bank permits as targets of internal transfers from this
    /// account. `None` is "no list", which differs from an empty list; an
    /// entry can be missing.
    pub internal_transfer_ibans: Option<Vec<Option<String>>>,
    /// Name to use as originator of payments.
    pub originator_name: Option<String>,
    /// A text field with no known use.
    pub reserved_text: Option<String>,
    /// Sort position among the accounts. Accounts sort by this, then by id.
    pub position: u16,
    /// Object id of the account's group, 0 for none.
    pub group_oid: i32,
    /// Where limit and available funds come from and which are shown.
    pub balance_flags: BalanceFlags,
    /// Available amount as reported or computed.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub available_funds: Decimal,
    /// Notification preferences.
    pub notify_flags: NotifyFlags,
    /// Notify about debits from this amount.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub notify_min_debit: Decimal,
    /// Notify about credits from this amount.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub notify_min_credit: Decimal,
    /// Notify when the balance falls below this amount.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub notify_min_balance: Decimal,
    /// Notify when the balance rises above this amount.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub notify_max_balance: Decimal,
    /// Spending limit for notifications.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub notify_spending_limit: Decimal,
    /// Retention period for payments. `None` if `payments_retention_text` is
    /// not a period.
    pub payments_retention: Option<RetentionPeriod>,
    /// The stored text of that period; no text means "unlimited".
    pub payments_retention_text: Option<String>,
    /// Retention period for documents. `None` if `files_retention_text` is
    /// not a period.
    pub files_retention: Option<RetentionPeriod>,
    /// The stored text of that period; no text means "unlimited".
    pub files_retention_text: Option<String>,
    /// Sync position of the bookkeeping-service link.
    pub sevdesk_last_entry_date: Option<Date>,
    /// Settings of the bookkeeping-service link.
    pub sevdesk_flags: BookkeepingFlags,
    /// Name of a user-chosen icon.
    pub icon_name: Option<String>,
}

impl Account {
    /// Whether this is the placeholder record of a migrated `Accts` file, or
    /// any other record of kind none. Loaders drop these (03 "`Accts` and
    /// `AcctsNew`").
    #[must_use]
    pub const fn is_placeholder(&self) -> bool {
        self.account_kind.raw() == 0
    }

    /// Whether the record was written in a layout newer than the one the
    /// format document describes, so that it may hold fields `b4` does not
    /// know (03 "Record version 27").
    #[must_use]
    pub const fn is_newer_version(&self) -> bool {
        self.record_version > DOCUMENTED_RECORD_VERSION
    }

    /// Whether the account is hidden in the application.
    #[must_use]
    pub const fn is_hidden(&self) -> bool {
        self.flags.contains(AccountFlags::HIDDEN)
    }

    /// The object id of the account's group, if it has one.
    #[must_use]
    pub const fn group(&self) -> Option<i32> {
        if self.group_oid == 0 {
            None
        } else {
            Some(self.group_oid)
        }
    }

    /// The reader's adjustments of 03 "Processing after the read", in the
    /// document's order. Steps that only touch bank access fields have
    /// nothing to change here, because those fields are not kept.
    fn apply_reader_adjustments(&mut self) {
        // Step 1: a kind without bookings has no booked balance.
        if !self.account_kind.has_statements() {
            self.statement_balance = Decimal::ZERO;
            self.statement_balance_time = None;
        }

        // Step 2: a cash account has no online side. (It also clears five
        // access fields.)
        if self.account_kind == AccountKind::Cash {
            self.internal_transfer_ibans = None;
            self.online_balance = Decimal::ZERO;
            self.online_balance_time = None;
            self.last_download_time = None;
            self.last_sync_time = None;
            self.sync_flags = SyncFlags::default();
        }

        // Step 3: old bank accounts fetch account info and payees as well.
        if self.record_version < FIRST_VERSION_WITH_ALL_SYNC_FLAGS
            && self.account_kind == AccountKind::Bank
        {
            self.sync_flags =
                SyncFlags::from_raw(self.sync_flags.raw() | SyncFlags::ADDED_TO_OLD_BANK_ACCOUNTS);
        }

        // Step 4: cash in a crypto currency is a crypto account. The code
        // must match exactly; what step 2 cleared stays cleared.
        if self.account_kind == AccountKind::Cash
            && self
                .currency
                .as_deref()
                .is_some_and(|currency| CRYPTO_CURRENCIES.contains(&currency))
        {
            self.account_kind = AccountKind::Crypto;
        }

        // Step 5 clears `fints_account_sub_id` for certain bank codes. That
        // is an access field: nothing to do.
    }
}

/// The time Banking4 keeps for one of the four day/time-stamp pairs: the
/// time stamp if it is set, else the day at midnight. The two are not
/// compared, so a time stamp of a single tick replaces any day (03 "Account").
fn merged_time(day: Option<Date>, time: Option<Timestamp>) -> Option<Timestamp> {
    time.or(day.map(Date::at_midnight))
}

/// Split the stored holder address into street and town (03 "Account",
/// field 32): at the first line feed, each part trimmed, an empty part
/// missing. Without a line feed the whole text is the street, untouched.
///
/// What Banking4 trims as white space is the Unicode `White_Space` set that
/// `str::trim` uses too: `09` to `0D`, the blank, `85`, `A0` and the Unicode
/// separators, but not `1C` to `1F`. A carriage return is trimmed away, never
/// a place to split.
fn split_address(address: Option<String>) -> (Option<String>, Option<String>) {
    let Some(address) = address else {
        return (None, None);
    };
    let Some((street, town)) = address.split_once('\n') else {
        return (Some(address), None);
    };
    let part = |text: &str| {
        Some(text.trim())
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    (part(street), part(town))
}

impl Record for Account {
    const NAME: &'static str = "account";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        // Fields 1 to 21 in wire order. Every record has them, or defaults.
        let base = RecordBase::read(r);
        // Only the low eight bits of the kind are kept.
        let [stored_kind, ..] = r.i32().to_le_bytes();
        let stored_account_kind = AccountKind::from_raw(stored_kind);
        let country_code = r.code(2);
        let bank_code = r.string("bank_code")?;
        let account_number = r.string("account_number")?;
        let currency = r.code(3);
        let name = r.string("name")?;
        r.skip_string("contact_name")?;
        let iban = r.string("iban")?;
        let bic = r.string("bic")?;
        let holder_name = r.string("holder_name")?;
        let description = r.string("description")?;
        let last_download_date = r.date_clamped();
        let _unused_15 = r.bool();
        let statement_retention_days = r.i32();
        let record_version = r.u8();
        let sync_flags = SyncFlags::from_raw(r.i32());
        let last_sync_date = r.date_clamped();
        let statement_balance_date = r.date_clamped();
        let statement_balance = r.decimal("statement_balance")?;

        let mut account = Self {
            base,
            account_kind: stored_account_kind,
            stored_account_kind,
            record_version,
            country_code,
            bank_code,
            account_number,
            currency,
            name,
            iban,
            bic,
            holder_name,
            description,
            last_download_time: merged_time(last_download_date, None),
            statement_retention_days,
            statement_retention: RetentionPeriod::from_days(statement_retention_days),
            sync_flags,
            last_sync_time: merged_time(last_sync_date, None),
            statement_balance_time: merged_time(statement_balance_date, None),
            statement_balance,
            online_balance_time: None,
            online_balance: Decimal::ZERO,
            flags: AccountFlags::default(),
            opening_date: None,
            holder_street: None,
            holder_town: None,
            credit_interest_rate: Decimal::ZERO,
            debit_interest_rate: Decimal::ZERO,
            overdraft_interest_rate: Decimal::ZERO,
            overdraft_limit: Decimal::ZERO,
            creditor_id: None,
            reference_account_iban: None,
            internal_transfer_ibans: None,
            originator_name: None,
            reserved_text: None,
            position: 0,
            group_oid: 0,
            balance_flags: BalanceFlags::default(),
            available_funds: Decimal::ZERO,
            notify_flags: NotifyFlags::default(),
            notify_min_debit: Decimal::ZERO,
            notify_min_credit: Decimal::ZERO,
            notify_min_balance: Decimal::ZERO,
            notify_max_balance: Decimal::ZERO,
            notify_spending_limit: Decimal::ZERO,
            // Below version 2 the two text-form periods are never assigned
            // and stay "not set" rather than "unlimited".
            payments_retention: Some(RetentionPeriod::NOT_SET),
            payments_retention_text: None,
            files_retention: Some(RetentionPeriod::NOT_SET),
            files_retention_text: None,
            sevdesk_last_entry_date: None,
            sevdesk_flags: BookkeepingFlags::default(),
            icon_name: None,
        };

        // The record of a version below 2 ends here: no further fields, no
        // adjustments, and the reader does not move to the second chunk. A
        // record cut off before the version byte reads as version 0 and
        // takes the same exit.
        if record_version < FIRST_VERSION_WITH_EXTENSION {
            return Ok(account);
        }

        // Fields 22 to 26 are unused.
        for _ in 0..4 {
            let _unused = r.bool();
        }
        let _unused_26 = r.i32();
        let online_balance_date = r.date_clamped();
        account.online_balance = r.decimal("online_balance")?;
        r.skip_string("order_contact_name")?;
        account.flags = AccountFlags::from_raw(r.i32());
        account.opening_date = r.date_clamped();
        (account.holder_street, account.holder_town) = split_address(r.string("holder_address")?);
        account.credit_interest_rate = r.decimal("credit_interest_rate")?;
        account.debit_interest_rate = r.decimal("debit_interest_rate")?;
        account.overdraft_interest_rate = r.decimal("overdraft_interest_rate")?;
        account.overdraft_limit = r.decimal("overdraft_limit")?;
        r.skip_string("fints_account_sub_id")?;
        let last_sync_time = r.ticks("last_sync_time")?;
        account.creditor_id = r.string("creditor_id")?;
        account.reference_account_iban = r.string("reference_account_iban")?;
        if r.bool() {
            // Banking4 trusts the count and allocates by it. Here it must
            // be plausible first: not negative, and no more entries than the
            // rest of the chunk can hold at two bytes each.
            let count = r.i32();
            let count = r.list_len("internal_transfer_ibans", count, 2)?;
            // Not allocated up front: the entries may turn out to be absent.
            let mut ibans = Vec::new();
            for _ in 0..count {
                ibans.push(r.string("internal_transfer_ibans")?);
            }
            account.internal_transfer_ibans = Some(ibans);
        }
        account.originator_name = r.string("originator_name")?;
        r.skip_string("last_download_token")?;
        account.reserved_text = r.string("reserved_text")?;
        account.position = r.u16();
        account.group_oid = r.i32();
        account.balance_flags = BalanceFlags::from_raw(r.u16());
        account.available_funds = r.decimal("available_funds")?;
        let statement_balance_time = r.ticks("statement_balance_time")?;
        let online_balance_time = r.ticks("online_balance_time")?;
        let last_download_time = r.ticks("last_download_time")?;
        account.notify_flags = NotifyFlags::from_raw(r.u16());
        account.notify_min_debit = r.decimal("notify_min_debit")?;
        account.notify_min_credit = r.decimal("notify_min_credit")?;
        account.notify_min_balance = r.decimal("notify_min_balance")?;
        account.notify_max_balance = r.decimal("notify_max_balance")?;
        account.notify_spending_limit = r.decimal("notify_spending_limit")?;
        r.skip_string("fints_customer_id")?;
        account.payments_retention_text = r.string("payments_retention")?;
        account.files_retention_text = r.string("files_retention")?;
        // Field 63 is unused, but Banking4 still rejects a value that is not
        // a time, and so does this reader.
        let _unused_63 = r.ticks("unused_63")?;
        let _contact_guid = r.guid();
        let _order_contact_guid = r.guid();
        account.sevdesk_last_entry_date = r.date_strict("sevdesk_last_entry_date")?;
        account.sevdesk_flags = BookkeepingFlags::from_raw(r.i32());
        account.icon_name = r.string("icon_name")?;
        // The second chunk holds one unused byte. Moving on is unconditional
        // and a no-op in a record without that chunk.
        r.next_chunk()?;
        let _unused_69 = r.u8();

        // Banking4 rejects a text that is not a period. Here the text is
        // kept instead and the period left open (03 open question 12).
        account.payments_retention =
            RetentionPeriod::from_text(account.payments_retention_text.as_deref());
        account.files_retention =
            RetentionPeriod::from_text(account.files_retention_text.as_deref());

        account.last_download_time = merged_time(last_download_date, last_download_time);
        account.last_sync_time = merged_time(last_sync_date, last_sync_time);
        account.statement_balance_time =
            merged_time(statement_balance_date, statement_balance_time);
        account.online_balance_time = merged_time(online_balance_date, online_balance_time);

        account.apply_reader_adjustments();
        Ok(account)
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::super::sample::{Sample, ACCESS_GUID_TEXT, ACCESS_MARKER};
    use super::super::{RetentionUnit, StatementSchedule};
    use super::*;
    use crate::model::base::{load_collection, load_collection_strict, LoadReport};
    use crate::wire::{UnreadBytes, ValueProblem, WireError};

    const TICKS_PER_SECOND: i64 = 10_000_000;

    /// Ticks of a day at a time of day given in seconds.
    fn ticks(year: u16, month: u8, day: u8, seconds: i64) -> i64 {
        Date::new(year, month, day).unwrap().at_midnight().ticks() + seconds * TICKS_PER_SECOND
    }

    fn time(year: u16, month: u8, day: u8, seconds: i64) -> Option<Timestamp> {
        Timestamp::from_ticks(ticks(year, month, day, seconds))
    }

    /// Decode a sample that the decoder must consume to the last byte.
    fn decode(sample: &Sample) -> Account {
        let mut collection = load_collection_strict::<Account>(&sample.file()).unwrap();
        assert_eq!(collection.records.len(), 1);
        collection.records.remove(0)
    }

    /// Decode a sample that must be reported as undecodable, and return why.
    fn undecodable(sample: &Sample) -> (LoadReport, DecodeError) {
        let collection = load_collection::<Account>(&sample.file());
        assert!(collection.records.is_empty());
        assert_eq!(collection.report.undecodable, 1);
        assert_eq!(collection.report.stopped, None);
        let error = collection.report.undecodable_samples[0].error;
        (collection.report, error)
    }

    #[test]
    fn decodes_every_kept_field_of_a_full_record() {
        let account = decode(&Sample::default());
        assert_eq!(account.base.oid, 800_000_001);
        assert_eq!(account.base.update_stamp, 800_000_002);
        assert_eq!(account.account_kind, AccountKind::Bank);
        assert_eq!(account.stored_account_kind, AccountKind::Bank);
        assert_eq!(account.record_version, DOCUMENTED_RECORD_VERSION);
        assert!(!account.is_newer_version());
        assert!(!account.is_placeholder());
        assert_eq!(account.country_code.as_deref(), Some("DE"));
        assert_eq!(account.bank_code.as_deref(), Some("99999901"));
        assert_eq!(account.account_number.as_deref(), Some("1234567"));
        assert_eq!(account.currency.as_deref(), Some("EUR"));
        assert_eq!(account.name.as_deref(), Some("Sample account"));
        assert_eq!(account.iban.as_deref(), Some("XX00SAMPLE"));
        assert_eq!(account.bic.as_deref(), Some("SAMPXX01"));
        assert_eq!(account.holder_name.as_deref(), Some("Sample Holder"));
        assert_eq!(account.description.as_deref(), Some("a description"));
        assert_eq!(account.statement_retention_days, 365);
        assert_eq!(
            account.statement_retention,
            RetentionPeriod::new(RetentionUnit::CalendarYears, 1)
        );
        assert_eq!(account.sync_flags.raw(), 0x06);
        assert_eq!(account.statement_balance.to_string(), "1021.21");
        assert_eq!(account.online_balance.to_string(), "-28.28");
        assert_eq!(account.flags.raw(), 0x1200);
        assert_eq!(
            account.flags.statement_schedule(),
            StatementSchedule::Monthly
        );
        assert!(!account.is_hidden());
        assert_eq!(account.opening_date, Date::new(2001, 2, 3));
        assert_eq!(account.holder_street.as_deref(), Some("1 Sample Street"));
        assert_eq!(account.holder_town.as_deref(), Some("Sampletown"));
        assert_eq!(account.credit_interest_rate.to_string(), "0.033");
        assert_eq!(account.debit_interest_rate.to_string(), "3.4");
        assert_eq!(account.overdraft_interest_rate.to_string(), "0.00035");
        assert_eq!(account.overdraft_limit.to_string(), "3600");
        assert_eq!(account.creditor_id.as_deref(), Some("creditor"));
        assert_eq!(
            account.reference_account_iban.as_deref(),
            Some("XX00REFERENCE")
        );
        assert_eq!(
            account.internal_transfer_ibans,
            Some(vec![Some("XX00TARGET".to_owned())])
        );
        assert_eq!(account.originator_name.as_deref(), Some("originator"));
        assert_eq!(account.reserved_text.as_deref(), Some("reserved"));
        assert_eq!(account.position, 3);
        assert_eq!(account.group_oid, 800_200_001);
        assert_eq!(account.group(), Some(800_200_001));
        assert_eq!(account.balance_flags.raw(), 0x0101);
        assert_eq!(account.available_funds.to_string(), "50.50");
        assert_eq!(account.notify_flags.raw(), 0x1);
        assert_eq!(account.notify_min_debit.to_string(), "55.5");
        assert_eq!(account.notify_min_credit.to_string(), "56.56");
        assert_eq!(account.notify_min_balance.to_string(), "-57.570");
        assert_eq!(account.notify_max_balance.to_string(), "58000");
        assert_eq!(account.notify_spending_limit.to_string(), "59.0059");
        assert_eq!(
            account.payments_retention,
            Some(RetentionPeriod::new(RetentionUnit::CalendarMonths, 6))
        );
        assert_eq!(account.payments_retention_text.as_deref(), Some("M6"));
        assert_eq!(
            account.files_retention,
            Some(RetentionPeriod::new(RetentionUnit::Days, 90))
        );
        assert_eq!(account.files_retention_text.as_deref(), Some("D90"));
        assert_eq!(account.sevdesk_last_entry_date, Date::new(2023, 11, 5));
        assert_eq!(account.sevdesk_flags.raw(), 0x1);
        assert_eq!(account.icon_name.as_deref(), Some("icon"));
    }

    /// The bank access fields are read past and exist nowhere in the decoded
    /// account: not as a key, and not as a value under another name.
    #[test]
    fn bank_access_fields_are_dropped() {
        const DROPPED: [&str; 7] = [
            "fints_customer_id",
            "last_download_token",
            "contact_guid",
            "order_contact_guid",
            "contact_name",
            "order_contact_name",
            "fints_account_sub_id",
        ];
        // A bank account keeps all of them in Banking4; a cash account has
        // them cleared by the reader. Neither may show any here.
        for kind in [0x01, 0x02, 0x04, 0x20] {
            let account = decode(&Sample {
                kind,
                ..Sample::default()
            });
            let json = serde_json::to_value(&account).unwrap();
            let object = json.as_object().unwrap();
            for key in DROPPED {
                assert!(!object.contains_key(key), "key {key}");
            }
            for key in object.keys() {
                assert!(
                    !["contact", "fints", "token", "guid"]
                        .iter()
                        .any(|part| key.contains(part)),
                    "key {key}"
                );
            }
            for text in [json.to_string(), format!("{account:?}")] {
                assert!(!text.contains(ACCESS_MARKER));
                assert!(!text.to_lowercase().contains(ACCESS_GUID_TEXT));
                assert!(!text.to_lowercase().contains("a5a5"));
            }
        }
    }

    #[test]
    fn only_the_low_eight_bits_of_the_kind_are_kept() {
        let account = decode(&Sample {
            kind: 0x0000_0104,
            ..Sample::default()
        });
        assert_eq!(account.stored_account_kind, AccountKind::CreditCard);
        assert_eq!(account.account_kind, AccountKind::CreditCard);

        // 0x100 is the placeholder kind for a loader: the test is on the low
        // eight bits.
        let placeholder = decode(&Sample {
            kind: 0x100,
            ..Sample::default()
        });
        assert!(placeholder.is_placeholder());
    }

    #[test]
    fn tick_precision_wins_over_day_precision() {
        // Only the day.
        let account = decode(&Sample {
            last_download_day: Some((2024, 3, 2)),
            last_sync_day: Some((2024, 3, 3)),
            statement_balance_day: Some((2024, 2, 29)),
            online_balance_day: Some((2024, 3, 1)),
            ..Sample::default()
        });
        assert_eq!(account.last_download_time, time(2024, 3, 2, 0));
        assert_eq!(account.last_sync_time, time(2024, 3, 3, 0));
        assert_eq!(account.statement_balance_time, time(2024, 2, 29, 0));
        assert_eq!(account.online_balance_time, time(2024, 3, 1, 0));

        // Only the time stamp.
        let account = decode(&Sample {
            last_download_ticks: ticks(2024, 3, 2, 3723),
            last_sync_ticks: ticks(2024, 3, 3, 14706),
            statement_balance_ticks: ticks(2024, 2, 29, 49530),
            online_balance_ticks: ticks(2024, 3, 1, 29350),
            ..Sample::default()
        });
        assert_eq!(account.last_download_time, time(2024, 3, 2, 3723));
        assert_eq!(account.last_sync_time, time(2024, 3, 3, 14706));
        assert_eq!(account.statement_balance_time, time(2024, 2, 29, 49530));
        assert_eq!(account.online_balance_time, time(2024, 3, 1, 29350));

        // Both, and different: the two are not compared, the time stamp
        // wins even when it is earlier, and one tick is enough.
        let account = decode(&Sample {
            last_download_day: Some((2024, 3, 2)),
            last_download_ticks: ticks(2020, 1, 1, 60),
            last_sync_day: Some((2024, 3, 3)),
            last_sync_ticks: 1,
            statement_balance_day: Some((2024, 2, 29)),
            statement_balance_ticks: ticks(2025, 6, 7, 0),
            online_balance_day: Some((2024, 3, 1)),
            online_balance_ticks: 0,
            ..Sample::default()
        });
        assert_eq!(account.last_download_time, time(2020, 1, 1, 60));
        assert_eq!(account.last_sync_time, Timestamp::from_ticks(1));
        assert_eq!(account.statement_balance_time, time(2025, 6, 7, 0));
        assert_eq!(account.online_balance_time, time(2024, 3, 1, 0));

        // Neither.
        let account = decode(&Sample::default());
        assert_eq!(account.last_download_time, None);
        assert_eq!(account.last_sync_time, None);
        assert_eq!(account.statement_balance_time, None);
        assert_eq!(account.online_balance_time, None);
    }

    #[test]
    fn impossible_days_are_clamped() {
        let account = decode(&Sample {
            last_download_day: Some((2023, 2, 31)),
            last_sync_day: Some((2024, 13, 40)),
            statement_balance_day: Some((12000, 12, 31)),
            online_balance_day: Some((2024, 0, 5)),
            ..Sample::default()
        });
        assert_eq!(account.last_download_time, time(2023, 2, 28, 0));
        assert_eq!(account.last_sync_time, time(2024, 12, 31, 0));
        assert_eq!(account.statement_balance_time, time(9999, 12, 31, 0));
        assert_eq!(account.online_balance_time, None);
    }

    #[test]
    fn the_bookkeeping_date_is_the_one_date_that_is_not_clamped() {
        // The same impossible day in both: the opening date is clamped, the
        // bookkeeping date makes the record undecodable.
        let account = decode(&Sample {
            opening_day: Some((2023, 2, 31)),
            ..Sample::default()
        });
        assert_eq!(account.opening_date, Date::new(2023, 2, 28));
        let (_, error) = undecodable(&Sample {
            sevdesk_day: Some((2023, 2, 31)),
            ..Sample::default()
        });
        assert!(
            matches!(
                error,
                DecodeError::Wire(WireError::Value {
                    field: "sevdesk_last_entry_date",
                    problem: ValueProblem::ImpossibleDate,
                    ..
                })
            ),
            "{error}"
        );
        // A zero part is "no date" for both.
        let account = decode(&Sample {
            opening_day: Some((2023, 0, 31)),
            sevdesk_day: Some((0, 2, 31)),
            ..Sample::default()
        });
        assert_eq!(account.opening_date, None);
        assert_eq!(account.sevdesk_last_entry_date, None);
    }

    #[test]
    fn step_1_a_kind_without_bookings_has_no_booked_balance() {
        let with_times = Sample {
            statement_balance_day: Some((2024, 2, 29)),
            statement_balance_ticks: ticks(2024, 2, 29, 49530),
            online_balance_ticks: ticks(2024, 3, 1, 29350),
            ..Sample::default()
        };
        for kind in [0x08, 0x10, 0x18, 0x80] {
            let account = decode(&Sample {
                kind,
                ..with_times.clone()
            });
            assert_eq!(account.statement_balance, Decimal::ZERO, "kind {kind:#x}");
            assert_eq!(account.statement_balance_time, None);
            // The online balance is not touched.
            assert_eq!(account.online_balance.to_string(), "-28.28");
            assert_eq!(account.online_balance_time, time(2024, 3, 1, 29350));
        }
        for kind in [0x01, 0x02, 0x04, 0x20, 0x40, 0x03, 0x09] {
            let account = decode(&Sample {
                kind,
                ..with_times.clone()
            });
            assert_eq!(
                account.statement_balance.to_string(),
                "1021.21",
                "kind {kind:#x}"
            );
            assert_eq!(account.statement_balance_time, time(2024, 2, 29, 49530));
        }
    }

    #[test]
    fn step_2_a_cash_account_has_no_online_side() {
        let online = Sample {
            last_download_day: Some((2024, 3, 2)),
            last_download_ticks: ticks(2024, 3, 2, 3723),
            last_sync_day: Some((2024, 3, 3)),
            last_sync_ticks: ticks(2024, 3, 3, 14706),
            statement_balance_ticks: ticks(2024, 2, 29, 49530),
            online_balance_day: Some((2024, 3, 1)),
            online_balance_ticks: ticks(2024, 3, 1, 29350),
            sync_flags: 0x1DFE,
            ..Sample::default()
        };
        // 0x102 is a cash account: the upper bits are not part of the kind.
        for kind in [0x02, 0x102] {
            let account = decode(&Sample {
                kind,
                ..online.clone()
            });
            assert_eq!(account.account_kind, AccountKind::Cash);
            assert_eq!(account.internal_transfer_ibans, None);
            assert_eq!(account.online_balance, Decimal::ZERO);
            assert_eq!(account.online_balance_time, None);
            assert_eq!(account.last_download_time, None);
            assert_eq!(account.last_sync_time, None);
            assert!(account.sync_flags.is_empty());
            // The booked balance stays.
            assert_eq!(account.statement_balance.to_string(), "1021.21");
            assert_eq!(account.statement_balance_time, time(2024, 2, 29, 49530));
        }
        // 0x03 is neither bank nor cash: nothing is cleared.
        let account = decode(&Sample {
            kind: 0x03,
            ..online
        });
        assert_eq!(account.online_balance.to_string(), "-28.28");
        assert_eq!(account.last_sync_time, time(2024, 3, 3, 14706));
        assert_eq!(account.sync_flags.raw(), 0x1DFE);
        assert!(account.internal_transfer_ibans.is_some());
    }

    #[test]
    fn step_3_old_bank_accounts_gain_two_sync_flags() {
        let cases = [
            // version, kind, stored flags, flags after reading
            (18, 0x01, 0x06, 0x0506),
            (2, 0x01, 0x00, 0x0500),
            (18, 0x01, 0x0506, 0x0506),
            (19, 0x01, 0x06, 0x06),
            (26, 0x01, 0x06, 0x06),
            (18, 0x04, 0x06, 0x06),
            (18, 0x03, 0x06, 0x06),
            // A cash account is cleared by step 2 and is not a bank account.
            (18, 0x02, 0x06, 0x00),
        ];
        for (version, kind, stored, expected) in cases {
            let account = decode(&Sample {
                version,
                kind,
                sync_flags: stored,
                ..Sample::default()
            });
            assert_eq!(
                account.sync_flags.raw(),
                expected,
                "version {version}, kind {kind:#x}"
            );
        }
    }

    #[test]
    fn step_4_cash_in_a_crypto_currency_is_a_crypto_account() {
        for currency in ["ETH", "LTC", "XBT", "XRP"] {
            let account = decode(&Sample {
                kind: 0x02,
                currency,
                sync_flags: 0x06,
                ..Sample::default()
            });
            assert_eq!(account.account_kind, AccountKind::Crypto, "{currency}");
            assert_eq!(account.stored_account_kind, AccountKind::Cash);
            // What step 2 cleared stays cleared.
            assert!(account.sync_flags.is_empty());
            assert_eq!(account.online_balance, Decimal::ZERO);
            assert_eq!(account.internal_transfer_ibans, None);
        }
        // Primary codes only, and case sensitive.
        for currency in ["BTC", "xbt", "EUR", "XB", ""] {
            let account = decode(&Sample {
                kind: 0x02,
                currency,
                ..Sample::default()
            });
            assert_eq!(account.account_kind, AccountKind::Cash, "{currency}");
        }
        // Only a cash account changes its kind.
        for kind in [0x01, 0x03, 0x04, 0x40] {
            let account = decode(&Sample {
                kind,
                currency: "XBT",
                ..Sample::default()
            });
            assert_eq!(account.account_kind, account.stored_account_kind);
        }
    }

    #[test]
    fn below_version_2_the_record_ends_after_the_statement_balance() {
        for version in [0, 1] {
            // A kind without bookings in a crypto currency: no step applies.
            let sample = Sample {
                version,
                kind: 0x08,
                currency: "XBT",
                statement_balance_day: Some((2024, 2, 29)),
                statement_balance_ticks: ticks(2024, 2, 29, 49530),
                last_sync_day: Some((2024, 3, 3)),
                sync_flags: 0x06,
                ..Sample::default()
            };
            let collection = load_collection::<Account>(&sample.file());
            let account = &collection.records[0];
            assert_eq!(account.record_version, version);
            // Fields 1 to 21 are read.
            assert_eq!(account.account_kind, AccountKind::Portfolio);
            assert_eq!(account.name.as_deref(), Some("Sample account"));
            assert_eq!(account.statement_retention_days, 365);
            assert_eq!(account.sync_flags.raw(), 0x06);
            assert_eq!(account.last_sync_time, time(2024, 3, 3, 0));
            // No processing: the balance of a portfolio stays, with its day.
            assert_eq!(account.statement_balance.to_string(), "1021.21");
            assert_eq!(account.statement_balance_time, time(2024, 2, 29, 0));
            // Nothing behind field 21, although the bytes are there.
            assert_eq!(account.online_balance, Decimal::ZERO);
            assert!(account.flags.is_empty());
            assert_eq!(account.opening_date, None);
            assert_eq!(account.holder_street, None);
            assert_eq!(account.internal_transfer_ibans, None);
            assert_eq!(account.position, 0);
            assert_eq!(account.group_oid, 0);
            assert_eq!(account.group(), None);
            assert_eq!(account.icon_name, None);
            assert_eq!(account.sevdesk_last_entry_date, None);
            // The text-form periods are "not set", not "unlimited".
            assert_eq!(account.payments_retention, Some(RetentionPeriod::NOT_SET));
            assert_eq!(account.files_retention, Some(RetentionPeriod::NOT_SET));
            assert_eq!(account.payments_retention_text, None);
            // The reader does not move to the second chunk, which is then
            // left over; the rest of chunk 1 is unread as well.
            assert_eq!(collection.report.extra_chunks.records, 1);
            assert_eq!(collection.report.unread_bytes.records, 1);
        }
    }

    #[test]
    fn the_holder_address_is_split_at_the_first_line_feed() {
        let cases: [(Option<&'static str>, Option<&str>, Option<&str>); 12] = [
            (None, None, None),
            (Some("street\ntown"), Some("street"), Some("town")),
            // Without a line feed the text is the street, untouched.
            (Some("  street  "), Some("  street  "), None),
            (Some("a\rb"), Some("a\rb"), None),
            // Each part is trimmed and an empty part is missing.
            (Some("  street \n town  "), Some("street"), Some("town")),
            (Some("\ntown"), None, Some("town")),
            (Some("street\n"), Some("street"), None),
            (Some(" \n\t"), None, None),
            // Only the first line feed splits.
            (Some("a\nb\nc"), Some("a"), Some("b\nc")),
            // A carriage return is white space, not a line break.
            (Some("a\r\nb"), Some("a"), Some("b")),
            // 85 and A0 are trimmed, 1C to 1F are not.
            (
                Some("\u{A0}a\u{85}\n\u{2003}b\u{3000}"),
                Some("a"),
                Some("b"),
            ),
            (Some("\u{1C}a\n b\u{1F}"), Some("\u{1C}a"), Some("b\u{1F}")),
        ];
        for (address, street, town) in cases {
            let account = decode(&Sample {
                holder_address: address,
                ..Sample::default()
            });
            assert_eq!(account.holder_street.as_deref(), street, "{address:?}");
            assert_eq!(account.holder_town.as_deref(), town, "{address:?}");
        }
    }

    #[test]
    fn the_transfer_list_can_be_absent_empty_or_hold_missing_entries() {
        let list = |transfer_list| {
            decode(&Sample {
                transfer_list,
                ..Sample::default()
            })
            .internal_transfer_ibans
        };
        assert_eq!(list(None), None);
        assert_eq!(list(Some((0, vec![]))), Some(vec![]));
        assert_eq!(
            list(Some((3, vec![Some("XX00A"), None, Some(" ")]))),
            Some(vec![Some("XX00A".to_owned()), None, Some(" ".to_owned())])
        );
    }

    #[test]
    fn an_implausible_list_count_makes_the_record_undecodable() {
        // Negative, and more entries than the rest of the chunk could hold
        // at two bytes each. Nothing is allocated by either.
        for count in [-1, i32::MIN, i32::MAX, 100_000] {
            let (_, error) = undecodable(&Sample {
                transfer_list: Some((count, vec![Some("XX00A")])),
                ..Sample::default()
            });
            assert!(
                matches!(
                    error,
                    DecodeError::Wire(WireError::Value {
                        field: "internal_transfer_ibans",
                        problem: ValueProblem::CountOutOfRange,
                        ..
                    })
                ),
                "count {count}: {error}"
            );
        }
        // The limit is exact: half the bytes left behind the count, since
        // the smallest entry takes two bytes.
        let without = Sample {
            transfer_list: None,
            ..Sample::default()
        }
        .record();
        let with = Sample {
            transfer_list: Some((0, vec![])),
            ..Sample::default()
        }
        .record();
        // Behind the chunk header the two differ first in the presence
        // byte; the count follows it, and the first chunk ends five bytes
        // before the record does.
        let presence = 4 + without[4..]
            .iter()
            .zip(&with[4..])
            .position(|(a, b)| a != b)
            .unwrap();
        let left = (with.len() - 5) - (presence + 5);
        let outcome = |count: usize| {
            let sample = Sample {
                transfer_list: Some((i32::try_from(count).unwrap(), vec![])),
                ..Sample::default()
            };
            let report = load_collection::<Account>(&sample.file()).report;
            report
                .undecodable_samples
                .first()
                .map(|sample| sample.error)
        };
        assert!(!matches!(
            outcome(left / 2),
            Some(DecodeError::Wire(WireError::Value {
                problem: ValueProblem::CountOutOfRange,
                ..
            }))
        ));
        assert!(matches!(
            outcome(left / 2 + 1),
            Some(DecodeError::Wire(WireError::Value {
                problem: ValueProblem::CountOutOfRange,
                ..
            }))
        ));

        // A count that is too large but still fits the chunk is trusted, as
        // in Banking4: the following fields are taken as entries.
        let collection = load_collection::<Account>(
            &Sample {
                transfer_list: Some((3, vec![Some("XX00A")])),
                ..Sample::default()
            }
            .file(),
        );
        assert_eq!(collection.report.stopped, None);
        assert_eq!(collection.report.framed(), 1);
    }

    #[test]
    fn a_retention_text_that_is_not_a_period_is_kept_as_text() {
        let account = decode(&Sample {
            payments_retention: Some("D"),
            files_retention: Some("D5x"),
            ..Sample::default()
        });
        assert_eq!(account.payments_retention, None);
        assert_eq!(account.payments_retention_text.as_deref(), Some("D"));
        assert_eq!(account.files_retention, None);
        assert_eq!(account.files_retention_text.as_deref(), Some("D5x"));

        // No text is "unlimited"; a period reaching back before the year 1,
        // which Banking4 rejects, is just a period here.
        let account = decode(&Sample {
            payments_retention: None,
            files_retention: Some("Y3000"),
            ..Sample::default()
        });
        assert_eq!(account.payments_retention, Some(RetentionPeriod::UNLIMITED));
        assert_eq!(account.payments_retention_text, None);
        assert_eq!(
            account.files_retention,
            Some(RetentionPeriod::new(RetentionUnit::CalendarYears, 3000))
        );
    }

    #[test]
    fn values_banking4_rejects_make_the_record_undecodable() {
        // An out-of-range time fails even in the unused field 63.
        let (_, error) = undecodable(&Sample {
            unused_63: -1,
            ..Sample::default()
        });
        assert!(matches!(
            error,
            DecodeError::Wire(WireError::Value {
                problem: ValueProblem::TicksOutOfRange,
                ..
            })
        ));
        let (_, error) = undecodable(&Sample {
            online_balance_ticks: i64::MAX,
            ..Sample::default()
        });
        assert!(matches!(
            error,
            DecodeError::Wire(WireError::Value {
                field: "online_balance_time",
                problem: ValueProblem::TicksOutOfRange,
                ..
            })
        ));
        // Below version 2 those fields are not read and cannot fail.
        let collection = load_collection::<Account>(
            &Sample {
                version: 1,
                unused_63: -1,
                ..Sample::default()
            }
            .file(),
        );
        assert_eq!(collection.report.decoded, 1);
    }

    #[test]
    fn a_record_without_its_second_chunk_reads_the_same() {
        let with = decode(&Sample::default());
        let without = decode(&Sample {
            second_chunk: false,
            ..Sample::default()
        });
        assert_eq!(with, without);
    }

    #[test]
    fn bytes_and_chunks_of_a_newer_version_are_left_for_the_loader() {
        // Version 27: ten more bytes at the end of chunk 1.
        let sample = Sample {
            version: 27,
            trailing: vec![0; 10],
            ..Sample::default()
        };
        let collection = load_collection::<Account>(&sample.file());
        let newer = &collection.records[0];
        assert!(newer.is_newer_version());
        assert_eq!(
            Account {
                record_version: DOCUMENTED_RECORD_VERSION,
                ..newer.clone()
            },
            decode(&Sample::default())
        );
        assert_eq!(collection.report.unread_bytes.records, 1);
        assert_eq!(
            collection.report.unread_histogram.get(&UnreadBytes {
                chunk: 1,
                bytes: 10
            }),
            Some(&1)
        );
        assert!(collection.report.extra_chunks.is_zero());

        // A third chunk is skipped and counted.
        let sample = Sample {
            third_chunk: true,
            ..Sample::default()
        };
        let collection = load_collection::<Account>(&sample.file());
        assert_eq!(collection.records[0], decode(&Sample::default()));
        assert_eq!(collection.report.extra_chunks.records, 1);
        assert!(collection.report.unread_bytes.is_zero());
    }

    #[test]
    fn an_empty_record_decodes_to_defaults() {
        let record = super::super::sample::chunk(&[], false);
        let collection = load_collection::<Account>(&super::super::sample::file(&[record]));
        let account = &collection.records[0];
        assert_eq!(
            account.base,
            RecordBase {
                oid: 0,
                update_stamp: 0
            }
        );
        assert!(account.is_placeholder());
        assert_eq!(account.record_version, 0);
        assert_eq!(account.name, None);
        assert_eq!(account.statement_retention, RetentionPeriod::NOT_SET);
        assert_eq!(account.statement_balance, Decimal::ZERO);
        assert_eq!(account.last_download_time, None);
    }
}
