//! Payment counterparties: payees (05 §3.1), payers (05 §3.2) and mandates
//! (05 §3.4).

use rust_decimal::Decimal;
use serde::Serialize;

use super::category::normalise_category;
use crate::model::base::{code_enum, flag_set, DecodeError, Record, RecordBase};
use crate::wire::{RawDate, Reader, Timestamp};

/// The `mandate_id` of a mandate Banking4's list does not show (05 §3.4).
const OFFLINE_MANDATE_ID: &str = "OFFLINE";

// ------------------------------------------------------------ enumerations

code_enum! {
    /// What kind of entry a payee is (05 §6.1). The values are single bits
    /// like those of a flag set, but Banking4 uses one value per record.
    pub enum PayeeKind: u16 {
        None = 0 => "none",
        /// An ordinary entry of the payee list.
        Payee = 1 => "payee",
        /// Apparently a legacy value, from the time mandates were stored with
        /// the payee (05 §3.1.2).
        LegacyMandate = 2 => "legacy_mandate",
        /// Apparently an entry Banking4 added on its own.
        Auto = 4 => "auto",
        /// Apparently an entry standing for one of the user's own accounts.
        /// Banking4 replaces it by `none` when it reads the record.
        Account = 8 => "account",
    }
}

impl PayeeKind {
    /// The kind Banking4 keeps after reading (05 §3.1 "After reading"):
    /// `none`, `payee`, `legacy_mandate` and `auto` stay, every other value
    /// becomes `none`.
    #[must_use]
    pub const fn after_reading(self) -> Self {
        match self {
            Self::None | Self::Payee | Self::LegacyMandate | Self::Auto => self,
            Self::Account | Self::Unknown(_) => Self::None,
        }
    }
}

flag_set! {
    /// Flags of a payee (05 §6.2).
    pub struct PayeeFlags: u16 {
        /// The only flag; apparently unsupported.
        VOP_NO_MATCH_UNSUPPORTED = 0x01 => "vop_no_match_unsupported",
    }
}

code_enum! {
    /// Result of a verification-of-payee request (05 §6.3).
    pub enum VopStatus: u8 {
        /// Never verified: what an unset field holds.
        None = 0 => "none",
        Match = 1 => "match",
        CloseMatch = 2 => "close_match",
        NoMatch = 3 => "no_match",
        NotApplicable = 4 => "not_applicable",
        Pending = 5 => "pending",
    }
}

flag_set! {
    /// Flags of a mandate (05 §6.8).
    pub struct MandateFlags: i32 {
        HIDDEN = 0x0001 => "hidden",
        REVOKED = 0x0100 => "revoked",
    }
}

// ------------------------------------------------------------------- payee

/// Split a stored address into its street line and its town line
/// (05 §3.1.1).
///
/// A text without a line feed is the street line as it is, untrimmed.
/// Otherwise the text is cut at the first line feed and both parts are
/// trimmed; a part that is empty after trimming is `None`, and further line
/// feeds stay in the town line.
#[must_use]
pub fn split_address(address: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(address) = address else {
        return (None, None);
    };
    match address.split_once('\n') {
        None => (Some(address.to_owned()), None),
        Some((street, town)) => {
            let line = |part: &str| {
                let part = part.trim();
                (!part.is_empty()).then(|| part.to_owned())
            };
            (line(street), line(town))
        }
    }
}

/// One record of `Payees` (05 §3.1): an entry of the address book of payment
/// counterparties, also usable as a payment template.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Payee {
    /// Object id and update stamp.
    #[serde(flatten)]
    pub base: RecordBase,
    /// Name of the payee.
    pub name: Option<String>,
    /// National account number.
    pub account_number: Option<String>,
    /// National bank code.
    pub bank_code: Option<String>,
    /// Default remittance text.
    pub purpose: Option<String>,
    /// Default category, as a normalised full name (05 §4.2).
    pub category: Option<String>,
    /// IBAN. Several payees can share one.
    pub iban: Option<String>,
    /// BIC.
    pub bic: Option<String>,
    /// Free user notes.
    pub notes: Option<String>,
    /// Street line of the postal address (first line of the stored
    /// `address`, 05 §3.1.1).
    pub street: Option<String>,
    /// Town line of the postal address.
    pub town: Option<String>,
    /// ISO 3166 country code.
    pub country: Option<String>,
    /// Name of the payee's bank.
    pub bank_name: Option<String>,
    /// Street line of the bank's address (stored `bank_address`).
    pub bank_street: Option<String>,
    /// Town line of the bank's address.
    pub bank_town: Option<String>,
    /// Legacy SEPA mandate reference (05 §3.1.2). Normally empty.
    pub mandate_id: Option<String>,
    /// Legacy mandate signature date.
    pub mandate_signature_date: Option<RawDate>,
    /// Currency of the default amount.
    pub currency: Option<String>,
    /// Default amount, without a direction.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub amount: Decimal,
    /// E-mail address.
    pub email: Option<String>,
    /// Customer number at the payee.
    pub customer_number: Option<String>,
    /// Kind of entry after the reader's filter ([`PayeeKind::after_reading`]).
    pub payee_kind: PayeeKind,
    /// Kind of entry as stored.
    pub stored_payee_kind: PayeeKind,
    /// Legacy mandate: SEPA local instrument code.
    pub local_instrument_code: Option<String>,
    /// Legacy mandate revocation date.
    pub mandate_revocation_date: Option<RawDate>,
    /// Flags.
    pub payee_flags: PayeeFlags,
    /// Last verification-of-payee result.
    pub vop_status: VopStatus,
    /// Name reported back by verification of payee.
    pub vop_matched_name: Option<String>,
}

impl Record for Payee {
    const NAME: &'static str = "payee";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let base = RecordBase::read(r);
        let name = r.string("name")?;
        let account_number = r.string("account_number")?;
        let bank_code = r.string("bank_code")?;
        let purpose = r.string("purpose")?;
        let category = normalise_category(r.string("category")?.as_deref());
        let iban = r.string("iban")?;
        let bic = r.string("bic")?;
        let notes = r.string("notes")?;
        let (street, town) = split_address(r.string("address")?.as_deref());
        let country = r.code(2);
        let bank_name = r.string("bank_name")?;
        let (bank_street, bank_town) = split_address(r.string("bank_address")?.as_deref());
        let mandate_id = r.string("mandate_id")?;
        let mandate_signature_date = r.date_raw();
        r.skip_string("reserved_text")?;
        let currency = r.code(3);
        let amount = r.decimal("amount")?;
        let email = r.string("email")?;
        let customer_number = r.string("customer_number")?;
        let stored_payee_kind = PayeeKind::from_raw(r.u16());
        let local_instrument_code = r.string("local_instrument_code")?;
        let mandate_revocation_date = r.date_raw();
        let payee_flags = PayeeFlags::from_raw(r.u16());
        let vop_status = VopStatus::from_raw(r.u8());
        let vop_matched_name = r.string("vop_matched_name")?;
        Ok(Self {
            base,
            name,
            account_number,
            bank_code,
            purpose,
            category,
            iban,
            bic,
            notes,
            street,
            town,
            country,
            bank_name,
            bank_street,
            bank_town,
            mandate_id,
            mandate_signature_date,
            currency,
            amount,
            email,
            customer_number,
            payee_kind: stored_payee_kind.after_reading(),
            stored_payee_kind,
            local_instrument_code,
            mandate_revocation_date,
            payee_flags,
            vop_status,
            vop_matched_name,
        })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

impl Payee {
    /// Whether the payee still carries a legacy mandate reference. Banking4
    /// turns such a payee into a payer record after loading and clears the
    /// mandate fields (05 §3.1.2); a reader can treat it as a payee plus an
    /// implied payer.
    #[must_use]
    pub const fn has_legacy_mandate(&self) -> bool {
        self.mandate_id.is_some()
    }
}

// ------------------------------------------------------------------- payer

/// One record of `Payers` (05 §3.2): a debtor the user collects from by
/// direct debit. Name, address and bank details are those of the payee
/// record `party_oid` refers to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Payer {
    /// Object id and update stamp.
    #[serde(flatten)]
    pub base: RecordBase,
    /// `oid` of the payee record that holds the payer's details.
    pub party_oid: i32,
    /// Copy of the party's name.
    pub party_name: Option<String>,
    /// SEPA mandate reference.
    pub mandate_id: Option<String>,
    /// Date the mandate was signed.
    pub mandate_signature_date: Option<RawDate>,
    /// Date the mandate was revoked.
    pub mandate_revocation_date: Option<RawDate>,
    /// SEPA local instrument code.
    pub local_instrument_code: Option<String>,
    /// Free user notes.
    pub notes: Option<String>,
    /// Default category, as a normalised full name (05 §4.2).
    pub category: Option<String>,
}

impl Record for Payer {
    const NAME: &'static str = "payer";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            base: RecordBase::read(r),
            party_oid: r.i32(),
            party_name: r.string("party_name")?,
            mandate_id: r.string("mandate_id")?,
            mandate_signature_date: r.date_raw(),
            mandate_revocation_date: r.date_raw(),
            local_instrument_code: r.string("local_instrument_code")?,
            notes: r.string("notes")?,
            category: normalise_category(r.string("category")?.as_deref()),
        })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

// ----------------------------------------------------------------- mandate

/// Why Banking4's mandate list does not show a record (05 §3.4 "Loader",
/// not confirmed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MandateDropReason {
    /// `mandate_id` or `creditor_id` has no value.
    MissingId,
    /// `mandate_id` is exactly `OFFLINE`.
    Offline,
}

/// One record of `Mandates` (05 §3.4): a direct debit mandate under which a
/// creditor debits one of the user's accounts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mandate {
    /// Object id and update stamp.
    #[serde(flatten)]
    pub base: RecordBase,
    /// SEPA creditor identifier of the party that debits the account.
    pub creditor_id: Option<String>,
    /// SEPA mandate reference.
    pub mandate_id: Option<String>,
    /// Category for bookings under this mandate, as a normalised full name
    /// (05 §4.2).
    pub category: Option<String>,
    /// Name of the creditor.
    pub creditor_name: Option<String>,
    /// Flags.
    pub mandate_flags: MandateFlags,
    /// A date associated with the mandate; which one is not established
    /// (05 §9 question 2). No time zone.
    pub mandate_date: Option<Timestamp>,
}

impl Record for Mandate {
    const NAME: &'static str = "mandate";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            base: RecordBase::read(r),
            creditor_id: r.string("creditor_id")?,
            mandate_id: r.string("mandate_id")?,
            category: normalise_category(r.string("category")?.as_deref()),
            creditor_name: r.string("creditor_name")?,
            mandate_flags: MandateFlags::from_raw(r.i32()),
            mandate_date: r.ticks("mandate_date")?,
        })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

impl Mandate {
    /// Why Banking4's list would drop this record while loading, by the two
    /// rules of 05 §3.4 that are fully specified; `None` if neither applies.
    ///
    /// Banking4 has a third rule that is **not** replicated: it also drops
    /// mandates of one particular creditor identifier that is built into
    /// Banking4 and not given in the format description. A record for which
    /// this returns `None` can therefore still be hidden in Banking4.
    #[must_use]
    pub fn loader_drop_reason(&self) -> Option<MandateDropReason> {
        match (self.mandate_id.as_deref(), self.creditor_id.as_deref()) {
            (None, _) | (_, None) => Some(MandateDropReason::MissingId),
            (Some(OFFLINE_MANDATE_ID), Some(_)) => Some(MandateDropReason::Offline),
            (Some(_), Some(_)) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerations_have_the_names_of_the_document() {
        let kinds: Vec<String> = [0, 1, 2, 4, 8, 3, 0xFFFF]
            .into_iter()
            .map(|raw| PayeeKind::from_raw(raw).to_string())
            .collect();
        assert_eq!(
            kinds,
            [
                "none",
                "payee",
                "legacy_mandate",
                "auto",
                "account",
                "3",
                "65535"
            ]
        );
        let status: Vec<String> = (0..=6)
            .map(|raw| VopStatus::from_raw(raw).to_string())
            .collect();
        assert_eq!(
            status,
            [
                "none",
                "match",
                "close_match",
                "no_match",
                "not_applicable",
                "pending",
                "6"
            ]
        );
        assert_eq!(
            PayeeFlags::from_raw(0xFFFF).to_string(),
            "vop_no_match_unsupported,0xfffe"
        );
        assert_eq!(MandateFlags::from_raw(0x101).to_string(), "hidden,revoked");
        assert_eq!(MandateFlags::from_raw(0x100).to_string(), "revoked");
        assert_eq!(MandateFlags::from_raw(2).to_string(), "0x2");
        assert!(MandateFlags::from_raw(-1).contains(MandateFlags::HIDDEN));
    }

    #[test]
    fn payee_kind_keeps_four_values_after_reading() {
        for kept in [0, 1, 2, 4] {
            let kind = PayeeKind::from_raw(kept);
            assert_eq!(kind.after_reading(), kind);
        }
        for replaced in [3, 5, 8, 9, 16, 0x8000, 0xFFFF] {
            assert_eq!(
                PayeeKind::from_raw(replaced).after_reading(),
                PayeeKind::None,
                "{replaced}"
            );
        }
    }

    #[test]
    fn address_is_cut_at_the_first_line_feed() {
        let split = |text| {
            let (street, town) = split_address(text);
            (street, town)
        };
        let some = |text: &str| Some(text.to_owned());
        assert_eq!(split(None), (None, None));
        // No line feed: the whole text, untrimmed.
        assert_eq!(
            split(Some(" Main Street 1 ")),
            (some(" Main Street 1 "), None)
        );
        assert_eq!(
            split(Some(" Main Street 1 \n 12345 Town ")),
            (some("Main Street 1"), some("12345 Town"))
        );
        assert_eq!(split(Some("\n12345 Town")), (None, some("12345 Town")));
        assert_eq!(
            split(Some("Main Street 1\n")),
            (some("Main Street 1"), None)
        );
        assert_eq!(split(Some("\n")), (None, None));
        assert_eq!(split(Some(" \n ")), (None, None));
        // Further line feeds stay in the town line.
        assert_eq!(
            split(Some("Street\nBuilding 2\n12345 Town")),
            (some("Street"), some("Building 2\n12345 Town"))
        );
        // A carriage return is white space at the end of a part, nothing more.
        assert_eq!(
            split(Some("Street\r\nTown")),
            (some("Street"), some("Town"))
        );
    }

    fn mandate(mandate_id: Option<&str>, creditor_id: Option<&str>) -> Mandate {
        Mandate {
            base: RecordBase {
                oid: 1,
                update_stamp: 2,
            },
            creditor_id: creditor_id.map(str::to_owned),
            mandate_id: mandate_id.map(str::to_owned),
            category: None,
            creditor_name: None,
            mandate_flags: MandateFlags::default(),
            mandate_date: None,
        }
    }

    #[test]
    fn mandates_without_ids_and_offline_mandates_are_marked() {
        assert_eq!(
            mandate(Some("M1"), Some("DE98ZZZ")).loader_drop_reason(),
            None
        );
        assert_eq!(
            mandate(None, Some("DE98ZZZ")).loader_drop_reason(),
            Some(MandateDropReason::MissingId)
        );
        assert_eq!(
            mandate(Some("M1"), None).loader_drop_reason(),
            Some(MandateDropReason::MissingId)
        );
        assert_eq!(
            mandate(None, None).loader_drop_reason(),
            Some(MandateDropReason::MissingId)
        );
        assert_eq!(
            mandate(Some("OFFLINE"), Some("DE98ZZZ")).loader_drop_reason(),
            Some(MandateDropReason::Offline)
        );
        // An offline mandate without a creditor lacks an id first.
        assert_eq!(
            mandate(Some("OFFLINE"), None).loader_drop_reason(),
            Some(MandateDropReason::MissingId)
        );
        // The comparison is exact.
        for other in ["offline", "OFFLINE ", " OFFLINE", "OFFLINE1"] {
            assert_eq!(
                mandate(Some(other), Some("DE98ZZZ")).loader_drop_reason(),
                None
            );
        }
    }
}
