//! Master data and portfolio records
//! (docs/format/05-master-data-and-portfolio.md, sections 3.1 to 3.11).
//!
//! | Record | Vault entry | Type | Section |
//! |---|---|---|---|
//! | payee | `Payees` | [`Payee`] | 05 §3.1 |
//! | payer | `Payers` | [`Payer`] | 05 §3.2 |
//! | category | `Categories` | [`Category`] | 05 §3.3, §4 |
//! | mandate | `Mandates` | [`Mandate`] | 05 §3.4 |
//! | exchange rate | `ExchangeRatesBIN` | [`ExchangeRate`] | 05 §3.5 |
//! | stock instrument | `StockInstruments` | [`StockInstrument`] | 05 §3.6 |
//! | stock price | `Stock<oid>/StockPrices` | [`StockPrice`] | 05 §3.7 |
//! | portfolio item | `Acct<oid>/Portf` | [`PortfolioItem`] | 05 §3.8 |
//! | portfolio transaction | `Acct<oid>/PortfTxns` | [`PortfolioTransaction`] | 05 §3.9 |
//! | tax exemption | `Acct<oid>/AcctTaxExemptions` | [`TaxExemption`] | 05 §3.10 |
//! | asset | `Acct<oid>/Assets` | [`Asset`] | 05 §3.11 |
//!
//! All eleven are single-chunk records (05 §2.3) and are loaded with
//! [`load_collection`](crate::model::base::load_collection). None of them
//! holds bank access material, so nothing is dropped at decode time except
//! the unused slots the format reserves.
//!
//! # What a field holds
//!
//! Fields carry the names of the tables in document 05 and the value **after
//! the reader's adjustments** ("After reading" in 05), which is the value
//! Banking4 works with. Where such an adjustment replaces a stored value by
//! another one, a `stored_*` field next to it keeps what the file holds:
//!
//! | Record | Adjusted | As stored |
//! |---|---|---|
//! | payee | `payee_kind` | `stored_payee_kind` |
//! | category | `full_name`, `totals_bucket`, `budget_period`, `budget_amount`, `icon_name` | `stored_full_name`, `stored_totals_bucket`, ... |
//! | stock instrument, portfolio item, portfolio transaction | `isin` (the canonical ISIN of 05 §5.1) | `stored_isin` (the identifier text) |
//! | portfolio item | five prices and rates, `quantity_currency`, `purchase_price_currency` | `stored_interest_rate`, ... |
//!
//! Adjustments that only tidy a text (trimming an address line, replacing a
//! line break in a name, normalising a category name) are applied without a
//! stored copy.
//!
//! # Texts with a format of their own
//!
//! Three texts are parsed, and none of them can make a record undecodable
//! (docs/format/README.md, open item 13): the raw text is always kept and a
//! field says when it did not parse.
//!
//! | Text | Raw | Parsed | When it does not parse |
//! |---|---|---|---|
//! | security identifier (05 §5.1) | `stored_isin` | `isin`, `isin_kind` | `isin_kind` is [`SecurityIdKind::Unrecognised`] |
//! | stock splits (05 §3.6.1) | `splits` | `split_list` | `malformed_split_lines` counts the lines |
//! | category rules (05 §4.5) | `rules` | `parsed_rules` | every text parses |
//!
//! # What Banking4 shows
//!
//! The collections of Banking4 drop or change some records while loading.
//! Decoding never does: every record of a file is returned. The rules that
//! are fully specified are available as functions that mark records instead:
//! [`category_loader_view`], [`exchange_rate_view`],
//! [`Mandate::loader_drop_reason`] and [`duplicate_isins`].

mod assets;
mod category;
mod exchange_rate;
mod party;
mod portfolio;
mod rules;
mod security_id;
mod stock;

pub use assets::{Asset, TaxExemption};
pub use category::{
    category_ancestors, category_child, category_cmp, category_depth, category_is_within,
    category_key, category_leaf, category_loader_view, category_parent, missing_special_categories,
    normalise_category, normalise_category_for, same_category, special_category_by_default_name,
    BudgetPeriod, Category, CategoryLoaderMark, Language, SpecialCategory, TotalsBucket,
    CATEGORY_SEPARATOR, UNNAMED_LEVEL,
};
pub use exchange_rate::{exchange_rate_view, ExchangeRate, ExchangeRateDrop, ExchangeRateView};
pub use party::{
    split_address, Mandate, MandateDropReason, MandateFlags, Payee, PayeeFlags, PayeeKind, Payer,
    VopStatus,
};
pub use portfolio::{
    DeliveryDirection, PortfolioItem, PortfolioItemState, PortfolioTransaction,
    PortfolioTransactionState, PriceType, QuantityType, TransactionActivity,
    APPLICATION_DEFAULT_CURRENCY,
};
pub use rules::{canonical_rules_text, parse_category_rules, CategoryRule};
pub use security_id::{
    is_valid_isin, isin_check_digit, parse_security_id, SecurityId, SecurityIdKind,
};
pub use stock::{
    duplicate_isins, instrument_for_isin, parse_splits, stock_directory_oid, SplitList,
    StockInstrument, StockPrice, StockSplit,
};
