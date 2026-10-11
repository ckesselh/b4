//! Per-account lists besides bookings and holdings: tax exemptions
//! (05 §3.10) and assets (05 §3.11).

use rust_decimal::Decimal;
use serde::Serialize;

use crate::model::base::{DecodeError, Record, RecordBase};
use crate::wire::{RawDate, Reader, Timestamp};

/// One record of `Acct<oid>/AcctTaxExemptions` (05 §3.10): a tax exemption
/// order of an account, with the part already used. Which account is the
/// `<oid>` of the directory.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaxExemption {
    /// Object id and update stamp.
    #[serde(flatten)]
    pub base: RecordBase,
    /// First day the order is valid. No time zone; probably a date at
    /// midnight (05 §9 question 1).
    pub valid_from: Option<Timestamp>,
    /// Last day the order is valid. No time zone.
    pub valid_until: Option<Timestamp>,
    /// Exempted amount.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub amount: Decimal,
    /// Part of the amount already used.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub used_amount: Decimal,
    /// Part of the amount still available. Stored, not derived from the two
    /// fields before.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub remaining_amount: Decimal,
}

impl Record for TaxExemption {
    const NAME: &'static str = "tax exemption";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            base: RecordBase::read(r),
            valid_from: r.ticks("valid_from")?,
            valid_until: r.ticks("valid_until")?,
            amount: r.decimal("amount")?,
            used_amount: r.decimal("used_amount")?,
            remaining_amount: r.decimal("remaining_amount")?,
        })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

/// One record of `Acct<oid>/Assets` (05 §3.11): an asset of an asset
/// account, for example a term deposit. Which account is the `<oid>` of the
/// directory.
///
/// The file stores one signed amount. Banking4 keeps it as an amount without
/// sign and a direction, and so does this struct;
/// [`Asset::signed_amount`] gives the stored value back.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Asset {
    /// Object id and update stamp.
    #[serde(flatten)]
    pub base: RecordBase,
    /// Identifier of the asset.
    pub asset_id: Option<String>,
    /// The amount without its sign (05 §3.11.1).
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub amount: Decimal,
    /// Whether the stored amount is below zero, which marks a debit
    /// (a liability). A stored negative zero is not below zero.
    pub debit: bool,
    /// ISO 4217 currency code.
    pub currency: Option<String>,
    /// Start date of the deposit, as stored.
    pub deposit_date: Option<RawDate>,
    /// End date of the deposit, as stored.
    pub expiry_date: Option<RawDate>,
    /// Interest rate.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub interest_rate: Decimal,
    /// One-character code of the interest method; its values are not
    /// established (05 §9 question 4).
    pub interest_method: Option<String>,
    /// Description of the asset.
    pub description: Option<String>,
}

impl Record for Asset {
    const NAME: &'static str = "asset";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let base = RecordBase::read(r);
        let asset_id = r.string("asset_id")?;
        let signed_amount = r.decimal("signed_amount")?;
        // 05 §3.11.1: below zero means debit, and the amount is the
        // negation. Zero, with or without a sign bit, is not below zero.
        let debit = signed_amount.is_sign_negative() && !signed_amount.is_zero();
        Ok(Self {
            base,
            asset_id,
            amount: signed_amount.abs(),
            debit,
            currency: r.code(3),
            deposit_date: r.date_raw(),
            expiry_date: r.date_raw(),
            interest_rate: r.decimal("interest_rate")?,
            interest_method: r.code(1),
            description: r.string("description")?,
        })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

impl Asset {
    /// The amount with its sign, as the file stores it: negative for a
    /// debit (05 §3.11.1).
    #[must_use]
    pub fn signed_amount(&self) -> Decimal {
        if self.debit {
            -self.amount
        } else {
            self.amount
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_amount_puts_the_direction_back() {
        let asset = |amount: &str, debit| Asset {
            base: RecordBase {
                oid: 1,
                update_stamp: 2,
            },
            asset_id: None,
            amount: amount.parse().unwrap(),
            debit,
            currency: None,
            deposit_date: None,
            expiry_date: None,
            interest_rate: Decimal::ZERO,
            interest_method: None,
            description: None,
        };
        assert_eq!(
            asset("2500.00", true).signed_amount().to_string(),
            "-2500.00"
        );
        assert_eq!(
            asset("2500.00", false).signed_amount().to_string(),
            "2500.00"
        );
        assert_eq!(asset("0.00", false).signed_amount().to_string(), "0.00");
    }
}
