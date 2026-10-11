//! Standing orders, `Acct<oid>/Stos` (04 §9).

use rust_decimal::Decimal;
use serde::Serialize;

use crate::model::base::{DecodeError, Record};
use crate::wire::{RawDate, Reader, TimeOfDay};

use super::base::{PaymentBase, SchemeRule};
use super::enums::{PeriodUnit, StandingOrderState};

/// One standing order (04 §9). Standing orders are not grouped: there is no
/// batch link.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StandingOrder {
    /// Object header and payment base.
    #[serde(flatten)]
    pub base: PaymentBase,
    /// State.
    pub state: StandingOrderState,
    /// Object id of the outbox order that set it up, changed or cancelled it.
    pub outbox_oid: i32,
    /// First execution, components as stored.
    pub first_execution_date: Option<RawDate>,
    /// Unit of the period.
    pub period_unit: PeriodUnit,
    /// Number of units between executions: 1 to 12 months or 1 to 52 weeks.
    pub period_length: i32,
    /// Day within the period. Months: 1 to 31 is the day of the month, 97 to
    /// 99 count from its end (99 is the last day). Weeks: 1 is Monday, 7
    /// Sunday, 0 unspecified.
    pub execution_day: i32,
    /// Last execution, components as stored; none for "until further notice".
    pub last_execution_date: Option<RawDate>,
    /// Next execution as Banking4 keeps it (04 §9.2): the stored date of a
    /// standing order that is kept in Banking4 only, none for every other
    /// one. For those Banking4 computes the date from the schedule and the
    /// current date, by a rule that is not known.
    pub next_execution_date: Option<RawDate>,
    /// Next execution as stored: the date computed when the record was last
    /// saved. Stale unless the standing order is local.
    pub stored_next_execution_date: Option<RawDate>,
    /// Execution time of day.
    pub execution_time: Option<TimeOfDay>,
}

impl Record for StandingOrder {
    const NAME: &'static str = "standing order";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let base = PaymentBase::read(r, SchemeRule::DeriveIfMissing)?;
        // A no-op in a legacy record (04 §9.1).
        r.next_chunk()?;

        // 04 §9.1, in wire order. Positions 9 to 12 are unused: read to stay
        // in step, never kept.
        let state = StandingOrderState::from_raw(r.i32());
        let outbox_oid = r.i32();
        let first_execution_date = r.date_raw();
        let period_unit = PeriodUnit::from_raw(r.i32());
        let period_length = r.i32();
        let execution_day = r.i32();
        let last_execution_date = r.date_raw();
        let stored_next_execution_date = r.date_raw();
        let _reserved_a = r.i32();
        let _reserved_b = r.bool();
        let _reserved_c = r.i32();
        let _reserved_d = r.ticks("reserved_d")?;
        let execution_time = r.time_hms("execution_time")?;

        Ok(Self {
            base,
            state,
            outbox_oid,
            first_execution_date,
            period_unit,
            period_length,
            execution_day,
            last_execution_date,
            // 04 §9.2: the stored date only counts for a local one.
            next_execution_date: stored_next_execution_date
                .filter(|_| state.contains(StandingOrderState::LOCAL)),
            stored_next_execution_date,
            execution_time,
        })
    }

    fn oid(&self) -> i32 {
        self.base.record.oid
    }

    fn is_legacy(&self) -> bool {
        self.base.legacy
    }
}

impl StandingOrder {
    /// Whether money leaves the account. For a standing order the scheme
    /// decides, not the stored bit (04 §9.4).
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
    /// the account's currency for a standing order the user entered by hand,
    /// `DEM` or `EUR` where the scheme implies it, otherwise none.
    ///
    /// `account_currency` is the currency of the account whose directory the
    /// file is in, with whatever the caller uses for an account without one.
    #[must_use]
    pub fn displayed_currency<'a>(&'a self, account_currency: Option<&'a str>) -> Option<&'a str> {
        self.base.currency_or_scheme_default(account_currency)
    }
}
