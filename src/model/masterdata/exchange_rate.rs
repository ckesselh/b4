//! Exchange rates (05 §3.5).

use std::collections::HashSet;

use rust_decimal::Decimal;
use serde::Serialize;

use crate::model::base::{DecodeError, Record, RecordBase};
use crate::wire::{Reader, Timestamp};

/// The currency every rate Banking4 shows is quoted against.
const EURO: &str = "EUR";
/// The currency code of gold, which has an inversion rule of its own.
const GOLD: &str = "XAU";

/// One record of `ExchangeRatesBIN` (05 §3.5): a quote of one currency
/// against another, as stored. [`exchange_rate_view`] gives what Banking4
/// makes of the file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExchangeRate {
    /// Object id and update stamp.
    #[serde(flatten)]
    pub base: RecordBase,
    /// Base currency of the quote.
    pub base_currency: Option<String>,
    /// Quoted currency.
    pub target_currency: Option<String>,
    /// Time of the quote. No time zone.
    pub quote_time: Option<Timestamp>,
    /// Units of `target_currency` per one unit of `base_currency`.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub quote_rate: Decimal,
}

impl Record for ExchangeRate {
    const NAME: &'static str = "exchange rate";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            base: RecordBase::read(r),
            base_currency: r.code(3),
            target_currency: r.code(3),
            quote_time: r.ticks("quote_time")?,
            quote_rate: r.decimal("quote_rate")?,
        })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

/// Why Banking4's rate list does not show a record (05 §3.5 "Loader", not
/// confirmed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeRateDrop {
    /// The rate had to be turned round and is zero or too small for its
    /// reciprocal to be represented. Banking4 fails on such a record.
    NotInvertible,
    /// Neither side of the pair is the euro.
    NotEuroBased,
    /// The quoted currency is missing or not three upper-case letters.
    InvalidTarget,
    /// An earlier record of the file already gave a rate for this currency.
    Duplicate,
}

/// What Banking4's rate list makes of one decoded record while loading
/// (05 §3.5 "Loader").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExchangeRateView {
    /// The base currency after the loader turned the pair round.
    pub base_currency: Option<String>,
    /// The quoted currency after the loader turned the pair round.
    pub target_currency: Option<String>,
    /// The rate after the loader's inversions. The reciprocal is computed
    /// with this crate's decimal arithmetic; its last digits can differ from
    /// Banking4's.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub quote_rate: Decimal,
    /// `None` if Banking4 shows the rate, otherwise why it does not.
    pub dropped: Option<ExchangeRateDrop>,
}

/// Apply the rules of Banking4's rate list to the records of an
/// `ExchangeRatesBIN` file (05 §3.5 "Loader"), giving one view per record in
/// the same order. Nothing is removed from `rates`.
///
/// 1. A pair quoted against the euro the other way round is turned round and
///    its rate inverted.
/// 2. A euro-to-gold rate above 1 is inverted.
/// 3. A pair whose base is still not the euro is dropped.
/// 4. A target that is not exactly three upper-case letters `A` to `Z` is
///    dropped.
/// 5. Of several rates for one target, the first wins.
///
/// These rules are not confirmed.
#[must_use]
pub fn exchange_rate_view(rates: &[ExchangeRate]) -> Vec<ExchangeRateView> {
    let mut loaded: HashSet<String> = HashSet::new();
    rates
        .iter()
        .map(|rate| {
            let mut view = single_rate_view(rate);
            if view.dropped.is_none() {
                // A view that got this far has a target (step 4).
                let first = view
                    .target_currency
                    .clone()
                    .is_some_and(|target| loaded.insert(target));
                if !first {
                    view.dropped = Some(ExchangeRateDrop::Duplicate);
                }
            }
            view
        })
        .collect()
}

/// Steps 1 to 4 of the loader, which look at one record only.
fn single_rate_view(rate: &ExchangeRate) -> ExchangeRateView {
    let mut view = ExchangeRateView {
        base_currency: rate.base_currency.clone(),
        target_currency: rate.target_currency.clone(),
        quote_rate: rate.quote_rate,
        dropped: None,
    };
    let is = |code: &Option<String>, wanted: &str| code.as_deref() == Some(wanted);
    let reciprocal = |value: Decimal| {
        if value.is_zero() {
            None
        } else {
            Decimal::ONE.checked_div(value)
        }
    };

    if !is(&view.base_currency, EURO) && is(&view.target_currency, EURO) {
        std::mem::swap(&mut view.base_currency, &mut view.target_currency);
        match reciprocal(view.quote_rate) {
            Some(inverted) => view.quote_rate = inverted,
            None => {
                view.dropped = Some(ExchangeRateDrop::NotInvertible);
                return view;
            }
        }
    }
    if is(&view.base_currency, EURO)
        && is(&view.target_currency, GOLD)
        && view.quote_rate > Decimal::ONE
    {
        // Greater than 1, so the reciprocal exists.
        if let Some(inverted) = reciprocal(view.quote_rate) {
            view.quote_rate = inverted;
        }
    }
    let valid_target = view
        .target_currency
        .as_deref()
        .is_some_and(|code| code.len() == 3 && code.bytes().all(|byte| byte.is_ascii_uppercase()));
    if !is(&view.base_currency, EURO) {
        view.dropped = Some(ExchangeRateDrop::NotEuroBased);
    } else if !valid_target {
        view.dropped = Some(ExchangeRateDrop::InvalidTarget);
    }
    view
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(base: Option<&str>, target: Option<&str>, quote_rate: &str) -> ExchangeRate {
        ExchangeRate {
            base: RecordBase {
                oid: 1,
                update_stamp: 2,
            },
            base_currency: base.map(str::to_owned),
            target_currency: target.map(str::to_owned),
            quote_time: None,
            quote_rate: quote_rate.parse().unwrap(),
        }
    }

    /// The view of one record on its own: base, target, rate, drop reason.
    fn view(
        base: Option<&str>,
        target: Option<&str>,
        quote_rate: &str,
    ) -> (
        Option<String>,
        Option<String>,
        String,
        Option<ExchangeRateDrop>,
    ) {
        let views = exchange_rate_view(&[rate(base, target, quote_rate)]);
        assert_eq!(views.len(), 1);
        let view = &views[0];
        (
            view.base_currency.clone(),
            view.target_currency.clone(),
            view.quote_rate.normalize().to_string(),
            view.dropped,
        )
    }

    fn kept(
        base: &str,
        target: &str,
        quote_rate: &str,
    ) -> (
        Option<String>,
        Option<String>,
        String,
        Option<ExchangeRateDrop>,
    ) {
        (
            Some(base.to_owned()),
            Some(target.to_owned()),
            quote_rate.to_owned(),
            None,
        )
    }

    #[test]
    fn euro_based_rates_are_shown_as_stored() {
        assert_eq!(
            view(Some("EUR"), Some("USD"), "1.0834"),
            kept("EUR", "USD", "1.0834")
        );
        assert_eq!(view(Some("EUR"), Some("EUR"), "1"), kept("EUR", "EUR", "1"));
    }

    #[test]
    fn a_pair_quoted_the_other_way_round_is_turned_round() {
        assert_eq!(
            view(Some("USD"), Some("EUR"), "0.8"),
            kept("EUR", "USD", "1.25")
        );
        assert_eq!(
            view(Some("CHF"), Some("EUR"), "-4"),
            kept("EUR", "CHF", "-0.25")
        );
        // A missing base with the euro as target is turned round too and
        // then has no valid target.
        assert_eq!(
            view(None, Some("EUR"), "2"),
            (
                Some("EUR".to_owned()),
                None,
                "0.5".to_owned(),
                Some(ExchangeRateDrop::InvalidTarget)
            )
        );
    }

    #[test]
    fn gold_is_always_shown_as_ounces_per_euro() {
        assert_eq!(
            view(Some("EUR"), Some("XAU"), "2000"),
            kept("EUR", "XAU", "0.0005")
        );
        assert_eq!(
            view(Some("EUR"), Some("XAU"), "0.0005"),
            kept("EUR", "XAU", "0.0005")
        );
        assert_eq!(view(Some("EUR"), Some("XAU"), "1"), kept("EUR", "XAU", "1"));
        // Turned round first, then judged: 0.0005 euro per ounce becomes
        // 2000, which is above 1 and inverted back.
        assert_eq!(
            view(Some("XAU"), Some("EUR"), "0.0005"),
            kept("EUR", "XAU", "0.0005")
        );
        assert_eq!(
            view(Some("XAU"), Some("EUR"), "2000"),
            kept("EUR", "XAU", "0.0005")
        );
    }

    #[test]
    fn other_pairs_and_odd_targets_are_dropped() {
        let dropped = |base, target, reason| {
            let (_, _, _, dropped) = view(base, target, "2");
            assert_eq!(dropped, Some(reason), "{base:?} {target:?}");
        };
        dropped(Some("USD"), Some("GBP"), ExchangeRateDrop::NotEuroBased);
        dropped(None, Some("GBP"), ExchangeRateDrop::NotEuroBased);
        dropped(None, None, ExchangeRateDrop::NotEuroBased);
        dropped(Some("eur"), Some("USD"), ExchangeRateDrop::NotEuroBased);
        dropped(Some("EUR"), None, ExchangeRateDrop::InvalidTarget);
        dropped(Some("EUR"), Some("usd"), ExchangeRateDrop::InvalidTarget);
        dropped(Some("EUR"), Some("US"), ExchangeRateDrop::InvalidTarget);
        dropped(Some("EUR"), Some("US1"), ExchangeRateDrop::InvalidTarget);
        dropped(Some("EUR"), Some("ÄÖÜ"), ExchangeRateDrop::InvalidTarget);
        // Lower case on the euro side: not recognised as the euro at all.
        dropped(Some("USD"), Some("eur"), ExchangeRateDrop::NotEuroBased);
    }

    #[test]
    fn a_zero_rate_cannot_be_turned_round() {
        assert_eq!(
            view(Some("USD"), Some("EUR"), "0.00"),
            (
                Some("EUR".to_owned()),
                Some("USD".to_owned()),
                "0".to_owned(),
                Some(ExchangeRateDrop::NotInvertible)
            )
        );
        // Not turned round: a zero rate is shown as it is.
        assert_eq!(view(Some("EUR"), Some("USD"), "0"), kept("EUR", "USD", "0"));
    }

    #[test]
    fn the_first_rate_for_a_currency_wins() {
        let rates = [
            rate(Some("EUR"), Some("USD"), "1.1"),
            rate(Some("USD"), Some("GBP"), "9"),
            rate(Some("USD"), Some("EUR"), "0.5"),
            rate(Some("EUR"), Some("CHF"), "0.95"),
            rate(Some("EUR"), Some("USD"), "1.2"),
            rate(Some("EUR"), Some("usd"), "1.3"),
        ];
        let dropped: Vec<Option<ExchangeRateDrop>> = exchange_rate_view(&rates)
            .into_iter()
            .map(|view| view.dropped)
            .collect();
        assert_eq!(
            dropped,
            [
                None,
                Some(ExchangeRateDrop::NotEuroBased),
                // Turned round it is a second rate for the dollar.
                Some(ExchangeRateDrop::Duplicate),
                None,
                Some(ExchangeRateDrop::Duplicate),
                Some(ExchangeRateDrop::InvalidTarget),
            ]
        );
        assert!(exchange_rate_view(&[]).is_empty());
    }

    #[test]
    fn a_dropped_record_does_not_claim_its_currency() {
        // The first record cannot be turned round, so the second one is the
        // first rate for the dollar.
        let rates = [
            rate(Some("USD"), Some("EUR"), "0"),
            rate(Some("EUR"), Some("USD"), "1.1"),
        ];
        let views = exchange_rate_view(&rates);
        assert_eq!(views[0].dropped, Some(ExchangeRateDrop::NotInvertible));
        assert_eq!(views[1].dropped, None);
    }
}
