//! Holdings and movements of a portfolio account: portfolio items (05 §3.8)
//! and portfolio transactions (05 §3.9).

use rust_decimal::Decimal;
use serde::Serialize;

use super::security_id::{parse_security_id, SecurityId, SecurityIdKind};
use super::stock::{instrument_for_isin, StockInstrument};
use crate::model::base::{code_enum, flag_set, DecodeError, Record, RecordBase};
use crate::wire::{text_lines, RawDate, Reader, TimeOfDay, Timestamp};

/// The currency Banking4 falls back to where a record needs one and stores
/// none (05 §3.8.1 rule 5).
///
/// In Banking4 this is the default currency of the installation, which a
/// vault does not record; `EUR` is its normal value (05 §8.1). The one field
/// it can end up in, [`PortfolioItem::purchase_price_currency`], has its
/// stored value next to it.
pub const APPLICATION_DEFAULT_CURRENCY: &str = "EUR";

// ------------------------------------------------------------ enumerations

flag_set! {
    /// Where a portfolio item comes from and whether it is deleted
    /// (05 §6.9).
    pub struct PortfolioItemState: i32 {
        /// Entered by the user.
        LOCAL = 0x00001 => "local",
        /// Reported by the bank.
        BANK = 0x00002 => "bank",
        DELETED = 0x10000 => "deleted",
    }
}

code_enum! {
    /// What a quantity counts (05 §6.10); the ISO 15022 quantity type.
    pub enum QuantityType: i32 {
        Undefined = 0 => "undefined",
        /// A nominal amount in `quantity_currency` (`FAMT`).
        FaceAmount = 1 => "face_amount",
        /// A number of units (`UNIT`).
        Units = 2 => "units",
    }
}

code_enum! {
    /// What a price means (05 §6.11, §5.3); the ISO 15022 price type.
    pub enum PriceType: i32 {
        Undefined = 0 => "undefined",
        /// An amount per unit (`ACTU`).
        Actual = 1 => "actual",
        /// A percentage of the face amount (`PRCT`).
        Percent = 2 => "percent",
        /// A yield (`YIEL`); Banking4 computes no value from it.
        Yield = 3 => "yield",
    }
}

code_enum! {
    /// State of a portfolio transaction (05 §6.12).
    pub enum PortfolioTransactionState: i32 {
        /// What an unset field holds.
        None = 0 => "none",
        Pending = 1 => "pending",
        Executed = 2 => "executed",
        Settled = 3 => "settled",
    }
}

code_enum! {
    /// Which way the securities of a transaction move (05 §6.13).
    pub enum DeliveryDirection: i32 {
        /// What an unset field holds.
        None = 0 => "none",
        /// The securities leave the portfolio (`DELI`).
        Deliver = 1 => "deliver",
        /// The securities enter the portfolio (`RECE`).
        Receive = 2 => "receive",
    }
}

code_enum! {
    /// The kind of business behind a portfolio transaction (05 §6.14).
    pub enum TransactionActivity: i32 {
        /// What an unset field holds.
        None = 0 => "none",
        BorrowingLending = 1 => "borrowing_lending",
        Collateral = 2 => "collateral",
        CorporateAction = 3 => "corporate_action",
        /// An ordinary trade settlement.
        Settlement = 4 => "settlement",
    }
}

// ---------------------------------------------------------- portfolio item

/// One record of `Acct<oid>/Portf` (05 §3.8): a position of a portfolio
/// account. Which account is the `<oid>` of the directory; the record does
/// not say.
///
/// On reading, Banking4 replaces a negative price or rate by 0 and adjusts
/// two currencies (05 §3.8.1). The fields named in the document hold those
/// adjusted values; each has a `stored_*` field with what the file holds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PortfolioItem {
    /// Object id and update stamp.
    #[serde(flatten)]
    pub base: RecordBase,
    /// Origin and deletion state.
    pub item_state: PortfolioItemState,
    /// The canonical ISIN of the stored identifier (05 §5.1), `None` if it
    /// has none. It is what links the item to its stock instrument
    /// ([`PortfolioItem::instrument`]).
    pub isin: Option<String>,
    /// The security identifier as stored, normally an ISIN.
    pub stored_isin: Option<String>,
    /// What kind of identifier `stored_isin` is.
    pub isin_kind: SecurityIdKind,
    /// Name of the security, with every line feed replaced by a space.
    pub name: Option<String>,
    /// Maturity date (bonds), as stored.
    pub maturity_date: Option<RawDate>,
    /// Interest rate; 0 where a negative value is stored.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub interest_rate: Decimal,
    /// `interest_rate` as stored.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub stored_interest_rate: Decimal,
    /// Key text of the position; what it identifies is not established
    /// (05 §9 question 3).
    pub portfolio_key: Option<String>,
    /// Holding size: units or a face amount, depending on `quantity_type`.
    /// Not guarded against negative values.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub quantity: Decimal,
    /// What `quantity` counts.
    pub quantity_type: QuantityType,
    /// Currency of a face-amount quantity; `None` when the quantity counts
    /// units, whatever is stored.
    pub quantity_currency: Option<String>,
    /// `quantity_currency` as stored.
    pub stored_quantity_currency: Option<String>,
    /// What the two prices mean.
    pub price_type: PriceType,
    /// Currency of `current_price`.
    pub current_price_currency: Option<String>,
    /// Purchase price per unit, or percent; 0 where a negative value is
    /// stored.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub purchase_price: Decimal,
    /// `purchase_price` as stored.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub stored_purchase_price: Decimal,
    /// Date of the purchase price, as stored.
    pub purchase_price_date: Option<RawDate>,
    /// Latest known price per unit, or percent; 0 where a negative value is
    /// stored.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub current_price: Decimal,
    /// `current_price` as stored.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub stored_current_price: Decimal,
    /// Date of the latest price, as stored.
    pub current_price_date: Option<RawDate>,
    /// Time of the latest price.
    pub current_price_time: Option<TimeOfDay>,
    /// Currency of `purchase_price`. Where none is stored for a positive
    /// price per unit, it is `current_price_currency`, or
    /// [`APPLICATION_DEFAULT_CURRENCY`] if that is missing too
    /// (05 §3.8.1 rule 5).
    pub purchase_price_currency: Option<String>,
    /// `purchase_price_currency` as stored.
    pub stored_purchase_price_currency: Option<String>,
    /// Price below which the user wants to be notified; 0 where a negative
    /// value is stored.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub alert_lower_limit: Decimal,
    /// `alert_lower_limit` as stored.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub stored_alert_lower_limit: Decimal,
    /// Price above which the user wants to be notified; 0 where a negative
    /// value is stored.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub alert_upper_limit: Decimal,
    /// `alert_upper_limit` as stored.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub stored_alert_upper_limit: Decimal,
    /// Date until which the position is blocked, as stored.
    pub blocked_until_date: Option<RawDate>,
    /// Additional text, one entry per line; empty lines are not kept
    /// (02 §5.2).
    pub additional_info: Vec<String>,
}

/// The value Banking4 keeps of a price or rate it does not allow to be
/// negative: a negative value becomes 0 (05 §3.8.1 rule 2, 02 §4.7).
fn non_negative(stored: Decimal) -> Decimal {
    if stored.is_sign_negative() && !stored.is_zero() {
        Decimal::ZERO
    } else {
        stored
    }
}

impl Record for PortfolioItem {
    const NAME: &'static str = "portfolio item";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let base = RecordBase::read(r);
        let item_state = PortfolioItemState::from_raw(r.i32());
        let stored_isin = r.string("isin")?;
        let name = r.string("name")?;
        let maturity_date = r.date_raw();
        let stored_interest_rate = r.decimal("interest_rate")?;
        let portfolio_key = r.string("portfolio_key")?;
        let quantity = r.decimal("quantity")?;
        let quantity_type = QuantityType::from_raw(r.i32());
        let stored_quantity_currency = r.code(3);
        let price_type = PriceType::from_raw(r.i32());
        let current_price_currency = r.code(3);
        let stored_purchase_price = r.decimal("purchase_price")?;
        let purchase_price_date = r.date_raw();
        let stored_current_price = r.decimal("current_price")?;
        let current_price_date = r.date_raw();
        let current_price_time = r.time_hms("current_price_time")?;
        let stored_purchase_price_currency = r.code(3);
        let stored_alert_lower_limit = r.decimal("alert_lower_limit")?;
        let stored_alert_upper_limit = r.decimal("alert_upper_limit")?;
        let blocked_until_date = r.date_raw();
        let additional_info = r.string("additional_info")?;

        // After reading (05 §3.8.1).
        let id = parse_security_id(stored_isin.as_deref());
        let purchase_price = non_negative(stored_purchase_price);
        let quantity_currency = if quantity_type == QuantityType::Units {
            None
        } else {
            stored_quantity_currency.clone()
        };
        let needs_currency = price_type == PriceType::Actual
            && purchase_price > Decimal::ZERO
            && stored_purchase_price_currency.is_none();
        let purchase_price_currency = if needs_currency {
            current_price_currency
                .clone()
                .or_else(|| Some(APPLICATION_DEFAULT_CURRENCY.to_owned()))
        } else {
            stored_purchase_price_currency.clone()
        };

        Ok(Self {
            base,
            item_state,
            isin: id.canonical_isin,
            stored_isin,
            isin_kind: id.kind,
            name: name.map(|name| name.replace('\n', " ")),
            maturity_date,
            interest_rate: non_negative(stored_interest_rate),
            stored_interest_rate,
            portfolio_key,
            quantity,
            quantity_type,
            quantity_currency,
            stored_quantity_currency,
            price_type,
            current_price_currency,
            purchase_price,
            stored_purchase_price,
            purchase_price_date,
            current_price: non_negative(stored_current_price),
            stored_current_price,
            current_price_date,
            current_price_time,
            purchase_price_currency,
            stored_purchase_price_currency,
            alert_lower_limit: non_negative(stored_alert_lower_limit),
            stored_alert_lower_limit,
            alert_upper_limit: non_negative(stored_alert_upper_limit),
            stored_alert_upper_limit,
            blocked_until_date,
            additional_info: text_lines(additional_info.as_deref())
                .into_iter()
                .map(str::to_owned)
                .collect(),
        })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

impl PortfolioItem {
    /// The stored identifier, interpreted (05 §5.1).
    #[must_use]
    pub fn security_id(&self) -> SecurityId {
        parse_security_id(self.stored_isin.as_deref())
    }

    /// The stock instrument of this position: the one whose ISIN is the
    /// canonical ISIN of the item's identifier (05 §5.2). `None` if the
    /// identifier has no canonical ISIN or no instrument carries it. The
    /// link is by ISIN, not by object id, and several items can share one
    /// instrument.
    #[must_use]
    pub fn instrument<'a>(
        &self,
        instruments: &'a [StockInstrument],
    ) -> Option<&'a StockInstrument> {
        instrument_for_isin(instruments, self.isin.as_deref()?)
    }
}

// --------------------------------------------------- portfolio transaction

/// One record of `Acct<oid>/PortfTxns` (05 §3.9): a securities transaction
/// reported for a portfolio account. Unlike the portfolio item it keeps its
/// dates as timestamps.
///
/// This record type is not confirmed against data exported from Banking4
/// (docs/format/README.md, open question A.8).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PortfolioTransaction {
    /// Object id and update stamp.
    #[serde(flatten)]
    pub base: RecordBase,
    /// Identifier of the transaction.
    pub transaction_id: Option<String>,
    /// State of the transaction.
    pub transaction_state: PortfolioTransactionState,
    /// Which way the securities move.
    pub delivery_direction: DeliveryDirection,
    /// Kind of business.
    pub activity: TransactionActivity,
    /// The canonical ISIN of the stored identifier (05 §5.1), `None` if it
    /// has none.
    pub isin: Option<String>,
    /// The security identifier as stored.
    pub stored_isin: Option<String>,
    /// What kind of identifier `stored_isin` is.
    pub isin_kind: SecurityIdKind,
    /// Name of the security.
    pub name: Option<String>,
    /// Quantity moved.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub quantity: Decimal,
    /// What `quantity` counts. One byte on the wire.
    pub quantity_type: QuantityType,
    /// Settlement date. No time zone.
    pub settlement_date: Option<Timestamp>,
    /// Settlement amount. No direction is stored with it.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub settlement_amount: Decimal,
    /// Currency of `settlement_amount`.
    pub settlement_currency: Option<String>,
    /// Whether accrued interest is part of the transaction.
    pub interest_accrued: bool,
    /// Number of interest days, 0 to 65535.
    pub interest_days: u16,
    /// Accrued interest amount.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub interest_amount: Decimal,
    /// Currency of `interest_amount`.
    pub interest_currency: Option<String>,
    /// Whether the user has seen the transaction.
    pub is_read: bool,
    /// Currency of a face-amount quantity.
    pub quantity_currency: Option<String>,
    /// Trade date. No time zone.
    pub trading_date: Option<Timestamp>,
    /// Trade price.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub trading_price: Decimal,
    /// Currency of `trading_price`.
    pub trading_currency: Option<String>,
}

impl Record for PortfolioTransaction {
    const NAME: &'static str = "portfolio transaction";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let base = RecordBase::read(r);
        let transaction_id = r.string("transaction_id")?;
        let transaction_state = PortfolioTransactionState::from_raw(r.i32());
        let delivery_direction = DeliveryDirection::from_raw(r.i32());
        let activity = TransactionActivity::from_raw(r.i32());
        let stored_isin = r.string("isin")?;
        let id = parse_security_id(stored_isin.as_deref());
        Ok(Self {
            base,
            transaction_id,
            transaction_state,
            delivery_direction,
            activity,
            isin: id.canonical_isin,
            stored_isin,
            isin_kind: id.kind,
            name: r.string("name")?,
            quantity: r.decimal("quantity")?,
            quantity_type: QuantityType::from_raw(i32::from(r.u8())),
            settlement_date: r.ticks("settlement_date")?,
            settlement_amount: r.decimal("settlement_amount")?,
            settlement_currency: r.code(3),
            interest_accrued: r.bool(),
            interest_days: r.u16(),
            interest_amount: r.decimal("interest_amount")?,
            interest_currency: r.code(3),
            is_read: r.bool(),
            quantity_currency: r.code(3),
            trading_date: r.ticks("trading_date")?,
            trading_price: r.decimal("trading_price")?,
            trading_currency: r.code(3),
        })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

impl PortfolioTransaction {
    /// The stored identifier, interpreted (05 §5.1).
    #[must_use]
    pub fn security_id(&self) -> SecurityId {
        parse_security_id(self.stored_isin.as_deref())
    }

    /// The stock instrument of this transaction, found by canonical ISIN
    /// like a portfolio item's (05 §5.2).
    #[must_use]
    pub fn instrument<'a>(
        &self,
        instruments: &'a [StockInstrument],
    ) -> Option<&'a StockInstrument> {
        instrument_for_isin(instruments, self.isin.as_deref()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerations_have_the_names_of_the_document() {
        let names = |values: &[String]| values.join(" ");
        let quantity: Vec<String> = (0..=3)
            .map(|raw| QuantityType::from_raw(raw).to_string())
            .collect();
        assert_eq!(names(&quantity), "undefined face_amount units 3");
        let price: Vec<String> = (0..=4)
            .map(|raw| PriceType::from_raw(raw).to_string())
            .collect();
        assert_eq!(names(&price), "undefined actual percent yield 4");
        let state: Vec<String> = (0..=4)
            .map(|raw| PortfolioTransactionState::from_raw(raw).to_string())
            .collect();
        assert_eq!(names(&state), "none pending executed settled 4");
        let direction: Vec<String> = (-1..=3)
            .map(|raw| DeliveryDirection::from_raw(raw).to_string())
            .collect();
        assert_eq!(names(&direction), "-1 none deliver receive 3");
        let activity: Vec<String> = (0..=5)
            .map(|raw| TransactionActivity::from_raw(raw).to_string())
            .collect();
        assert_eq!(
            names(&activity),
            "none borrowing_lending collateral corporate_action settlement 5"
        );
        assert_eq!(
            PortfolioItemState::from_raw(0x10002).to_string(),
            "bank,deleted"
        );
        assert_eq!(
            PortfolioItemState::from_raw(0xFFFF).to_string(),
            "local,bank,0xfffc"
        );
        assert!(PortfolioItemState::from_raw(0).is_empty());
    }

    #[test]
    fn negative_prices_become_zero_and_nothing_else_changes() {
        let decimal = |text: &str| text.parse::<Decimal>().unwrap();
        assert_eq!(non_negative(decimal("-0.01")), Decimal::ZERO);
        assert_eq!(non_negative(decimal("-5")).to_string(), "0");
        assert_eq!(non_negative(decimal("0.00")).to_string(), "0.00");
        assert_eq!(non_negative(decimal("12.340")).to_string(), "12.340");
    }
}
