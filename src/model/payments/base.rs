//! The payment base shared by bookings, payments and standing orders
//! (04 §5).

use rust_decimal::Decimal;
use serde::Serialize;

use crate::model::base::{DecodeError, RecordBase};
use crate::wire::{text_lines, RawDate, Reader, TimeOfDay};

use super::enums::{CostBearer, SchemeClass, SchemeType};
use super::legacy;

/// The name Banking4 gives the built-in "do not evaluate" category in
/// German, the product's default language (04 §5.7 step 3).
const DO_NOT_EVALUATE: &str = "(nicht auswerten)";

/// The names of the pseudo categories "catalogue root", "automatic" and "not
/// set" in the three languages Banking4 ships (04 §5.7 step 4). A vault does
/// not record the language it was written under, so all of them count.
const PSEUDO_CATEGORIES: [&str; 9] = [
    "(Kategorienkatalog)",
    "(automatisch)",
    "(nicht gesetzt)",
    "(Catalog of Categories)",
    "(automatic)",
    "(not set)",
    "(Catalogue des catégories)",
    "(automatique)",
    "(non activé)",
];

/// The part of a category name that stands in for an empty level (04 §5.7
/// step 5).
const UNKNOWN_LEVEL: &str = "UNBEKANNT";

/// What the base reader does with the scheme type once the fields are read
/// (04 §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SchemeRule {
    /// A booking: no scheme, whatever is stored.
    AlwaysNone,
    /// A payment or standing order: a missing scheme is derived from the
    /// class, the direction and the local instrument.
    DeriveIfMissing,
}

/// The fields bookings, payments and standing orders have in common (04 §5):
/// an amount without sign, a direction, the other party, the purpose, the
/// category and the SEPA details.
///
/// The fields hold what the record stores, after the adjustments Banking4's
/// reader makes to a single record: the purpose text is cut into lines
/// (04 §5.6), the category name is normalised (04 §5.7), the tagged purpose
/// lines of a legacy record are moved into their fields (04 §5.8) and a
/// missing scheme is derived (04 §5.4). Where such an adjustment replaces a
/// stored number, a `stored_` field keeps it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaymentBase {
    /// Object id and update stamp.
    #[serde(flatten)]
    pub record: RecordBase,
    /// Whether the record is in the legacy single-chunk layout (04 §5.2). The
    /// fields from `counterparty_street` to `ultimate_counterparty_name`,
    /// the cost bearer and the foreign payment and bank address fields then
    /// come from tagged purpose lines.
    pub legacy: bool,
    /// The class nibble of the first byte as stored (04 §5.1). Banking4 does
    /// not keep it; see [`SchemeType::class`] for the class it would write.
    pub stored_scheme_class: SchemeClass,
    /// The direction bit as stored: set for a debit, money leaving the
    /// account. It decides the direction only without a scheme; use
    /// [`PaymentBase::is_debit`].
    pub debit_flag: bool,
    /// The amount without sign, as stored. Banking4 never stores a negative
    /// amount, but nothing checks it. See [`PaymentBase::signed_amount`].
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub amount: Decimal,
    /// ISO 4217 currency code as stored. What Banking4 shows can differ: the
    /// account's currency for a booking (04 §6.2), a scheme's currency for a
    /// payment without one (04 §5.11).
    pub currency: Option<String>,
    /// Name of the other party.
    pub counterparty_name: Option<String>,
    /// IBAN of the other party, or a national account number.
    pub counterparty_account: Option<String>,
    /// BIC of the other party's bank, or a national bank code.
    pub counterparty_bank_code: Option<String>,
    /// ISO 3166 country code of the other party.
    pub counterparty_country: Option<String>,
    /// The remittance information, one entry per line, without empty lines
    /// (04 §5.6). See [`PaymentBase::purpose`] for the text.
    pub purpose_lines: Vec<String>,
    /// Requested execution date, components as stored.
    pub scheduled_date: Option<RawDate>,
    /// Reference the bank assigned to a submitted order.
    pub order_id: Option<String>,
    /// Full category name, levels separated by `:`, normalised as Banking4
    /// does on reading (04 §5.7).
    pub category: Option<String>,
    /// Street and building number of the other party.
    pub counterparty_street: Option<String>,
    /// Post code and town of the other party.
    pub counterparty_town: Option<String>,
    /// SEPA end-to-end reference.
    pub end_to_end_id: Option<String>,
    /// SEPA payment information id (batch reference).
    pub payment_info_id: Option<String>,
    /// SEPA purpose code.
    pub purpose_code: Option<String>,
    /// SEPA category purpose code.
    pub category_purpose_code: Option<String>,
    /// SEPA direct debit mandate reference.
    pub mandate_id: Option<String>,
    /// Date the mandate was signed, components as stored.
    pub mandate_signed_date: Option<RawDate>,
    /// SEPA creditor identifier.
    pub creditor_id: Option<String>,
    /// SEPA local instrument (`CORE`, `B2B`, `INST`, ...).
    pub local_instrument_code: Option<String>,
    /// SEPA sequence type (`FRST`, `RCUR`, `OOFF`, `FNAL`).
    pub sequence_type: Option<String>,
    /// Ultimate creditor or debtor, if different from the other party.
    pub ultimate_counterparty_name: Option<String>,
    /// Who pays the charges of a foreign payment.
    pub cost_bearer: CostBearer,
    /// Object id of the account charged for the fees; 0 if none.
    pub cost_bearer_account_oid: i32,
    /// Payment method code of a foreign payment.
    pub intl_remittance_method: Option<String>,
    /// Free instruction text of a foreign payment.
    pub intl_remittance_instruction: Option<String>,
    /// Name of the other party's bank.
    pub counterparty_bank_name: Option<String>,
    /// Street of that bank.
    pub counterparty_bank_street: Option<String>,
    /// Town of that bank.
    pub counterparty_bank_town: Option<String>,
    /// The payment scheme Banking4 works with (04 §5.4): always none for a
    /// booking; for a payment or standing order the stored scheme, or the one
    /// derived from class, direction and local instrument if none is stored.
    pub scheme_type: SchemeType,
    /// The scheme type as stored, before 04 §5.4. None in a legacy record,
    /// which has no such field.
    pub stored_scheme_type: SchemeType,
    /// Requested execution time as stored. Banking4 reports it for instant
    /// credit transfers only; see [`PaymentBase::reported_scheduled_time`].
    pub scheduled_time: Option<TimeOfDay>,
    /// Regulatory reporting purpose code of a foreign payment.
    pub regulatory_reporting_code: Option<String>,
}

impl PaymentBase {
    /// Read the object header and the payment base (04 §4, §5.1 to §5.4). The
    /// reader is left in the first chunk; the record type then moves on with
    /// [`Reader::next_chunk`].
    pub(super) fn read(r: &mut Reader<'_>, rule: SchemeRule) -> Result<Self, DecodeError> {
        // 04 §5.1, in wire order.
        let record = RecordBase::read(r);
        let class_and_direction = r.u8();
        let amount = r.decimal("amount")?;
        let currency = r.code(3);
        let counterparty_name = r.string("counterparty_name")?;
        let counterparty_account = r.string("counterparty_account")?;
        let counterparty_bank_code = r.string("counterparty_bank_code")?;
        let counterparty_country = r.code(2);
        let purpose_text = r.string("purpose_text")?;
        let scheduled_date = r.date_raw();
        let order_id = r.string("order_id")?;
        let category = r.string("category")?;

        let mut base = Self {
            record,
            // 04 §5.2: the test is made here, after `category`, and nowhere
            // else. A record without a second chunk has no extension.
            legacy: !r.more_chunks(),
            stored_scheme_class: SchemeClass::from_raw(class_and_direction >> 4),
            debit_flag: class_and_direction & 1 != 0,
            amount,
            currency,
            counterparty_name,
            counterparty_account,
            counterparty_bank_code,
            counterparty_country,
            purpose_lines: text_lines(purpose_text.as_deref())
                .into_iter()
                .map(str::to_owned)
                .collect(),
            scheduled_date,
            order_id,
            category: normalise_category(category),
            counterparty_street: None,
            counterparty_town: None,
            end_to_end_id: None,
            payment_info_id: None,
            purpose_code: None,
            category_purpose_code: None,
            mandate_id: None,
            mandate_signed_date: None,
            creditor_id: None,
            local_instrument_code: None,
            sequence_type: None,
            ultimate_counterparty_name: None,
            cost_bearer: CostBearer::None,
            cost_bearer_account_oid: 0,
            intl_remittance_method: None,
            intl_remittance_instruction: None,
            counterparty_bank_name: None,
            counterparty_bank_street: None,
            counterparty_bank_town: None,
            scheme_type: SchemeType::None,
            stored_scheme_type: SchemeType::None,
            scheduled_time: None,
            regulatory_reporting_code: None,
        };
        if base.legacy {
            legacy::apply_tagged_lines(&mut base);
        } else {
            base.read_extension(r)?;
        }
        base.scheme_type = match rule {
            SchemeRule::AlwaysNone => SchemeType::None,
            SchemeRule::DeriveIfMissing if base.stored_scheme_type == SchemeType::None => {
                derive_scheme(
                    base.stored_scheme_class,
                    base.debit_flag,
                    base.local_instrument_code.as_deref(),
                )
            }
            SchemeRule::DeriveIfMissing => base.stored_scheme_type,
        };
        Ok(base)
    }

    /// Read the extension fields from the rest of the first chunk (04 §5.3),
    /// in wire order.
    fn read_extension(&mut self, r: &mut Reader<'_>) -> Result<(), DecodeError> {
        self.counterparty_street = r.string("counterparty_street")?;
        self.counterparty_town = r.string("counterparty_town")?;
        self.end_to_end_id = r.string("end_to_end_id")?;
        self.payment_info_id = r.string("payment_info_id")?;
        self.purpose_code = r.string("purpose_code")?;
        self.category_purpose_code = r.string("category_purpose_code")?;
        self.mandate_id = r.string("mandate_id")?;
        self.mandate_signed_date = r.date_raw();
        self.creditor_id = r.string("creditor_id")?;
        self.local_instrument_code = r.string("local_instrument_code")?;
        self.sequence_type = r.string("sequence_type")?;
        self.ultimate_counterparty_name = r.string("ultimate_counterparty_name")?;
        self.cost_bearer = CostBearer::from_raw(r.u8());
        self.cost_bearer_account_oid = r.i32();
        self.intl_remittance_method = r.string("intl_remittance_method")?;
        self.intl_remittance_instruction = r.string("intl_remittance_instruction")?;
        self.counterparty_bank_name = r.string("counterparty_bank_name")?;
        self.counterparty_bank_street = r.string("counterparty_bank_street")?;
        self.counterparty_bank_town = r.string("counterparty_bank_town")?;
        self.stored_scheme_type = SchemeType::from_raw(r.i32());
        self.scheduled_time = r.time_hms("scheduled_time")?;
        self.regulatory_reporting_code = r.string("regulatory_reporting_code")?;
        Ok(())
    }

    /// The effective direction (04 §5.5): `true` for a debit, money leaving
    /// the account. With a scheme the scheme decides and the stored bit is
    /// ignored; without one the stored bit decides.
    #[must_use]
    pub fn is_debit(&self) -> bool {
        self.scheme_type.debit().unwrap_or(self.debit_flag)
    }

    /// The amount with the sign Banking4 works with (04 §5.5): negative when
    /// money leaves the account. The scale of the stored amount is kept.
    #[must_use]
    pub fn signed_amount(&self) -> Decimal {
        // A debit of nothing stays a plain zero: negating it would give a
        // zero that prints with a minus sign.
        if self.is_debit() && !self.amount.is_zero() {
            -self.amount
        } else {
            self.amount
        }
    }

    /// The purpose as one text, lines joined with a line feed, or `None`
    /// without lines. This is what Banking4 writes back (04 §5.6).
    #[must_use]
    pub fn purpose(&self) -> Option<String> {
        (!self.purpose_lines.is_empty()).then(|| self.purpose_lines.join("\n"))
    }

    /// The execution time Banking4 reports (04 §5.3 position 32): the stored
    /// one for an instant credit transfer, none for every other scheme.
    #[must_use]
    pub fn reported_scheduled_time(&self) -> Option<TimeOfDay> {
        self.scheduled_time
            .filter(|_| self.scheme_type == SchemeType::SepaInstantCreditTransfer)
    }

    /// The currency Banking4 reports for a payment or standing order
    /// (04 §5.11): the stored one, or else the currency its scheme implies.
    /// See [`Payment::displayed_currency`](super::Payment::displayed_currency).
    pub(super) fn currency_or_scheme_default<'a>(
        &'a self,
        account_currency: Option<&'a str>,
    ) -> Option<&'a str> {
        if let Some(currency) = &self.currency {
            return Some(currency);
        }
        match self.scheme_type {
            SchemeType::UserExpense | SchemeType::UserIncome => account_currency,
            SchemeType::DomesticLegacyCreditTransfer => Some("DEM"),
            SchemeType::SepaCreditTransfer
            | SchemeType::SepaDirectDebitCore
            | SchemeType::SepaDirectDebitB2b
            | SchemeType::SepaInstantCreditTransfer => Some("EUR"),
            _ => None,
        }
    }
}

/// The scheme of a payment or standing order that stores none (04 §5.4).
/// The local instrument is compared exactly: `inst` or `INST ` do not make
/// an instant transfer.
fn derive_scheme(class: SchemeClass, debit: bool, local_instrument: Option<&str>) -> SchemeType {
    match (class, debit) {
        (SchemeClass::Any, true) => SchemeType::UserExpense,
        (SchemeClass::Any, false) => SchemeType::UserIncome,
        (SchemeClass::Sepa, true) if local_instrument == Some("INST") => {
            SchemeType::SepaInstantCreditTransfer
        }
        (SchemeClass::Sepa, true) => SchemeType::SepaCreditTransfer,
        (SchemeClass::Sepa, false) if local_instrument == Some("B2B") => {
            SchemeType::SepaDirectDebitB2b
        }
        (SchemeClass::Sepa, false) => SchemeType::SepaDirectDebitCore,
        (SchemeClass::ForeignLegacy, true) => SchemeType::ForeignLegacyCreditTransfer,
        (SchemeClass::DomesticLegacy, true) => SchemeType::DomesticLegacyCreditTransfer,
        _ => SchemeType::None,
    }
}

/// Normalise a stored category name the way Banking4 does on reading
/// (04 §5.7): trim, turn a blank name into the "do not evaluate" category,
/// drop the pseudo categories, and repair empty levels.
///
/// The writer stores names as they are, so this is needed for records of a
/// current version too.
fn normalise_category(stored: Option<String>) -> Option<String> {
    let stored = stored?;
    let name = stored.trim();
    if name.is_empty() {
        return Some(DO_NOT_EVALUATE.to_owned());
    }
    if PSEUDO_CATEGORIES.contains(&name) {
        return None;
    }
    let is_tidy = name.len() == stored.len()
        && name
            .split(':')
            .all(|level| !level.is_empty() && level.trim().len() == level.len());
    if is_tidy {
        // The usual case: nothing to repair, keep the allocation.
        return Some(stored);
    }
    let levels: Vec<&str> = name
        .split(':')
        .map(|level| match level.trim() {
            "" => UNKNOWN_LEVEL,
            level => level,
        })
        .collect();
    Some(levels.join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalised(stored: &str) -> Option<String> {
        normalise_category(Some(stored.to_owned()))
    }

    #[test]
    fn category_names_are_trimmed_and_repaired() {
        assert_eq!(normalise_category(None), None);
        assert_eq!(normalised("Wohnen:Miete").as_deref(), Some("Wohnen:Miete"));
        assert_eq!(
            normalised("  Wohnen : :Strom  ").as_deref(),
            Some("Wohnen:UNBEKANNT:Strom")
        );
        assert_eq!(normalised(":").as_deref(), Some("UNBEKANNT:UNBEKANNT"));
        assert_eq!(normalised("A:").as_deref(), Some("A:UNBEKANNT"));
        // U+00A0 and U+0085 are white space, U+001F is not (04 §5.7 step 2).
        assert_eq!(normalised("\u{a0}A\u{85}").as_deref(), Some("A"));
        assert_eq!(normalised("\u{1f}A").as_deref(), Some("\u{1f}A"));
    }

    #[test]
    fn a_blank_category_becomes_do_not_evaluate() {
        assert_eq!(normalised("   ").as_deref(), Some("(nicht auswerten)"));
        assert_eq!(normalised("\t\u{a0}").as_deref(), Some("(nicht auswerten)"));
    }

    #[test]
    fn pseudo_categories_of_every_language_are_no_category() {
        for name in PSEUDO_CATEGORIES {
            assert_eq!(normalised(name), None, "{name}");
            assert_eq!(normalised(&format!("  {name} ")), None, "{name}");
        }
        // Exact and case-sensitive; the real "do not evaluate" category stays.
        assert_eq!(
            normalised("(Automatisch)").as_deref(),
            Some("(Automatisch)")
        );
        assert_eq!(
            normalised("(nicht auswerten)").as_deref(),
            Some("(nicht auswerten)")
        );
    }

    #[test]
    fn a_missing_scheme_is_derived_from_class_direction_and_instrument() {
        use SchemeClass::{Any, DomesticLegacy, ForeignLegacy, Sepa};
        let cases = [
            (Any, true, None, SchemeType::UserExpense),
            (Any, false, None, SchemeType::UserIncome),
            (Sepa, true, None, SchemeType::SepaCreditTransfer),
            (
                Sepa,
                true,
                Some("INST"),
                SchemeType::SepaInstantCreditTransfer,
            ),
            (Sepa, true, Some("inst"), SchemeType::SepaCreditTransfer),
            (Sepa, true, Some("INST "), SchemeType::SepaCreditTransfer),
            (Sepa, true, Some("B2B"), SchemeType::SepaCreditTransfer),
            (Sepa, false, None, SchemeType::SepaDirectDebitCore),
            (Sepa, false, Some("CORE"), SchemeType::SepaDirectDebitCore),
            (Sepa, false, Some("B2B"), SchemeType::SepaDirectDebitB2b),
            (Sepa, false, Some("INST"), SchemeType::SepaDirectDebitCore),
            (
                ForeignLegacy,
                true,
                None,
                SchemeType::ForeignLegacyCreditTransfer,
            ),
            (ForeignLegacy, false, None, SchemeType::None),
            (
                DomesticLegacy,
                true,
                None,
                SchemeType::DomesticLegacyCreditTransfer,
            ),
            (DomesticLegacy, false, None, SchemeType::None),
            (SchemeClass::Unknown(4), true, None, SchemeType::None),
            (SchemeClass::Unknown(15), false, None, SchemeType::None),
        ];
        for (class, debit, instrument, expected) in cases {
            assert_eq!(
                derive_scheme(class, debit, instrument),
                expected,
                "{class} {debit} {instrument:?}"
            );
        }
    }
}
