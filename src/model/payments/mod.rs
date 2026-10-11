//! The payment family: bookings, payments and standing orders
//! (docs/format/04-payments-and-bookings.md).
//!
//! The three record types share the [`PaymentBase`]: an amount without sign,
//! a direction, the other party, the purpose and the SEPA details. Each is a
//! [`Record`](crate::model::base::Record) and is loaded with
//! [`load_collection`](crate::model::base::load_collection).
//!
//! # What a decoded record holds
//!
//! Banking4 does not show the values a file stores; its reader changes some
//! of them, and its lists change more. The work is split the same way here.
//!
//! 1. **Decoding** gives the stored values after the adjustments that need
//!    nothing but the record itself: purpose text cut into lines, category
//!    name normalised, tagged purpose lines of a legacy single-chunk record
//!    moved into their fields, a missing scheme derived, an undated booking
//!    made pending, and so on (04 §5.4 to §5.8, §6.2, §8.2, §9.2). Where
//!    that replaces a stored number, a `stored_` field keeps the original.
//! 2. **The account** a file belongs to decides two more things, which the
//!    decoder cannot know: [`Booking::apply_account_kind`] and
//!    [`Booking::adjusted_currency`] (04 §6.2 steps 3 and 4), and the
//!    currency of a payment or standing order that stores none
//!    ([`Payment::displayed_currency`], 04 §5.11).
//! 3. **The whole list** of an account is regrouped by
//!    [`regroup_bookings`] and [`apply_payment_rules`] (04 §6.4, §8.3).
//!    They mark records and never remove one: `b4` keeps every record a
//!    file holds.
//!
//! ```
//! use b4::model::base::load_collection;
//! use b4::model::payments::{regroup_bookings, Booking, ACCOUNT_KIND_CASH};
//! use b4::wire::Date;
//!
//! # let stmt_file: Vec<u8> = Vec::new();
//! # let today = Date::MAX;
//! let mut bookings = load_collection::<Booking>(&stmt_file);
//! for booking in &mut bookings.records {
//!     booking.apply_account_kind(ACCOUNT_KIND_CASH);
//! }
//! let groups = regroup_bookings(&mut bookings.records, today);
//! let balance_movement: rust_decimal::Decimal = bookings
//!     .records
//!     .iter()
//!     .filter(|booking| booking.is_counted())
//!     .map(Booking::signed_amount)
//!     .sum();
//! # assert_eq!(groups.groups, 0);
//! # assert!(balance_movement.is_zero());
//! ```
//!
//! # Amounts
//!
//! `amount` is the stored value, which has no sign. The direction is the
//! stored bit for a booking and follows from the scheme for a payment or
//! standing order (04 §5.5); `is_debit()` and `signed_amount()` apply that
//! rule. A signed amount is negative when money leaves the account.
//!
//! # Not here
//!
//! The outbox order and the split editing row (04 §7, §10) are not decoded.
//! The two authorisation values of a payment are read to stay in step and
//! dropped (see [`Payment`]).

mod base;
mod booking;
mod collection;
mod enums;
mod legacy;
mod payment;
mod standing_order;

pub use base::PaymentBase;
pub use booking::Booking;
pub use collection::{
    apply_payment_rules, banking4_booking_order, banking4_payment_order, regroup_bookings,
    PaymentRules, Regrouping,
};
pub use enums::{
    BatchBooking, BatchRole, BookingStatus, ColorFlag, CostBearer, EntryBits,
    PayeeVerificationChoice, PaymentFlags, PaymentState, PeriodUnit, SchemeClass, SchemeType,
    SepaStatus, SplitRole, StandingOrderState, TransactionCode, VatRateType,
};
pub use payment::Payment;
pub use standing_order::StandingOrder;

/// The account kind of a cash account (04 §11.22, 03 "Account kind"). Its
/// bookings carry no value, analysis or transaction date (04 §6.2 step 4).
pub const ACCOUNT_KIND_CASH: u8 = 0x02;
/// The account kind of a PayPal account (04 §11.22). Its bookings keep their
/// stored currency (04 §6.2 step 3).
pub const ACCOUNT_KIND_PAYPAL: u8 = 0x20;

/// Record bytes for the unit tests of this module, put together primitive by
/// primitive (02 §4).
#[cfg(test)]
mod testing {
    /// The payload of one chunk.
    #[derive(Debug, Default)]
    pub(super) struct Payload(Vec<u8>);

    impl Payload {
        pub(super) fn u8(mut self, value: u8) -> Self {
            self.0.push(value);
            self
        }

        pub(super) fn u16(mut self, value: u16) -> Self {
            self.0.extend(value.to_le_bytes());
            self
        }

        pub(super) fn i32(mut self, value: i32) -> Self {
            self.0.extend(value.to_le_bytes());
            self
        }

        /// A decimal with two decimals from a number of cents.
        pub(super) fn cents(mut self, cents: u32) -> Self {
            self.0.extend(cents.to_le_bytes());
            self.0.extend([0; 8]);
            self.0.extend(0x0002_0000_u32.to_le_bytes());
            self
        }

        /// A string; the empty one is "no value".
        pub(super) fn string(mut self, text: &str) -> Self {
            let length = u16::try_from(text.len()).expect("a short test string");
            self.0.extend(length.to_le_bytes());
            self.0.extend(text.bytes());
            self
        }

        /// A fixed-width code, padded with NUL.
        pub(super) fn code(mut self, width: usize, text: &str) -> Self {
            assert!(text.len() <= width);
            self.0.extend(text.bytes());
            self.0.resize(self.0.len() + width - text.len(), 0);
            self
        }

        /// A date; all zeros is "no date".
        pub(super) fn date(mut self, year: u16, month: u8, day: u8) -> Self {
            self.0.extend(year.to_le_bytes());
            self.0.extend([month, day]);
            self
        }

        /// A time of day; `None` is "no time".
        pub(super) fn time(mut self, time: Option<(u8, u8, u8)>) -> Self {
            let (hour, minute, second) = time.unwrap_or((0xFF, 0xFF, 0xFF));
            self.0.extend([hour, minute, second]);
            self
        }

        pub(super) fn ticks(mut self, ticks: i64) -> Self {
            self.0.extend(ticks.to_le_bytes());
            self
        }
    }

    /// A record body from the payloads of its chunks.
    pub(super) fn record(chunks: &[Payload]) -> Vec<u8> {
        let mut body = Vec::new();
        for (number, chunk) in chunks.iter().enumerate() {
            let size = u32::try_from(chunk.0.len() + 4).expect("a small test chunk");
            let more = if number + 1 < chunks.len() {
                0x8000_0000
            } else {
                0
            };
            body.extend((size | more).to_le_bytes());
            body.extend(&chunk.0);
        }
        body
    }
}
