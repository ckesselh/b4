//! The keyword expression of a category (05 §4.5): the text Banking4 uses to
//! suggest a category for a booking.
//!
//! Only the structure is decoded here. How a rule is matched against a
//! booking, and what the characters `#`, `?` and `*` mean in a keyword, is
//! not established (05 §9 question 7).

use serde::Serialize;

/// The marker that joins the keywords of one rule.
const AND: char = '&';
/// The marker in front of a negative rule.
const NOT: char = '~';
/// The joining marker of layout versions below [`CURRENT_MARKERS_SINCE`].
const LEGACY_AND: char = '+';
/// A marker that is let through in every layout version.
const DIGIT: char = '#';
/// First `layout_version` that uses `&` and `~`; before it the joining marker
/// was `+`, and `&` and `~` were separators.
const CURRENT_MARKERS_SINCE: i32 = 4;
/// A piece and a keyword need at least this many characters.
const MIN_LENGTH: usize = 2;

/// One rule of a category: keywords that, by the name of the joining marker,
/// must all be present.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct CategoryRule {
    /// Whether the rule was written with a leading `~`.
    pub negative: bool,
    /// The keywords, lower-case, each at least two characters, sorted by
    /// code point.
    pub keywords: Vec<String>,
}

/// What the parser does with one character of a rule text (05 §4.5,
/// "Characters"). Markers are handled before this table is asked.
fn keyword_char(c: char) -> Option<char> {
    match c {
        '0'..='9' | 'a'..='z' | 'ä' | 'ö' | 'ü' | 'ß' | '*' | '+' | ',' | '-' | '.' | '/' | '?' => {
            Some(c)
        }
        'A'..='Z' => Some(c.to_ascii_lowercase()),
        'Ä' => Some('ä'),
        'Ö' => Some('ö'),
        'Ü' => Some('ü'),
        // White space, control characters, all other punctuation, every
        // other letter of Latin-1 and everything above U+00FF separate.
        _ => None,
    }
}

/// Parse the rule text of a category (05 §4.5). `layout_version` selects the
/// marker set: `&` and `~` from version 4 on, `+` alone before. `None` and a
/// text without any rule give no rules; no text fails to parse.
///
/// The result has the positive rules first, then the negative ones; each
/// group and the keywords of each rule are sorted by code point, and
/// duplicate rules are kept. Banking4 sorts by a language-dependent string
/// comparison instead, so its order can differ for keywords with characters
/// other than `a` to `z`: compare rule sets, not sequences.
#[must_use]
pub fn parse_category_rules(text: Option<&str>, layout_version: i32) -> Vec<CategoryRule> {
    let Some(text) = text else {
        return Vec::new();
    };
    let current = layout_version >= CURRENT_MARKERS_SINCE;
    let and = if current { AND } else { LEGACY_AND };

    let mut rules = Vec::new();
    let mut piece = String::new();
    // The chained `None` flushes the last piece like a separator would.
    for c in text.chars().map(Some).chain([None]) {
        let kept = c.and_then(|c| {
            let is_marker = c == DIGIT || (current && (c == AND || c == NOT));
            if is_marker {
                Some(c)
            } else {
                keyword_char(c)
            }
        });
        match kept {
            Some(c) => piece.push(c),
            None => {
                rules.extend(rule_of_piece(&piece, and));
                piece.clear();
            }
        }
    }
    // A stable sort by (negative, keywords): positive rules first.
    rules.sort();
    rules
}

/// The rule one piece of the text stands for, if any (05 §4.5, "From pieces
/// to rules").
fn rule_of_piece(piece: &str, and: char) -> Option<CategoryRule> {
    if piece.chars().count() < MIN_LENGTH {
        return None;
    }
    // Only as the first character of a piece does `~` negate; anywhere else
    // it is part of a keyword. Below version 4 it never gets this far.
    let (negative, rest) = match piece.strip_prefix(NOT) {
        Some(rest) => (true, rest),
        None => (false, piece),
    };
    let mut keywords: Vec<String> = rest
        .split(and)
        .filter(|keyword| keyword.chars().count() >= MIN_LENGTH)
        .map(str::to_owned)
        .collect();
    if keywords.is_empty() {
        return None;
    }
    keywords.sort();
    Some(CategoryRule { negative, keywords })
}

/// The text Banking4 would write for these rules (05 §4.5, "Canonical
/// form"): the positive rules joined by one space, then one space and the
/// negative rules, each with a leading `~`; `None` for no rules. With
/// negative rules only, the text starts with a space.
///
/// The order is the one of `rules`, which for a parsed list is by code
/// point. Banking4's own order depends on its language settings, so the text
/// equals what Banking4 writes only when all keywords consist of `a` to `z`.
#[must_use]
pub fn canonical_rules_text(rules: &[CategoryRule]) -> Option<String> {
    if rules.is_empty() {
        return None;
    }
    let render = |negative: bool| -> Vec<String> {
        rules
            .iter()
            .filter(|rule| rule.negative == negative)
            .map(|rule| {
                let keywords = rule.keywords.join("&");
                if negative {
                    format!("{NOT}{keywords}")
                } else {
                    keywords
                }
            })
            .collect()
    };
    let mut text = render(false).join(" ");
    let negative = render(true);
    if !negative.is_empty() {
        text.push(' ');
        text.push_str(&negative.join(" "));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(negative: bool, keywords: &[&str]) -> CategoryRule {
        CategoryRule {
            negative,
            keywords: keywords.iter().map(|&keyword| keyword.to_owned()).collect(),
        }
    }

    fn canonical(text: &str, version: i32) -> Option<String> {
        canonical_rules_text(&parse_category_rules(Some(text), version))
    }

    #[test]
    fn rules_are_folded_split_and_sorted() {
        assert_eq!(
            parse_category_rules(Some("Rent&FLAT landlord ~Janitor"), 9),
            [
                rule(false, &["flat", "rent"]),
                rule(false, &["landlord"]),
                rule(true, &["janitor"]),
            ]
        );
        assert_eq!(
            canonical("Rent&FLAT landlord ~Janitor", 9).as_deref(),
            Some("flat&rent landlord ~janitor")
        );
    }

    #[test]
    fn no_text_and_a_text_without_rules_give_no_rules() {
        assert!(parse_category_rules(None, 9).is_empty());
        for text in ["", " ", "a", "~q", "x & y", "é à ñ", "&&", "~", "~&"] {
            assert!(parse_category_rules(Some(text), 9).is_empty(), "{text:?}");
            assert_eq!(canonical(text, 9), None);
        }
        assert_eq!(canonical_rules_text(&[]), None);
    }

    #[test]
    fn short_pieces_and_keywords_are_dropped() {
        // 05 §4.5 step 3.
        assert_eq!(
            parse_category_rules(Some("x&flat"), 9),
            [rule(false, &["flat"])]
        );
        assert_eq!(
            parse_category_rules(Some("ab&c&de"), 9),
            [rule(false, &["ab", "de"])]
        );
        // Duplicate rules are kept.
        assert_eq!(
            canonical("flat rent rent ~janitor x y", 9).as_deref(),
            Some("flat rent rent ~janitor")
        );
    }

    #[test]
    fn negative_rules_follow_the_positive_ones() {
        assert_eq!(
            canonical("~janitor ~cleaning&stairs", 9).as_deref(),
            Some(" ~cleaning&stairs ~janitor")
        );
        assert_eq!(
            canonical("~zz aa ~bb cc", 9).as_deref(),
            Some("aa cc ~bb ~zz")
        );
        // A rule that is the beginning of another one comes first.
        assert_eq!(
            canonical("abc ab&cd ab", 9).as_deref(),
            Some("ab ab&cd abc")
        );
    }

    #[test]
    fn only_the_listed_characters_make_keywords() {
        // Every other letter of Latin-1 separates: `café` is cut after `caf`.
        assert_eq!(
            parse_category_rules(Some("café"), 9),
            [rule(false, &["caf"])]
        );
        assert_eq!(
            parse_category_rules(Some("MÜLL Öl ÄRZTE straße"), 9),
            [
                rule(false, &["müll"]),
                rule(false, &["straße"]),
                rule(false, &["ärzte"]),
                rule(false, &["öl"]),
            ]
        );
        assert_eq!(
            parse_category_rules(Some("a+b,c-d inv#?* 12.5/x"), 9),
            [
                rule(false, &["12.5/x"]),
                rule(false, &["a+b,c-d"]),
                rule(false, &["inv#?*"]),
            ]
        );
        for separator in [
            ' ', '\t', '\n', '\0', '!', '"', '(', ')', ':', ';', '=', '@', '[', '_', '|', 'é', 'ÿ',
            '\u{a0}', '€', 'ł', '日',
        ] {
            let text = format!("ab{separator}cd");
            assert_eq!(
                parse_category_rules(Some(&text), 9),
                [rule(false, &["ab"]), rule(false, &["cd"])],
                "{separator:?}"
            );
        }
    }

    #[test]
    fn tilde_negates_only_at_the_start_of_a_piece() {
        assert_eq!(
            parse_category_rules(Some("ab~cd"), 9),
            [rule(false, &["ab~cd"])]
        );
        assert_eq!(
            parse_category_rules(Some("~~ab"), 9),
            [rule(true, &["~ab"])]
        );
        // The ambiguity the document describes: a positive rule whose first
        // keyword starts with `~` is written like a negative one.
        let rules = parse_category_rules(Some("zz&~ab"), 9);
        assert_eq!(rules, [rule(false, &["zz", "~ab"])]);
        assert_eq!(canonical_rules_text(&rules).as_deref(), Some("zz&~ab"));
        let rules = parse_category_rules(Some("ab&~ab"), 9);
        assert_eq!(canonical_rules_text(&rules).as_deref(), Some("ab&~ab"));
    }

    #[test]
    fn layout_versions_below_four_join_with_plus() {
        assert_eq!(
            canonical("rent+flat landlord", 3).as_deref(),
            Some("flat&rent landlord")
        );
        // The old negative marker is not converted.
        assert_eq!(
            parse_category_rules(Some("-janitor"), 3),
            [rule(false, &["-janitor"])]
        );
        // `&` and `~` separate in those versions.
        assert_eq!(
            parse_category_rules(Some("ab&cd~ef"), 3),
            [
                rule(false, &["ab"]),
                rule(false, &["cd"]),
                rule(false, &["ef"])
            ]
        );
        assert_eq!(parse_category_rules(Some("~ab"), 0), [rule(false, &["ab"])]);
        // From version 4 on `+` is an ordinary keyword character.
        assert_eq!(
            parse_category_rules(Some("rent+flat"), 4),
            [rule(false, &["rent+flat"])]
        );
        assert_eq!(
            parse_category_rules(Some("ab#cd"), 3),
            [rule(false, &["ab#cd"])]
        );
        assert_eq!(
            parse_category_rules(Some("rent+flat"), i32::MIN),
            [rule(false, &["flat", "rent"])]
        );
    }
}
