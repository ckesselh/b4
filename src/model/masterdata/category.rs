//! Categories (05 §3.3 and §4): the record, the hierarchy that lives in the
//! names, the normalisation of stored names, budgets and the loader's rules.

use std::cmp::Ordering;

use rust_decimal::Decimal;
use serde::Serialize;

use super::rules::{parse_category_rules, CategoryRule};
use crate::model::base::{code_enum, DecodeError, Record, RecordBase};
use crate::wire::Reader;

/// The character between the levels of a category full name (05 §4.1). A
/// level name cannot contain it.
pub const CATEGORY_SEPARATOR: char = ':';

/// What an empty level of a stored category name is replaced by (04 §5.7
/// step 5). The literal is the same in every language of Banking4.
pub const UNNAMED_LEVEL: &str = "UNBEKANNT";

/// First `layout_version` whose `budget_period` byte is meaningful (05 §3.3.1).
const BUDGET_PERIOD_SINCE_VERSION: i32 = 6;

// ------------------------------------------------------------ enumerations

code_enum! {
    /// How a category enters the income and expense totals (05 §6.4).
    pub enum TotalsBucket: u8 {
        /// Decided by the sign of the bookings.
        Auto = 0 => "auto",
        /// Left out of totals.
        Exclude = 1 => "exclude",
        Income = 2 => "income",
        Expense = 3 => "expense",
    }
}

code_enum! {
    /// The time span one `budget_amount` is meant for (05 §6.5, §4.4).
    pub enum BudgetPeriod: u8 {
        /// Counted as one month.
        None = 0 => "none",
        /// Apparently retired; counted as one month.
        Week = 1 => "week",
        Month = 2 => "month",
        Quarter = 3 => "quarter",
        Year = 4 => "year",
        /// The amount is a fixed total, not scaled by time.
        All = 5 => "all",
        /// Only used for evaluation ranges; as a budget period one month.
        MonthRange = 6 => "month_range",
    }
}

impl BudgetPeriod {
    /// How many months one period spans for the budget arithmetic of 05 §4.4:
    /// 3 for a quarter, 12 for a year, 1 for everything else.
    #[must_use]
    pub const fn months(self) -> u32 {
        match self {
            Self::Quarter => 3,
            Self::Year => 12,
            _ => 1,
        }
    }
}

code_enum! {
    /// The role of a built-in category (05 §6.6, §4.3). There is at most one
    /// category of each special kind in what Banking4 shows.
    pub enum SpecialCategory: u8 {
        /// An ordinary category.
        None = 0 => "none",
        /// Transfers between own accounts. Always top level, excluded from
        /// totals, without a budget.
        Reclassification = 1 => "reclassification",
        /// "Do not evaluate". Always top level, excluded from totals,
        /// without a budget.
        Excluded = 2 => "excluded",
        /// Value added tax.
        Vat = 3 => "vat",
    }
}

impl SpecialCategory {
    /// The three special kinds, in the order of their numbers.
    pub const SPECIAL: [SpecialCategory; 3] = [Self::Reclassification, Self::Excluded, Self::Vat];
}

// ---------------------------------------------------------------- language

/// A user interface language of Banking4. Two rules of the category name
/// normalisation and the names of newly created special categories depend on
/// the language the writing installation ran in, which a vault does not
/// record (docs/format/README.md, open question A.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    /// The product's default, also used for Swiss German.
    German,
    /// English.
    English,
    /// French.
    French,
}

impl Language {
    /// Every language the documented Banking4 version ships texts for.
    pub const ALL: [Language; 3] = [Self::German, Self::English, Self::French];

    /// The names of the three pseudo categories (catalogue root, "automatic",
    /// "not set") in this language (04 §5.7). They are placeholders of the
    /// user interface and never names of real categories.
    #[must_use]
    pub const fn pseudo_category_names(self) -> [&'static str; 3] {
        match self {
            Self::German => ["(Kategorienkatalog)", "(automatisch)", "(nicht gesetzt)"],
            Self::English => ["(Catalog of Categories)", "(automatic)", "(not set)"],
            Self::French => [
                "(Catalogue des catégories)",
                "(automatique)",
                "(non activé)",
            ],
        }
    }

    /// The name Banking4 gives a special category when it creates it in this
    /// language (05 §4.3); `None` for [`SpecialCategory::None`] and unknown
    /// values. A user can rename the category afterwards, so a category is
    /// recognised by its `special_category`, never by this name.
    #[must_use]
    pub const fn special_category_name(self, special: SpecialCategory) -> Option<&'static str> {
        Some(match (special, self) {
            (SpecialCategory::Reclassification, Self::German) => "(Umbuchung)",
            (SpecialCategory::Reclassification, Self::English) => "(Transfer)",
            (SpecialCategory::Reclassification, Self::French) => "(Transfert)",
            (SpecialCategory::Excluded, Self::German) => "(nicht auswerten)",
            (SpecialCategory::Excluded, Self::English) => "(exclude)",
            (SpecialCategory::Excluded, Self::French) => "(ne pas évaluer)",
            (SpecialCategory::Vat, Self::German) => "(Umsatzsteuer)",
            (SpecialCategory::Vat, Self::English | Self::French) => "(VAT)",
            (SpecialCategory::None | SpecialCategory::Unknown(_), _) => return None,
        })
    }

    /// The full name of the "do not evaluate" category in this language,
    /// which a stored name of white space only is replaced by (04 §5.7
    /// step 3).
    #[must_use]
    pub const fn excluded_category_name(self) -> &'static str {
        match self {
            Self::German => "(nicht auswerten)",
            Self::English => "(exclude)",
            Self::French => "(ne pas évaluer)",
        }
    }
}

/// The special category a name stands for when it is one of the names
/// Banking4 creates special categories with, in any language (05 §4.3). The
/// comparison is exact.
///
/// This is for a category text on a booking that matches no category record:
/// it may refer to a special category of a catalogue that was not created
/// yet. A category *record* is special by its `special_category` field only.
#[must_use]
pub fn special_category_by_default_name(name: &str) -> Option<SpecialCategory> {
    SpecialCategory::SPECIAL.into_iter().find(|&special| {
        Language::ALL
            .into_iter()
            .any(|language| language.special_category_name(special) == Some(name))
    })
}

// ----------------------------------------------------------- normalisation

/// Normalise a stored category name the way a reader that does not know the
/// language of the writing installation should (04 §5.7, 05 §4.2). Every
/// record decoder of this crate uses it for its category field, and a
/// category text from any record must go through it before it is compared
/// with [`Category::full_name`].
///
/// 1. `None` stays `None`.
/// 2. White space at both ends is removed.
/// 3. A text of white space only becomes the German name of the "do not
///    evaluate" category (German is the product's default).
/// 4. The name of a pseudo category in **any** of the three languages
///    becomes `None`.
/// 5. Otherwise the text is cut at every `:`, each level is trimmed, an
///    empty level becomes [`UNNAMED_LEVEL`], and the levels are joined again.
///
/// Banking4 itself applies steps 3 and 4 with the texts of the one language
/// it runs in; [`normalise_category_for`] does exactly that. The two differ
/// only for a stored name that is a pseudo category name of another language.
#[must_use]
pub fn normalise_category(text: Option<&str>) -> Option<String> {
    normalise(text, Language::German, &Language::ALL)
}

/// Normalise a stored category name exactly as Banking4 does when it runs in
/// `language` (04 §5.7): like [`normalise_category`], with the texts of that
/// one language in steps 3 and 4.
#[must_use]
pub fn normalise_category_for(text: Option<&str>, language: Language) -> Option<String> {
    normalise(text, language, &[language])
}

fn normalise(
    text: Option<&str>,
    excluded_name: Language,
    pseudo_names: &[Language],
) -> Option<String> {
    // `str::trim` removes the Unicode White_Space set, as Banking4 does
    // (04 §5.7 step 2).
    let trimmed = text?.trim();
    if trimmed.is_empty() {
        return Some(excluded_name.excluded_category_name().to_owned());
    }
    let is_pseudo = pseudo_names
        .iter()
        .any(|language| language.pseudo_category_names().contains(&trimmed));
    if is_pseudo {
        return None;
    }
    let levels: Vec<&str> = trimmed
        .split(CATEGORY_SEPARATOR)
        .map(|level| match level.trim() {
            "" => UNNAMED_LEVEL,
            level => level,
        })
        .collect();
    Some(levels.join(":"))
}

// --------------------------------------------------------------- hierarchy

/// The name of a category: the last level of its full name (05 §4.1).
#[must_use]
pub fn category_leaf(full_name: &str) -> &str {
    full_name
        .rsplit_once(CATEGORY_SEPARATOR)
        .map_or(full_name, |(_, leaf)| leaf)
}

/// The full name of the parent category, or `None` for a top-level category
/// (05 §4.1). The parent need not exist as a record.
#[must_use]
pub fn category_parent(full_name: &str) -> Option<&str> {
    full_name
        .rsplit_once(CATEGORY_SEPARATOR)
        .map(|(parent, _)| parent)
}

/// The depth of a category: the number of separators in its full name, so 0
/// for a top-level category (05 §4.1).
#[must_use]
pub fn category_depth(full_name: &str) -> usize {
    full_name.matches(CATEGORY_SEPARATOR).count()
}

/// The full name of the child `name` of the category `parent` (05 §4.1).
#[must_use]
pub fn category_child(parent: &str, name: &str) -> String {
    format!("{parent}{CATEGORY_SEPARATOR}{name}")
}

/// The full names of all ancestors of a category, from the top level down to
/// its parent; empty for a top-level category. None of them need exist as a
/// record: nothing creates records for missing ancestors (05 §4.1).
#[must_use]
pub fn category_ancestors(full_name: &str) -> Vec<&str> {
    full_name
        .match_indices(CATEGORY_SEPARATOR)
        .map(|(index, _)| &full_name[..index])
        .collect()
}

/// Whether `full_name` is `ancestor` itself or one of its descendants: equal,
/// or starting with `ancestor` followed by a separator. This test is exact
/// and case-sensitive (05 §4.1), unlike [`same_category`].
#[must_use]
pub fn category_is_within(full_name: &str, ancestor: &str) -> bool {
    full_name
        .strip_prefix(ancestor)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(CATEGORY_SEPARATOR))
}

/// The character Banking4 compares instead of `c` when it orders category
/// names: its upper-case form (05 §4.1 rule 2). The comparison works on
/// single UTF-16 units, so a character whose upper-case form is not exactly
/// one such unit (`ß`, anything outside the basic plane) stays as it is.
fn fold(c: char) -> char {
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) if c.len_utf16() == 1 && single.len_utf16() == 1 => single,
        _ => c,
    }
}

/// The UTF-16 units of `name` after upper-casing, which is what the ordering
/// of 05 §4.1 compares.
fn folded_units(name: &str) -> impl Iterator<Item = u16> + '_ {
    name.chars().map(fold).flat_map(|c| {
        let mut buffer = [0_u16; 2];
        let units = c.encode_utf16(&mut buffer).len();
        buffer.into_iter().take(units)
    })
}

/// The key under which two full names are the same category: the name with
/// every character upper-cased (05 §4.1). Banking4 treats names that differ
/// only in case as one category, so joins between a category text and the
/// category list go through this key.
///
/// How Banking4 upper-cases characters beyond Latin-1 is not confirmed; this
/// function maps every character to its single upper-case character where
/// Unicode defines one.
#[must_use]
pub fn category_key(full_name: &str) -> String {
    full_name.chars().map(fold).collect()
}

/// The order in which Banking4 keeps its category list (05 §4.1): `None`
/// first; then character by character ignoring case, where the separator
/// sorts before every other character at the first difference, so that a
/// category is followed immediately by its descendants; a name that is the
/// beginning of another one sorts first.
#[must_use]
pub fn category_cmp(a: Option<&str>, b: Option<&str>) -> Ordering {
    let (a, b) = match (a, b) {
        (None, None) => return Ordering::Equal,
        (None, Some(_)) => return Ordering::Less,
        (Some(_), None) => return Ordering::Greater,
        (Some(a), Some(b)) => (a, b),
    };
    const SEPARATOR: u16 = CATEGORY_SEPARATOR as u16;
    let (mut a, mut b) = (folded_units(a), folded_units(b));
    loop {
        match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x == y => {}
            (Some(SEPARATOR), Some(_)) => return Ordering::Less,
            (Some(_), Some(SEPARATOR)) => return Ordering::Greater,
            (Some(x), Some(y)) => return x.cmp(&y),
        }
    }
}

/// Whether two full names are the same category: equal ignoring case
/// (05 §4.1). Neither side is trimmed or normalised here (05 §4.6);
/// normalise a stored text with [`normalise_category`] first.
#[must_use]
pub fn same_category(a: &str, b: &str) -> bool {
    category_cmp(Some(a), Some(b)) == Ordering::Equal
}

// ------------------------------------------------------------------ record

/// One record of `Categories` (05 §3.3).
///
/// A category has no id that anything refers to: other records name it by
/// its full name as text (05 §4.6). Find the category of such a text with
/// [`normalise_category`] and [`same_category`], or join on
/// [`category_key`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Category {
    /// Object id and update stamp.
    #[serde(flatten)]
    pub base: RecordBase,
    /// The full hierarchical name, levels separated by `:`, normalised; for
    /// the special categories 1 and 2 only its last level (05 §3.3.1).
    /// `None` for a record Banking4 would drop.
    pub full_name: Option<String>,
    /// The name as stored.
    pub stored_full_name: Option<String>,
    /// Version of the record layout the writer used; current writers store 9.
    /// Banking4 does not keep it. It is kept here because the stored `rules`
    /// text and `stored_budget_period` cannot be interpreted without it.
    pub layout_version: i32,
    /// Free description.
    pub description: Option<String>,
    /// The keyword expression for automatic categorisation, as stored
    /// (05 §4.5).
    pub rules: Option<String>,
    /// `rules` parsed by the rule of `layout_version`: positive rules first,
    /// then the negative ones.
    pub parsed_rules: Vec<CategoryRule>,
    /// How the category enters totals; `exclude` for the special categories
    /// 1 and 2.
    pub totals_bucket: TotalsBucket,
    /// `totals_bucket` as stored.
    pub stored_totals_bucket: TotalsBucket,
    /// The period `budget_amount` is for: `month` when `layout_version` is
    /// below 6 whatever is stored, `none` for the special categories 1 and 2.
    pub budget_period: BudgetPeriod,
    /// `budget_period` as stored.
    pub stored_budget_period: BudgetPeriod,
    /// Budget for one `budget_period`; 0 for the special categories 1 and 2.
    /// Whether an expense budget is positive or negative is not established
    /// (05 §9 question 6).
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub budget_amount: Decimal,
    /// `budget_amount` as stored.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub stored_budget_amount: Decimal,
    /// VAT rate in percent that bookings of this category get by default.
    #[serde(serialize_with = "crate::wire::serialize_decimal")]
    pub vat_percent: Decimal,
    /// Name of the icon; `reclass` and `exclude` for the special categories
    /// 1 and 2.
    pub icon_name: Option<String>,
    /// `icon_name` as stored.
    pub stored_icon_name: Option<String>,
    /// Name of the colour.
    pub color_name: Option<String>,
    /// Whether this is one of the built-in categories.
    pub special_category: SpecialCategory,
}

impl Record for Category {
    const NAME: &'static str = "category";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let base = RecordBase::read(r);
        let stored_full_name = r.string("full_name")?;
        let layout_version = r.i32();
        let description = r.string("description")?;
        let rules = r.string("rules")?;
        r.skip_string("reserved_text")?;
        let stored_totals_bucket = TotalsBucket::from_raw(r.u8());
        // The byte is consumed whatever the version says (05 §3.3.1).
        let stored_budget_period = BudgetPeriod::from_raw(r.u8());
        let stored_budget_amount = r.decimal("budget_amount")?;
        let vat_percent = r.decimal("vat_percent")?;
        let stored_icon_name = r.string("icon_name")?;
        let color_name = r.string("color_name")?;
        let special_category = SpecialCategory::from_raw(r.u8());
        let _reserved_byte = r.u8();

        // After reading (05 §3.3.1). The version rule comes first: the
        // special categories override its result.
        let mut full_name = normalise_category(stored_full_name.as_deref());
        let mut totals_bucket = stored_totals_bucket;
        let mut budget_period = if layout_version < BUDGET_PERIOD_SINCE_VERSION {
            BudgetPeriod::Month
        } else {
            stored_budget_period
        };
        let mut budget_amount = stored_budget_amount;
        let mut icon_name = stored_icon_name.clone();
        let forced_icon = match special_category {
            SpecialCategory::Reclassification => Some("reclass"),
            SpecialCategory::Excluded => Some("exclude"),
            _ => None,
        };
        if let Some(icon) = forced_icon {
            full_name = full_name.map(|name| category_leaf(&name).to_owned());
            totals_bucket = TotalsBucket::Exclude;
            budget_period = BudgetPeriod::None;
            budget_amount = Decimal::ZERO;
            icon_name = Some(icon.to_owned());
        }

        Ok(Self {
            base,
            full_name,
            stored_full_name,
            layout_version,
            description,
            parsed_rules: parse_category_rules(rules.as_deref(), layout_version),
            rules,
            totals_bucket,
            stored_totals_bucket,
            budget_period,
            stored_budget_period,
            budget_amount,
            stored_budget_amount,
            vat_percent,
            icon_name,
            stored_icon_name,
            color_name,
            special_category,
        })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

impl Category {
    /// The name of the category: the last level of `full_name`.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.full_name.as_deref().map(category_leaf)
    }

    /// The full name of the parent, `None` for a top-level category.
    #[must_use]
    pub fn parent(&self) -> Option<&str> {
        self.full_name.as_deref().and_then(category_parent)
    }

    /// The depth in the tree, 0 for a top-level category.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.full_name.as_deref().map_or(0, category_depth)
    }

    /// The key other records' category texts are matched with
    /// ([`category_key`]).
    #[must_use]
    pub fn key(&self) -> Option<String> {
        self.full_name.as_deref().map(category_key)
    }

    /// The budget for one month: `budget_amount` divided by the months of
    /// its period (05 §4.4, not confirmed). `None` if the result cannot be
    /// represented.
    #[must_use]
    pub fn monthly_budget(&self) -> Option<Decimal> {
        self.budget_amount
            .checked_div(Decimal::from(self.budget_period.months()))
    }

    /// The budget for a span of `months` months: `budget_amount × months /
    /// months per period`, except for the period `all`, where it is
    /// `budget_amount` whatever the span (05 §4.4, not confirmed). The
    /// budgets of child categories are not included. `None` if the result
    /// cannot be represented.
    #[must_use]
    pub fn budget_for_months(&self, months: u32) -> Option<Decimal> {
        if self.budget_period == BudgetPeriod::All {
            return Some(self.budget_amount);
        }
        self.budget_amount
            .checked_mul(Decimal::from(months))?
            .checked_div(Decimal::from(self.budget_period.months()))
    }
}

// ------------------------------------------------------------ loader rules

/// What the category list of Banking4 makes of one decoded record while
/// loading (05 §3.3 "Loader", not confirmed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CategoryLoaderMark {
    /// The record has no name after reading and is not shown.
    pub dropped: bool,
    /// The special role the record has in the application: the stored one
    /// for the first record of each special kind, `none` for every later
    /// record of the same kind.
    pub special_category: SpecialCategory,
}

/// Mark the records of a `Categories` file the way Banking4 treats them when
/// it loads the file (05 §3.3 "Loader"), one mark per record in the same
/// order. Nothing is removed from `categories`.
///
/// A record without a name is dropped, and a dropped record does not claim
/// its special kind. The document does not say which of the two tests comes
/// first; this order follows from "a record the loader drops does not appear
/// in the application at all" (05 §2.4).
#[must_use]
pub fn category_loader_view(categories: &[Category]) -> Vec<CategoryLoaderMark> {
    let mut claimed: Vec<SpecialCategory> = Vec::new();
    categories
        .iter()
        .map(|category| {
            let dropped = category.full_name.is_none();
            let stored = category.special_category;
            let is_special = SpecialCategory::SPECIAL.contains(&stored);
            let special_category = if dropped || (is_special && claimed.contains(&stored)) {
                SpecialCategory::None
            } else {
                if is_special {
                    claimed.push(stored);
                }
                stored
            };
            CategoryLoaderMark {
                dropped,
                special_category,
            }
        })
        .collect()
}

/// The special categories Banking4 would add after loading this file because
/// no record carries them (05 §3.3 "Loader", §4.3): none if the file yields
/// no category at all, otherwise each of the three kinds that is missing.
/// A build of Banking4 without VAT support does not add the VAT category.
#[must_use]
pub fn missing_special_categories(categories: &[Category]) -> Vec<SpecialCategory> {
    let marks = category_loader_view(categories);
    if marks.iter().all(|mark| mark.dropped) {
        return Vec::new();
    }
    SpecialCategory::SPECIAL
        .into_iter()
        .filter(|special| marks.iter().all(|mark| mark.special_category != *special))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerations_have_the_names_of_the_document() {
        let buckets: Vec<String> = (0..=4)
            .map(|raw| TotalsBucket::from_raw(raw).to_string())
            .collect();
        assert_eq!(buckets, ["auto", "exclude", "income", "expense", "4"]);
        let periods: Vec<String> = (0..=7)
            .map(|raw| BudgetPeriod::from_raw(raw).to_string())
            .collect();
        assert_eq!(
            periods,
            [
                "none",
                "week",
                "month",
                "quarter",
                "year",
                "all",
                "month_range",
                "7"
            ]
        );
        let special: Vec<String> = (0..=4)
            .map(|raw| SpecialCategory::from_raw(raw).to_string())
            .collect();
        assert_eq!(
            special,
            ["none", "reclassification", "excluded", "vat", "4"]
        );
    }

    #[test]
    fn normalisation_follows_the_examples_of_the_document() {
        // 05 §4.2.
        for (stored, normalised) in [
            (" Home : Rent ", "Home:Rent"),
            ("Home::Rent", "Home:UNBEKANNT:Rent"),
            (":", "UNBEKANNT:UNBEKANNT"),
            ("Home:", "Home:UNBEKANNT"),
            ("Home", "Home"),
            ("\u{a0}Home\t:\r\n Rent\u{85}", "Home:Rent"),
            // 1C..1F are not white space (04 §5.7 step 2).
            ("\u{1c}Home", "\u{1c}Home"),
            ("   ", "(nicht auswerten)"),
            ("\t", "(nicht auswerten)"),
        ] {
            assert_eq!(
                normalise_category(Some(stored)).as_deref(),
                Some(normalised),
                "{stored:?}"
            );
        }
        assert_eq!(normalise_category(None), None);
    }

    #[test]
    fn pseudo_category_names_are_no_names() {
        for language in Language::ALL {
            for name in language.pseudo_category_names() {
                assert_eq!(normalise_category(Some(name)), None, "{name}");
                assert_eq!(normalise_category_for(Some(name), language), None);
                // Surrounding blanks are removed before the comparison, which
                // is exact and case-sensitive.
                assert_eq!(normalise_category(Some(&format!(" {name} "))), None);
                let lower = name.to_lowercase();
                if lower != name {
                    assert_eq!(normalise_category(Some(&lower)), Some(lower));
                }
            }
        }
        // As a level of a longer name it is ordinary text.
        assert_eq!(
            normalise_category(Some("Home:(automatisch)")).as_deref(),
            Some("Home:(automatisch)")
        );
    }

    #[test]
    fn one_language_knows_only_its_own_texts() {
        assert_eq!(
            normalise_category_for(Some("(automatic)"), Language::German).as_deref(),
            Some("(automatic)")
        );
        assert_eq!(
            normalise_category_for(Some("(automatic)"), Language::English),
            None
        );
        assert_eq!(
            normalise_category_for(Some("  "), Language::German).as_deref(),
            Some("(nicht auswerten)")
        );
        assert_eq!(
            normalise_category_for(Some("  "), Language::English).as_deref(),
            Some("(exclude)")
        );
        assert_eq!(
            normalise_category_for(Some("  "), Language::French).as_deref(),
            Some("(ne pas évaluer)")
        );
        assert_eq!(
            normalise_category_for(Some(" A :: B "), Language::French).as_deref(),
            Some("A:UNBEKANNT:B")
        );
    }

    #[test]
    fn hierarchy_is_read_from_the_name() {
        let name = "Home:Utilities:Electricity";
        assert_eq!(category_leaf(name), "Electricity");
        assert_eq!(category_parent(name), Some("Home:Utilities"));
        assert_eq!(category_depth(name), 2);
        assert_eq!(category_ancestors(name), ["Home", "Home:Utilities"]);
        assert_eq!(category_leaf("Home"), "Home");
        assert_eq!(category_parent("Home"), None);
        assert_eq!(category_depth("Home"), 0);
        assert!(category_ancestors("Home").is_empty());
        assert_eq!(category_child("Home", "Rent"), "Home:Rent");
        assert_eq!(
            category_parent(&category_child("Home", "Rent")),
            Some("Home")
        );
        // A trailing separator names an empty last level.
        assert_eq!(category_leaf("Home:"), "");
        assert_eq!(category_parent("Home:"), Some("Home"));
    }

    #[test]
    fn descendant_test_is_exact_and_case_sensitive() {
        assert!(category_is_within("Home", "Home"));
        assert!(category_is_within("Home:Rent", "Home"));
        assert!(category_is_within("Home:Rent:Garage", "Home:Rent"));
        assert!(!category_is_within("Homework", "Home"));
        assert!(!category_is_within("Home", "Home:Rent"));
        assert!(!category_is_within("home:Rent", "Home"));
        assert!(!category_is_within("Other", "Home"));
    }

    #[test]
    fn ordering_ignores_case_and_puts_descendants_after_their_parent() {
        let mut names = vec![
            Some("Homework"),
            Some("home:Rent"),
            Some("Home"),
            None,
            Some("Home-Office"),
            Some("HOME:Garden"),
            Some("Auto"),
        ];
        names.sort_by(|a, b| category_cmp(*a, *b));
        assert_eq!(
            names,
            [
                None,
                Some("Auto"),
                Some("Home"),
                Some("HOME:Garden"),
                Some("home:Rent"),
                Some("Home-Office"),
                Some("Homework"),
            ]
        );
        // The separator sorts first only where the names first differ; a
        // blank (0x20) would otherwise come before it.
        assert_eq!(category_cmp(Some("A:B"), Some("A B")), Ordering::Less);
        assert_eq!(category_cmp(Some("A"), Some("A")), Ordering::Equal);
        assert_eq!(category_cmp(Some("b"), Some("A")), Ordering::Greater);
        assert_eq!(category_cmp(None, None), Ordering::Equal);
    }

    #[test]
    fn same_category_is_equality_ignoring_case() {
        assert!(same_category("Home:Rent", "HOME:rent"));
        assert!(same_category("Übrig:Café", "üBRIG:CAFÉ"));
        assert!(!same_category("Home:Rent", "Home: Rent"));
        assert!(!same_category("Home", "Home:Rent"));
        assert_eq!(category_key("Home:Rent"), "HOME:RENT");
        assert_eq!(category_key("Übrig:Café"), "ÜBRIG:CAFÉ");
        // No single upper-case character exists for these: kept.
        assert_eq!(category_key("Straße"), "STRAßE");
        assert_eq!(
            category_key("Home:Rent") == category_key("HOME:rent"),
            same_category("Home:Rent", "HOME:rent")
        );
    }

    #[test]
    fn special_categories_are_recognised_by_their_default_names() {
        for (name, special) in [
            ("(Umbuchung)", SpecialCategory::Reclassification),
            ("(Transfer)", SpecialCategory::Reclassification),
            ("(Transfert)", SpecialCategory::Reclassification),
            ("(nicht auswerten)", SpecialCategory::Excluded),
            ("(exclude)", SpecialCategory::Excluded),
            ("(ne pas évaluer)", SpecialCategory::Excluded),
            ("(Umsatzsteuer)", SpecialCategory::Vat),
            ("(VAT)", SpecialCategory::Vat),
        ] {
            assert_eq!(special_category_by_default_name(name), Some(special));
        }
        assert_eq!(special_category_by_default_name("(vat)"), None);
        assert_eq!(special_category_by_default_name("Umbuchung"), None);
        assert_eq!(
            Language::German.special_category_name(SpecialCategory::None),
            None
        );
        for language in Language::ALL {
            assert_eq!(
                language.special_category_name(SpecialCategory::Excluded),
                Some(language.excluded_category_name())
            );
        }
    }

    /// A category with the fields the loader rules and the budget look at.
    fn category(full_name: Option<&str>, special: u8) -> Category {
        Category {
            base: RecordBase {
                oid: 1,
                update_stamp: 2,
            },
            full_name: full_name.map(str::to_owned),
            stored_full_name: full_name.map(str::to_owned),
            layout_version: 9,
            description: None,
            rules: None,
            parsed_rules: Vec::new(),
            totals_bucket: TotalsBucket::Auto,
            stored_totals_bucket: TotalsBucket::Auto,
            budget_period: BudgetPeriod::Month,
            stored_budget_period: BudgetPeriod::Month,
            budget_amount: Decimal::ZERO,
            stored_budget_amount: Decimal::ZERO,
            vat_percent: Decimal::ZERO,
            icon_name: None,
            stored_icon_name: None,
            color_name: None,
            special_category: SpecialCategory::from_raw(special),
        }
    }

    #[test]
    fn loader_view_drops_nameless_records_and_keeps_one_special_of_each_kind() {
        let categories = [
            category(Some("Home"), 0),
            category(None, 2),
            category(Some("(nicht auswerten)"), 2),
            category(Some("Second"), 2),
            category(Some("(Umbuchung)"), 1),
            category(Some("Odd"), 7),
            category(Some("Odd too"), 7),
        ];
        let marks = category_loader_view(&categories);
        let dropped: Vec<bool> = marks.iter().map(|mark| mark.dropped).collect();
        assert_eq!(dropped, [false, true, false, false, false, false, false]);
        let special: Vec<u8> = marks
            .iter()
            .map(|mark| mark.special_category.raw())
            .collect();
        // The nameless record does not claim kind 2; the second record of
        // kind 2 loses it; values without a meaning are left alone.
        assert_eq!(special, [0, 0, 2, 0, 1, 7, 7]);
        assert_eq!(
            missing_special_categories(&categories),
            [SpecialCategory::Vat]
        );
    }

    #[test]
    fn special_categories_are_missing_only_from_a_catalogue_that_has_categories() {
        assert!(missing_special_categories(&[]).is_empty());
        assert!(missing_special_categories(&[category(None, 0)]).is_empty());
        assert_eq!(
            missing_special_categories(&[category(Some("Home"), 0)]),
            SpecialCategory::SPECIAL
        );
        let complete = [
            category(Some("a"), 3),
            category(Some("b"), 1),
            category(Some("c"), 2),
        ];
        assert!(missing_special_categories(&complete).is_empty());
    }

    #[test]
    fn budget_scales_with_the_period() {
        let budget = |period: u8, amount: &str| {
            let mut category = category(Some("Budget"), 0);
            category.budget_period = BudgetPeriod::from_raw(period);
            category.budget_amount = amount.parse().unwrap();
            category
        };
        let text = |value: Option<Decimal>| value.map(|value| value.normalize().to_string());

        assert_eq!(BudgetPeriod::Quarter.months(), 3);
        assert_eq!(BudgetPeriod::Year.months(), 12);
        for other in [0, 1, 2, 5, 6, 77] {
            assert_eq!(BudgetPeriod::from_raw(other).months(), 1);
        }

        assert_eq!(
            text(budget(2, "450.00").monthly_budget()).as_deref(),
            Some("450")
        );
        assert_eq!(
            text(budget(3, "450.00").monthly_budget()).as_deref(),
            Some("150")
        );
        assert_eq!(
            text(budget(4, "1200").monthly_budget()).as_deref(),
            Some("100")
        );
        assert_eq!(
            text(budget(4, "-1200").monthly_budget()).as_deref(),
            Some("-100")
        );

        assert_eq!(
            text(budget(2, "450").budget_for_months(6)).as_deref(),
            Some("2700")
        );
        assert_eq!(
            text(budget(3, "450").budget_for_months(6)).as_deref(),
            Some("900")
        );
        assert_eq!(
            text(budget(4, "1200").budget_for_months(6)).as_deref(),
            Some("600")
        );
        // A fixed total is not scaled by time.
        assert_eq!(
            text(budget(5, "1200").budget_for_months(6)).as_deref(),
            Some("1200")
        );
        assert_eq!(
            text(budget(5, "1200").budget_for_months(0)).as_deref(),
            Some("1200")
        );
        assert_eq!(
            text(budget(2, "450").budget_for_months(0)).as_deref(),
            Some("0")
        );

        // An amount at the edge of the type cannot be multiplied: no panic.
        let mut huge = budget(2, "0");
        huge.budget_amount = Decimal::MAX;
        assert_eq!(huge.budget_for_months(2), None);
        assert_eq!(huge.monthly_budget(), Some(Decimal::MAX));
    }

    #[test]
    fn accessors_read_the_hierarchy_of_the_record() {
        let nested = category(Some("Home:Rent"), 0);
        assert_eq!(nested.name(), Some("Rent"));
        assert_eq!(nested.parent(), Some("Home"));
        assert_eq!(nested.depth(), 1);
        assert_eq!(nested.key().as_deref(), Some("HOME:RENT"));
        let nameless = category(None, 0);
        assert_eq!(nameless.name(), None);
        assert_eq!(nameless.parent(), None);
        assert_eq!(nameless.depth(), 0);
        assert_eq!(nameless.key(), None);
    }
}
