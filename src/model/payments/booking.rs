//! Bookings: the entries of an account statement, `Acct<oid>/Stmt`
//! (04 §6).

use rust_decimal::Decimal;
use serde::Serialize;

use crate::model::base::{DecodeError, Record};
use crate::wire::{Date, RawDate, Reader, TimeOfDay};

use super::base::{PaymentBase, SchemeRule};
use super::enums::{
    BatchRole, BookingStatus, ColorFlag, EntryBits, SplitRole, TransactionCode, VatRateType,
};
use super::{ACCOUNT_KIND_CASH, ACCOUNT_KIND_PAYPAL};

/// One entry of an account statement (04 §6).
///
/// [`Record::decode`] applies the adjustments Banking4 makes on reading that
/// need nothing but the record: an undated entry is pending, a colour flag
/// out of range is none, a master without a link points at itself (04 §6.2
/// steps 1, 2 and 5 to 7). The two that depend on the account are separate:
/// [`Booking::adjusted_currency`] and [`Booking::apply_account_kind`].
///
/// The last two fields are not stored. They are filled in by
/// [`regroup_bookings`](super::regroup_bookings) over all bookings of an
/// account.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Booking {
    /// Object header and payment base.
    #[serde(flatten)]
    pub base: PaymentBase,
    /// Date the bank booked the entry; none for an entry that is not booked
    /// yet. Components as stored.
    pub booking_date: Option<RawDate>,
    /// Value date, components as stored.
    pub value_date: Option<RawDate>,
    /// Kind of transaction as stored. Banking4 may display another one
    /// (04 §6.6).
    pub transaction_code: TransactionCode,
    /// Booking text supplied by the bank.
    pub booking_text: Option<String>,
    /// Primanota number.
    pub primanota: Option<String>,
    /// Markers; bit 0 says the user has seen the entry.
    pub entry_bits: EntryBits,
    /// Colour flag as Banking4 keeps it: a stored value outside 0 to 7 is
    /// none (04 §6.2 step 2).
    pub flag: ColorFlag,
    /// Colour flag as stored.
    pub stored_flag: ColorFlag,
    /// Role in a split or batch as stored. Banking4 clears it when the group
    /// does not hold together; see [`Booking::effective_split`].
    pub split: SplitRole,
    /// Object id of the master this entry belongs to, after 04 §6.2 steps 5
    /// to 7: 0 if none, the entry's own id for a master. Banking4 clears it
    /// when the group does not hold together; see
    /// [`Booking::effective_batch_oid`].
    pub batch_oid: i32,
    /// Reference assigned by the bank.
    pub bank_reference: Option<String>,
    /// Free text note by the user.
    pub notes: Option<String>,
    /// Whether the entry is booked, after 04 §6.2 step 1: an entry stored as
    /// booked without a booking date is pending.
    pub booking_status: BookingStatus,
    /// German business transaction code, read as a signed number (04 §6.1
    /// position 13).
    pub gvc: i16,
    /// Text key extension, read as a signed number.
    pub gvc_extension: i16,
    /// Date the user assigned for evaluation, overriding the other dates in
    /// reports.
    pub analysis_date: Option<Date>,
    /// Date of the underlying transaction, for example the card payment.
    pub transaction_date: Option<Date>,
    /// SWIFT transaction type code from the statement.
    pub swift_transaction_code: Option<String>,
    /// VAT contained in the amount, without sign.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub vat_amount: Decimal,
    /// VAT treatment.
    pub vat_rate_type: VatRateType,
    /// Original amount, if the booking was converted from another currency.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub instructed_amount: Decimal,
    /// Currency of the original amount.
    pub instructed_currency: Option<String>,
    /// Charges deducted by the bank.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub charges_amount: Decimal,
    /// Currency of the charges.
    pub charges_currency: Option<String>,
    /// Bank reference needed to return this direct debit online.
    pub debit_return_order_id: Option<String>,
    /// Entry date that belongs to that reference, components as stored.
    pub debit_return_date: Option<RawDate>,
    /// Entry time that belongs to that reference.
    pub debit_return_time: Option<TimeOfDay>,
    /// Virtual IBAN the payment was addressed to.
    pub virtual_iban: Option<String>,
    /// Position among the slaves of its group, counted from 0; 0 for every
    /// other entry (04 §6.4). Not stored.
    pub batch_index: u32,
    /// Set when Banking4 would take this entry out of its group and show it
    /// as an ordinary booking: a master left without slaves, or a slave its
    /// master does not reach (04 §6.4 step 3). Not stored.
    pub ungrouped_by_banking4: bool,
}

impl Record for Booking {
    const NAME: &'static str = "booking";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let base = PaymentBase::read(r, SchemeRule::AlwaysNone)?;
        // In a legacy record this does nothing, and the fields below follow
        // the category in the only chunk (04 §6.1).
        r.next_chunk()?;

        // 04 §6.1, in wire order.
        let booking_date = r.date_raw();
        let value_date = r.date_raw();
        let transaction_code = TransactionCode::from_raw(r.i32());
        let booking_text = r.string("booking_text")?;
        let primanota = r.string("primanota")?;
        let entry_bits = EntryBits::from_raw(r.u8());
        let stored_flag = ColorFlag::from_raw(r.i32());
        let split = SplitRole::from_raw(r.u8());
        let stored_batch_oid = r.i32();
        let bank_reference = r.string("bank_reference")?;
        let notes = r.string("notes")?;
        let stored_status = BookingStatus::from_raw(r.u8());
        let gvc = r.u16_signed();
        let gvc_extension = r.u16_signed();
        let analysis_date = r.date_clamped();
        let transaction_date = r.date_clamped();
        let swift_transaction_code = r.string("swift_transaction_code")?;
        let vat_amount = r.decimal("vat_amount")?;
        let vat_rate_type = VatRateType::from_raw(r.u8());
        let instructed_amount = r.decimal("instructed_amount")?;
        let instructed_currency = r.code(3);
        let charges_amount = r.decimal("charges_amount")?;
        let charges_currency = r.code(3);
        let debit_return_order_id = r.string("debit_return_order_id")?;
        let debit_return_date = r.date_raw();
        let debit_return_time = r.time_hms("debit_return_time")?;
        let virtual_iban = r.string("virtual_iban")?;

        // 04 §6.2 step 1: without a booking date the entry is not booked.
        let booking_status = if booking_date.is_none() && stored_status == BookingStatus::Booked {
            BookingStatus::Pending
        } else {
            stored_status
        };
        // Step 2.
        let flag = if (0..=7).contains(&stored_flag.raw()) {
            stored_flag
        } else {
            ColorFlag::None
        };
        // Steps 5 to 7: a master of an old record does not name itself yet.
        let batch_oid = match stored_batch_oid {
            0 | -1 if split.is_any_master() => base.record.oid,
            -1 => 0,
            other => other,
        };

        Ok(Self {
            base,
            booking_date,
            value_date,
            transaction_code,
            booking_text,
            primanota,
            entry_bits,
            flag,
            stored_flag,
            split,
            batch_oid,
            bank_reference,
            notes,
            booking_status,
            gvc,
            gvc_extension,
            analysis_date,
            transaction_date,
            swift_transaction_code,
            vat_amount,
            vat_rate_type,
            instructed_amount,
            instructed_currency,
            charges_amount,
            charges_currency,
            debit_return_order_id,
            debit_return_date,
            debit_return_time,
            virtual_iban,
            batch_index: 0,
            ungrouped_by_banking4: false,
        })
    }

    fn oid(&self) -> i32 {
        self.base.record.oid
    }

    fn is_legacy(&self) -> bool {
        self.base.legacy
    }
}

impl Booking {
    /// The amount with its sign: negative when money leaves the account. For
    /// a booking the direction is always the stored bit (04 §5.5).
    #[must_use]
    pub fn signed_amount(&self) -> Decimal {
        self.base.signed_amount()
    }

    /// The amount without the VAT it contains (04 §6.1 position 18), or
    /// `None` if the difference is not representable.
    #[must_use]
    pub fn net_amount(&self) -> Option<Decimal> {
        self.base.amount.checked_sub(self.vat_amount)
    }

    /// The currency Banking4 keeps for the booking after reading it (04 §6.2
    /// step 3): the account's currency, except in a PayPal account, where the
    /// stored one stays.
    ///
    /// `account_kind` is the kind of the account whose directory the file is
    /// in, `account_currency` that account's currency. An account without a
    /// currency of its own uses the application's default, which is not in
    /// the vault; the caller decides what to pass then (EUR is the product's
    /// default).
    ///
    /// What Banking4 *displays* for a booking is the account's currency in
    /// every case, PayPal included: see [`Booking::displayed_currency`].
    #[must_use]
    pub fn adjusted_currency<'a>(
        &'a self,
        account_kind: u8,
        account_currency: Option<&'a str>,
    ) -> Option<&'a str> {
        if account_kind == ACCOUNT_KIND_PAYPAL {
            self.base.currency.as_deref()
        } else {
            account_currency
        }
    }

    /// The currency Banking4 displays for the booking (04 §6.2, after the
    /// list of steps): the currency of its account, whatever the record
    /// stores and whatever the kind of the account. In a PayPal account that
    /// can be another currency than the one the record keeps
    /// ([`Booking::adjusted_currency`]).
    ///
    /// For an account without a currency of its own Banking4 uses the
    /// application's default, which is not in the vault. With `None` for
    /// `account_currency` this returns the stored code in its place.
    #[must_use]
    pub fn displayed_currency<'a>(&'a self, account_currency: Option<&'a str>) -> Option<&'a str> {
        account_currency.or(self.base.currency.as_deref())
    }

    /// Apply the adjustment that depends on the kind of the account the
    /// booking belongs to (04 §6.2 step 4): in a cash account a booking has
    /// no value date, analysis date or transaction date, whatever is stored.
    pub fn apply_account_kind(&mut self, account_kind: u8) {
        if account_kind == ACCOUNT_KIND_CASH {
            self.value_date = None;
            self.analysis_date = None;
            self.transaction_date = None;
        }
    }

    /// The link to the master as Banking4 shows it: 0 once the entry was
    /// taken out of its group (04 §6.4 step 3).
    #[must_use]
    pub const fn effective_batch_oid(&self) -> i32 {
        if self.ungrouped_by_banking4 {
            0
        } else {
            self.batch_oid
        }
    }

    /// The split role as Banking4 shows it: none once the entry was taken
    /// out of its group (04 §6.4 step 3).
    #[must_use]
    pub fn effective_split(&self) -> SplitRole {
        if self.ungrouped_by_banking4 {
            SplitRole::default()
        } else {
            self.split
        }
    }

    /// Whether the entry is a master, a slave or neither (04 §5.10), after
    /// the regrouping if it was done.
    #[must_use]
    pub const fn batch_role(&self) -> BatchRole {
        BatchRole::of(self.base.record.oid, self.effective_batch_oid())
    }

    /// Whether the entry's amount counts towards the balance. Masters do
    /// not: their slaves carry the money (04 §6.4). Sum the amounts of the
    /// counted entries only.
    #[must_use]
    pub const fn is_counted(&self) -> bool {
        !matches!(self.batch_role(), BatchRole::Master)
    }

    /// The category Banking4 displays (04 §5.7): none for a master, even if
    /// one is stored.
    #[must_use]
    pub fn displayed_category(&self) -> Option<&str> {
        match self.batch_role() {
            BatchRole::Master => None,
            _ => self.base.category.as_deref(),
        }
    }
}
