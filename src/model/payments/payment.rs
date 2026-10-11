//! Payments: orders that were entered, queued or sent, `Acct<oid>/Paymts`
//! (04 §8).

use rust_decimal::Decimal;
use serde::Serialize;

use crate::model::base::{DecodeError, Record};
use crate::wire::Reader;

use super::base::{PaymentBase, SchemeRule};
use super::enums::{
    BatchBooking, BatchRole, ColorFlag, PayeeVerificationChoice, PaymentFlags, PaymentState,
    SepaStatus,
};

/// One payment (04 §8).
///
/// The record stores two one-time authorisation values, `used_tan` and
/// `order_reference_tan`. They are bank access material: the decoder steps
/// over them and they do not exist here.
///
/// The last three fields are not stored. They are filled in by
/// [`apply_payment_rules`](super::apply_payment_rules) over all payments of
/// an account, which also replaces legacy values of `state` and `flags`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Payment {
    /// Object header and payment base.
    #[serde(flatten)]
    pub base: PaymentBase,
    /// Processing state.
    pub state: PaymentState,
    /// Flags.
    pub flags: PaymentFlags,
    /// Object id of the outbox order this payment was submitted with; 0 or
    /// -1 if none.
    pub outbox_oid: i32,
    /// Object id of the batch master, 0 if none (a stored -1 is 0, 04 §8.2).
    /// Banking4 clears it when the batch does not hold together; see
    /// [`Payment::effective_batch_oid`].
    pub batch_oid: i32,
    /// Colour flag. Not limited to the known colours.
    pub color_flag: ColorFlag,
    /// Name of the initiating party of a batch.
    pub initiator_name: Option<String>,
    /// Whether a batch is to be booked as one entry.
    pub batch_booking: BatchBooking,
    /// Identifier assigned when the payment was imported from a file.
    pub import_id: Option<String>,
    /// Object id of the booking in the same account this payment was matched
    /// with; 0 if none.
    pub reconciled_booking_oid: i32,
    /// Internal reference generated when the payment was created.
    pub payment_reference: Option<String>,
    /// Result of the verification of payee, as the stored JSON text. Banking4
    /// rejects a record whose text it cannot parse (04 §8.5); here the text
    /// is kept unparsed and never a reason to reject.
    pub payee_verification_json: Option<String>,
    /// Four-letter status code from a payment status report, as stored.
    pub sepa_status_code: Option<String>,
    /// The status Banking4 makes of that code.
    pub sepa_status: SepaStatus,
    /// Reason code from a payment status report.
    pub sepa_reason_code: Option<String>,
    /// Additional information from a payment status report, as the stored
    /// text with one line per item. Banking4 loses it on reading (04 §8.2).
    pub sepa_additional_info: Option<String>,
    /// The user's choice for the verification of payee.
    pub payee_verification_choice: PayeeVerificationChoice,
    /// Set when Banking4 would not show this payment at all: its state is
    /// the legacy "advised direct debit" (04 §8.3 step 1). Not stored.
    pub hidden_by_banking4: bool,
    /// Position among the slaves of its batch, counted from 0; 0 for every
    /// other payment (04 §6.4). Not stored.
    pub batch_index: u32,
    /// Set when Banking4 would take this payment out of its batch: a master
    /// left without slaves, or a slave its master does not reach (04 §6.4
    /// step 3). Not stored.
    pub ungrouped_by_banking4: bool,
}

impl Record for Payment {
    const NAME: &'static str = "payment";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let base = PaymentBase::read(r, SchemeRule::DeriveIfMissing)?;
        // A no-op in a legacy record (04 §8.1).
        r.next_chunk()?;

        // 04 §8.1, in wire order. Positions 4, 11 and 19 are unused and
        // positions 9 and 13 are authorisation values: read to stay in step,
        // never kept.
        let state = PaymentState::from_raw(r.u16());
        let flags = PaymentFlags::from_raw(r.u16());
        let outbox_oid = r.i32();
        let _reserved_a = r.i32();
        let stored_batch_oid = r.i32();
        let color_flag = ColorFlag::from_raw(r.i32());
        let initiator_name = r.string("initiator_name")?;
        let batch_booking = BatchBooking::from_raw(r.u8());
        r.skip_string("used_tan")?;
        let import_id = r.string("import_id")?;
        let _reserved_b = r.ticks("reserved_b")?;
        let reconciled_booking_oid = r.i32();
        r.skip_string("order_reference_tan")?;
        let payment_reference = r.string("payment_reference")?;
        let payee_verification_json = r.string("payee_verification_json")?;
        let sepa_status_code = r.string("sepa_status_code")?;
        let sepa_reason_code = r.string("sepa_reason_code")?;
        let sepa_additional_info = r.string("sepa_additional_info")?;
        let _reserved_c = r.bool();
        let payee_verification_choice = PayeeVerificationChoice::from_raw(r.u8());

        Ok(Self {
            base,
            state,
            flags,
            outbox_oid,
            // 04 §8.2 step 1. Unlike for a booking, nothing repairs a master.
            batch_oid: if stored_batch_oid == -1 {
                0
            } else {
                stored_batch_oid
            },
            color_flag,
            initiator_name,
            batch_booking,
            import_id,
            reconciled_booking_oid,
            payment_reference,
            payee_verification_json,
            sepa_status: SepaStatus::from_code(sepa_status_code.as_deref()),
            sepa_status_code,
            sepa_reason_code,
            sepa_additional_info,
            payee_verification_choice,
            hidden_by_banking4: false,
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

impl Payment {
    /// Whether money leaves the account. For a payment the scheme decides,
    /// not the stored bit (04 §8.6).
    #[must_use]
    pub fn is_debit(&self) -> bool {
        self.base.is_debit()
    }

    /// The amount with its sign: negative when money leaves the account
    /// (04 §5.5).
    #[must_use]
    pub fn signed_amount(&self) -> Decimal {
        self.base.signed_amount()
    }

    /// The currency Banking4 reports (04 §5.11): the stored one; without one
    /// the account's currency for a payment the user entered by hand, `DEM`
    /// or `EUR` where the scheme implies it, otherwise none.
    ///
    /// `account_currency` is the currency of the account whose directory the
    /// file is in, with whatever the caller uses for an account without one.
    #[must_use]
    pub fn displayed_currency<'a>(&'a self, account_currency: Option<&'a str>) -> Option<&'a str> {
        self.base.currency_or_scheme_default(account_currency)
    }

    /// The link to the batch master as Banking4 shows it: 0 once the payment
    /// was taken out of its batch (04 §6.4 step 3).
    #[must_use]
    pub const fn effective_batch_oid(&self) -> i32 {
        if self.ungrouped_by_banking4 {
            0
        } else {
            self.batch_oid
        }
    }

    /// Whether the payment is a batch master, a slave or neither (04 §5.10),
    /// after the regrouping if it was done.
    #[must_use]
    pub const fn batch_role(&self) -> BatchRole {
        BatchRole::of(self.base.record.oid, self.effective_batch_oid())
    }

    /// The category Banking4 displays (04 §5.7): none for a batch master,
    /// even if one is stored.
    #[must_use]
    pub fn displayed_category(&self) -> Option<&str> {
        match self.batch_role() {
            BatchRole::Master => None,
            _ => self.base.category.as_deref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{record, Payload};
    use super::*;
    use crate::model::payments::SchemeType;
    use crate::wire::Reader;

    /// The first chunk of a current record: head and extension with a SEPA
    /// credit transfer of 12.34.
    fn first_chunk() -> Payload {
        let head = Payload::default()
            .i32(800_000_001)
            .i32(810_000_001)
            .u8(0x11)
            .cents(1234)
            .code(3, "EUR")
            .string("name")
            .string("account")
            .string("bank")
            .code(2, "DE")
            .string("line one\nline two")
            .date(2024, 2, 29)
            .string("order")
            .string("Wohnen:Miete");
        // Twelve texts with the mandate date after the seventh, the cost
        // bearer and its account, five texts, scheme, time, one text.
        let mut extension = head;
        for number in 0..7 {
            extension = extension.string(&format!("ext{number}"));
        }
        extension = extension.date(2020, 1, 2);
        for number in 7..11 {
            extension = extension.string(&format!("ext{number}"));
        }
        extension = extension.u8(2).i32(7);
        for number in 11..16 {
            extension = extension.string(&format!("ext{number}"));
        }
        extension.i32(5).time(Some((1, 2, 3))).string("reg")
    }

    fn second_chunk() -> Payload {
        Payload::default()
            .u16(0x0040)
            .u16(0x0100)
            .i32(830_000_001)
            .i32(0x0BAD_F00D)
            .i32(-1)
            .i32(3)
            .string("initiator")
            .u8(1)
            .string("TAN-USED-123456")
            .string("import")
            .ticks(0)
            .i32(840_000_001)
            .string("TAN-REFERENCE-654321")
            .string("reference")
            .string("{}")
            .string("ACSC")
            .string("AC01")
            .string("info one\ninfo two")
            .u8(1)
            .u8(2)
    }

    fn decode(body: &[u8]) -> Payment {
        let mut reader = Reader::new(body).expect("a well-formed test record");
        let payment = Payment::decode(&mut reader).expect("a decodable test record");
        assert!(
            reader.finish().is_ok_and(|stats| stats.is_exact()),
            "the record was not consumed"
        );
        payment
    }

    #[test]
    fn a_payment_decodes_field_by_field() {
        let payment = decode(&record(&[first_chunk(), second_chunk()]));
        assert!(!payment.base.legacy);
        assert_eq!(payment.base.record.oid, 800_000_001);
        assert_eq!(payment.base.amount.to_string(), "12.34");
        assert_eq!(payment.signed_amount().to_string(), "-12.34");
        assert_eq!(payment.base.scheme_type, SchemeType::SepaCreditTransfer);
        assert_eq!(
            payment.base.purpose().as_deref(),
            Some("line one\nline two")
        );
        assert_eq!(
            payment.base.regulatory_reporting_code.as_deref(),
            Some("reg")
        );
        // Not an instant transfer, so the stored time is not reported.
        assert!(payment.base.scheduled_time.is_some());
        assert_eq!(payment.base.reported_scheduled_time(), None);
        assert_eq!(payment.state, PaymentState::SENT);
        assert_eq!(payment.flags, PaymentFlags::NEEDS_MORE_AUTHORISATION);
        assert_eq!(payment.outbox_oid, 830_000_001);
        assert_eq!(payment.batch_oid, 0, "a stored -1 is no batch");
        assert_eq!(payment.color_flag, ColorFlag::Blue);
        assert_eq!(payment.initiator_name.as_deref(), Some("initiator"));
        assert_eq!(payment.batch_booking, BatchBooking::Yes);
        assert_eq!(payment.import_id.as_deref(), Some("import"));
        assert_eq!(payment.reconciled_booking_oid, 840_000_001);
        assert_eq!(payment.payment_reference.as_deref(), Some("reference"));
        assert_eq!(payment.payee_verification_json.as_deref(), Some("{}"));
        assert_eq!(payment.sepa_status_code.as_deref(), Some("ACSC"));
        assert_eq!(
            payment.sepa_status,
            SepaStatus::AcceptedSettlementCompletedDebtor
        );
        assert_eq!(payment.sepa_reason_code.as_deref(), Some("AC01"));
        assert_eq!(
            payment.sepa_additional_info.as_deref(),
            Some("info one\ninfo two")
        );
        assert_eq!(
            payment.payee_verification_choice,
            PayeeVerificationChoice::OptOut
        );
    }

    #[test]
    fn the_authorisation_values_of_a_payment_are_dropped() {
        let payment = decode(&record(&[first_chunk(), second_chunk()]));
        let json = serde_json::to_value(&payment).expect("a payment serialises");
        let object = json.as_object().expect("a payment serialises as an object");
        assert!(!object.contains_key("used_tan"));
        assert!(!object.contains_key("order_reference_tan"));
        assert!(
            object
                .keys()
                .all(|key| key.split('_').all(|word| word != "tan")),
            "no key may name an authorisation value"
        );
        // Not under another name either, and not in the debug output.
        for text in [json.to_string(), format!("{payment:?}")] {
            assert!(!text.contains("TAN-USED-123456"));
            assert!(!text.contains("TAN-REFERENCE-654321"));
        }
        // The fields around them are in step.
        assert_eq!(object["import_id"], "import");
        assert_eq!(object["payment_reference"], "reference");
    }

    #[test]
    fn a_payee_verification_text_that_is_not_json_does_not_reject_the_payment() {
        let second = Payload::default()
            .u16(1)
            .u16(0)
            .i32(0)
            .i32(0)
            .i32(0)
            .i32(0)
            .string("")
            .u8(0)
            .string("")
            .string("")
            .ticks(0)
            .i32(0)
            .string("")
            .string("")
            .string("not json at all");
        let body = record(&[first_chunk(), second]);
        let mut reader = Reader::new(&body).expect("a well-formed test record");
        let payment = Payment::decode(&mut reader).expect("the text is kept, not parsed");
        assert_eq!(
            payment.payee_verification_json.as_deref(),
            Some("not json at all")
        );
        assert_eq!(payment.sepa_status, SepaStatus::None);
    }

    #[test]
    fn a_legacy_payment_takes_its_scheme_from_the_tagged_lines() {
        // One chunk: head with class sepa and the debit bit, then the
        // payment's own fields directly.
        let chunk = Payload::default()
            .i32(1)
            .i32(2)
            .u8(0x11)
            .cents(500)
            .code(3, "")
            .string("name")
            .string("")
            .string("")
            .code(2, "")
            .string("EREF+ref\nLCIC+INST\ntext")
            .date(0, 0, 0)
            .string("")
            .string("")
            .u16(1);
        let payment = decode(&record(&[chunk]));
        assert!(payment.base.legacy);
        assert_eq!(payment.base.stored_scheme_type, SchemeType::None);
        assert_eq!(
            payment.base.scheme_type,
            SchemeType::SepaInstantCreditTransfer
        );
        assert_eq!(payment.base.end_to_end_id.as_deref(), Some("ref"));
        assert_eq!(payment.base.purpose_lines, ["text"]);
        assert_eq!(payment.state, PaymentState::LOCAL);
        // No stored currency: the scheme implies one (04 §5.11).
        assert_eq!(payment.displayed_currency(Some("CHF")), Some("EUR"));
    }
}
