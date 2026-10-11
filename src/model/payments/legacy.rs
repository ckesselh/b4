//! The legacy single-chunk layout of the payment family: fields that were
//! folded into tagged purpose lines (04 §5.8).
//!
//! A record without a second chunk has no extension fields on the wire.
//! Banking4 fills some of them from purpose lines of the form `TAG+value`
//! and removes those lines from the purpose. A real vault still holds such
//! records, so this is not optional.

use crate::wire::RawDate;

use super::base::PaymentBase;
use super::enums::{CostBearer, SchemeClass};

/// Purpose codes that Banking4 files as a category purpose instead (the
/// `PURP` row of 04 §5.8).
const CATEGORY_PURPOSE_CODES: [&str; 2] = ["CORT", "INTC"];
/// Text keys of a business-to-business direct debit (04 §5.8, `TXTS`).
const B2B_TEXT_KEYS: [&str; 6] = ["04", "74", "84", "08", "96", "97"];
/// Text keys of a core direct debit (04 §5.8, `TXTS`).
const CORE_TEXT_KEYS: [&str; 6] = ["05", "95", "09", "71", "81", "92"];

/// `Some(text)` unless the text is empty: a text without characters is "no
/// value" everywhere in the format (02 §4.8).
fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

/// Split a line into tag and value if it is tagged: at least five characters
/// with `+` as the fifth.
///
/// Banking4 counts 16-bit characters. Only lines that start with four ASCII
/// characters are recognised here, which gives the same result: every tag of
/// the table is ASCII, and a line with any other "tag" is kept unchanged
/// whether it counts as tagged or not.
fn tagged(line: &str) -> Option<(&str, &str)> {
    let head = line.as_bytes().get(..5)?;
    if !head.is_ascii() || head.last() != Some(&b'+') {
        return None;
    }
    Some((line.get(..4)?, line.get(5..)?))
}

/// Cut `text` after `units` characters as Banking4 counts them (UTF-16 code
/// units), or `None` if it is shorter. A cut through a character outside the
/// Basic Multilingual Plane leaves U+FFFD on both sides.
fn split_utf16(text: &str, units: usize) -> Option<(String, String)> {
    if text.is_ascii() {
        // The common case, without the detour over 16-bit units.
        let (head, tail) = (text.get(..units)?, text.get(units..)?);
        return Some((head.to_owned(), tail.to_owned()));
    }
    let encoded: Vec<u16> = text.encode_utf16().collect();
    let (head, tail) = (encoded.get(..units)?, encoded.get(units..)?);
    Some((
        String::from_utf16_lossy(head),
        String::from_utf16_lossy(tail),
    ))
}

/// The number a text key or its extension stands for: its value if it
/// consists of decimal digits only, otherwise 0.
fn key_number(text: Option<&str>) -> u32 {
    text.filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse().ok())
        .unwrap_or(0)
}

/// The purpose code of a SEPA debit for a legacy text key and its extension
/// (the first table of the `TXTS` rule in 04 §5.8).
fn purpose_code_of_text_key(key: u32, extension: u32) -> Option<&'static str> {
    Some(match key {
        6 | 106 | 182 | 190 => match extension {
            3 | 23 => "CDCD",
            73 => "MTUP",
            24 => "ETUP",
            _ => "DCRD",
        },
        107 => "CGDD",
        8 | 52 | 152 => "RINP",
        53 | 80 | 153 => "SALA",
        54 | 154 => "CBFF",
        155 => "CBFR",
        56 | 156 => "GOVT",
        17 | 67 | 167 => "IVPT",
        19 | 69 | 119 | 169 => "CHAR",
        82 | 83 => "CDCD",
        801..=813 => "NCHG",
        _ => return None,
    })
}

/// The number written in one part of a text of decimal digits.
fn digits<T: std::str::FromStr>(text: &str, range: std::ops::Range<usize>) -> Option<T> {
    text.get(range)?.parse().ok()
}

/// The date of an `MDOS` or `MDAT` value: `Ok(None)` for an empty value or
/// eight zeros, `Ok(Some(_))` for eight digits with a plausible month and
/// day, `Err(())` for anything else, which leaves the field as it was.
///
/// The parts are not checked against the calendar: like every `date_raw`
/// value the result can be 31 February.
fn mandate_date(value: &str) -> Result<Option<RawDate>, ()> {
    if value.is_empty() || value == "00000000" {
        return Ok(None);
    }
    if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    let year: i16 = digits(value, 0..4).ok_or(())?;
    let month: u8 = digits(value, 4..6).ok_or(())?;
    let day: u8 = digits(value, 6..8).ok_or(())?;
    if year == 0 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(());
    }
    Ok(Some(RawDate { year, month, day }))
}

/// The first two non-empty parts of an address value, which separates its
/// parts with U+0001 (`PADR`, `BADR`).
fn address(value: &str) -> (Option<String>, Option<String>) {
    let mut parts = value.split('\u{1}').filter(|part| !part.is_empty());
    (
        parts.next().map(str::to_owned),
        parts.next().map(str::to_owned),
    )
}

/// An end-to-end or payment information reference: trimmed, and "no value"
/// for the placeholders banks send instead of one.
fn reference(value: &str) -> Option<String> {
    let value = value.trim();
    (!matches!(value, "" | "NONREF" | "NOTPROVIDED")).then(|| value.to_owned())
}

/// Apply the `TXTS` rule: what a legacy text key means depends on the class
/// and direction of the record.
fn apply_text_key(base: &mut PaymentBase, value: &str) {
    let Some((key, rest)) = split_utf16(value, 2) else {
        return;
    };
    // The extension is characters three to five, and only of a value with at
    // least five characters.
    let extension = split_utf16(&rest, 3).map(|(extension, _)| extension);
    match (base.stored_scheme_class, base.debit_flag) {
        (SchemeClass::Sepa, true) => {
            let code =
                purpose_code_of_text_key(key_number(Some(&key)), key_number(extension.as_deref()));
            base.purpose_code = code.map(str::to_owned);
        }
        (SchemeClass::Sepa, false) => {
            // Both are assigned even when the key says nothing.
            base.local_instrument_code = if B2B_TEXT_KEYS.contains(&key.as_str()) {
                Some("B2B".to_owned())
            } else if CORE_TEXT_KEYS.contains(&key.as_str()) {
                Some("CORE".to_owned())
            } else {
                None
            };
            base.sequence_type = match extension.as_deref() {
                Some("991") => Some("FRST"),
                Some("992") => Some("RCUR"),
                Some("993") => Some("OOFF"),
                Some("994") => Some("FNAL"),
                _ => None,
            }
            .map(str::to_owned);
        }
        (SchemeClass::ForeignLegacy, _) => base.intl_remittance_method = non_empty(&key),
        _ => {}
    }
}

/// Apply the `PURP` rule: a purpose code, a category purpose code, or a
/// purpose code followed by text that belongs to the purpose.
fn apply_purpose_code(base: &mut PaymentBase, value: &str, lines: &mut Vec<String>) {
    match split_utf16(value, 4) {
        Some((code, rest)) if !rest.is_empty() => {
            base.purpose_code = Some(code);
            lines.push(rest);
        }
        _ if CATEGORY_PURPOSE_CODES.contains(&value) => {
            base.category_purpose_code = Some(value.to_owned());
        }
        _ => base.purpose_code = non_empty(value),
    }
}

/// Move the tagged purpose lines of a legacy record into the extension
/// fields (04 §5.8) and leave the remaining lines as the purpose.
///
/// `base` holds the head fields of 04 §5.1 and extension fields at their
/// defaults. A tag that assigns a field overwrites it each time it occurs.
pub(super) fn apply_tagged_lines(base: &mut PaymentBase) {
    let lines = std::mem::take(&mut base.purpose_lines);
    let mut kept = Vec::with_capacity(lines.len());
    // `Some` once the tag was seen, whatever its value.
    let mut deviating_originator: Option<String> = None;
    let mut deviating_recipient: Option<String> = None;

    for line in lines {
        let Some((tag, value)) = tagged(&line) else {
            kept.push(line);
            continue;
        };
        match tag {
            "EREF" => base.end_to_end_id = reference(value),
            "KREF" => base.payment_info_id = reference(value),
            "MREF" => base.mandate_id = non_empty(value),
            "CRED" | "DEBT" => base.creditor_id = non_empty(value),
            "SVWZ" => kept.extend(non_empty(value)),
            "TXTS" => apply_text_key(base, value),
            "MDOS" | "MDAT" => {
                if let Ok(date) = mandate_date(value) {
                    base.mandate_signed_date = date;
                }
            }
            "PADR" => (base.counterparty_street, base.counterparty_town) = address(value),
            "BNAM" => base.counterparty_bank_name = non_empty(value),
            "BADR" => {
                (base.counterparty_bank_street, base.counterparty_bank_town) = address(value);
            }
            "PTCB" => {
                base.cost_bearer = match value {
                    "OUR" => CostBearer::Ours,
                    "SHA" => CostBearer::Shared,
                    "BEN" => CostBearer::Beneficiary,
                    _ => CostBearer::None,
                };
            }
            "PTCA" => base.cost_bearer_account_oid = value.trim().parse().unwrap_or(0),
            "PTMT" => base.intl_remittance_method = non_empty(value),
            "PT20" => base.intl_remittance_instruction = non_empty(value),
            "PURP" => apply_purpose_code(base, value, &mut kept),
            "CPCD" => base.category_purpose_code = non_empty(value),
            "LCIC" => base.local_instrument_code = non_empty(value),
            "SQTP" => base.sequence_type = non_empty(value),
            "ABWA" => deviating_originator = Some(value.to_owned()),
            "ABWE" => deviating_recipient = Some(value.to_owned()),
            _ => kept.push(line),
        }
    }

    // Which of the two deviating parties is the other side of this payment
    // depends on who collects: with a creditor id the record is a direct
    // debit. The party that is not the other side stays in the purpose.
    if deviating_originator.is_some() || deviating_recipient.is_some() {
        let (ultimate, other_tag, other) = if base.creditor_id.is_some() == base.debit_flag {
            (deviating_originator, "ABWE+", deviating_recipient)
        } else {
            (deviating_recipient, "ABWA+", deviating_originator)
        };
        base.ultimate_counterparty_name = ultimate.filter(|name| !name.is_empty());
        if let Some(other) = other {
            kept.push(format!("{other_tag}{other}"));
        }
    }
    base.purpose_lines = kept;
}

#[cfg(test)]
mod tests {
    use super::super::base::SchemeRule;
    use super::super::enums::SchemeType;
    use super::super::testing::{record, Payload};
    use super::*;
    use crate::wire::Reader;

    /// First byte of the base: class sepa with the debit bit set and clear.
    const SEPA_DEBIT: u8 = 0x11;
    const SEPA_CREDIT: u8 = 0x10;

    /// The payment base of a legacy record with this first byte and purpose
    /// text, read as a payment's (the scheme is derived).
    fn legacy(class_and_direction: u8, purpose: &str) -> PaymentBase {
        let chunk = Payload::default()
            .i32(1)
            .i32(2)
            .u8(class_and_direction)
            .cents(100)
            .code(3, "EUR")
            .string("name")
            .string("")
            .string("")
            .code(2, "")
            .string(purpose)
            .date(0, 0, 0)
            .string("")
            .string("");
        let body = record(&[chunk]);
        let mut reader = Reader::new(&body).expect("a well-formed test record");
        let base = PaymentBase::read(&mut reader, SchemeRule::DeriveIfMissing)
            .expect("a decodable test record");
        assert!(base.legacy);
        base
    }

    #[test]
    fn every_tag_fills_its_field_and_leaves_the_purpose() {
        let base = legacy(
            SEPA_DEBIT,
            "before\nEREF+ e2e \nKREF+ batch\nMREF+ mandate \nCRED+creditor\nSVWZ+text\n\
             MDAT+20240229\nPADR+street\u{1}town\nBNAM+bank\nBADR+bank street\u{1}bank town\n\
             PTCB+BEN\nPTCA+ 42 \nPTMT+method\nPT20+instruction\nPURP+SALA\nCPCD+CASH\n\
             LCIC+CORE\nSQTP+RCUR\nafter",
        );
        assert_eq!(base.purpose_lines, ["before", "text", "after"]);
        assert_eq!(base.end_to_end_id.as_deref(), Some("e2e"));
        assert_eq!(base.payment_info_id.as_deref(), Some("batch"));
        // Verbatim, unlike the two references.
        assert_eq!(base.mandate_id.as_deref(), Some(" mandate "));
        assert_eq!(base.creditor_id.as_deref(), Some("creditor"));
        assert_eq!(
            base.mandate_signed_date,
            Some(RawDate {
                year: 2024,
                month: 2,
                day: 29
            })
        );
        assert_eq!(base.counterparty_street.as_deref(), Some("street"));
        assert_eq!(base.counterparty_town.as_deref(), Some("town"));
        assert_eq!(base.counterparty_bank_name.as_deref(), Some("bank"));
        assert_eq!(
            base.counterparty_bank_street.as_deref(),
            Some("bank street")
        );
        assert_eq!(base.counterparty_bank_town.as_deref(), Some("bank town"));
        assert_eq!(base.cost_bearer, CostBearer::Beneficiary);
        assert_eq!(base.cost_bearer_account_oid, 42);
        assert_eq!(base.intl_remittance_method.as_deref(), Some("method"));
        assert_eq!(
            base.intl_remittance_instruction.as_deref(),
            Some("instruction")
        );
        assert_eq!(base.purpose_code.as_deref(), Some("SALA"));
        assert_eq!(base.category_purpose_code.as_deref(), Some("CASH"));
        assert_eq!(base.local_instrument_code.as_deref(), Some("CORE"));
        assert_eq!(base.sequence_type.as_deref(), Some("RCUR"));
        assert_eq!(base.ultimate_counterparty_name, None);
        // Fields the legacy form cannot carry.
        assert_eq!(base.stored_scheme_type, SchemeType::None);
        assert_eq!(base.scheduled_time, None);
        assert_eq!(base.regulatory_reporting_code, None);
    }

    #[test]
    fn the_last_occurrence_of_an_assigning_tag_wins_and_appending_tags_append() {
        let base = legacy(
            SEPA_DEBIT,
            "MREF+first\nSVWZ+one\nMREF+second\nSVWZ+\nSVWZ+two\nDEBT+a\nCRED+b\nMDOS+20200101\n\
             MDAT+nonsense\nPTCB+OUR\nPTCB+other\nPTCA+7\nPTCA+seven",
        );
        assert_eq!(base.mandate_id.as_deref(), Some("second"));
        assert_eq!(base.purpose_lines, ["one", "two"]);
        assert_eq!(base.creditor_id.as_deref(), Some("b"));
        // A value that is not a date leaves the earlier one.
        assert_eq!(base.mandate_signed_date.map(|date| date.year), Some(2020));
        assert_eq!(base.cost_bearer, CostBearer::None);
        assert_eq!(base.cost_bearer_account_oid, 0);
    }

    #[test]
    fn an_empty_date_value_clears_the_date() {
        let base = legacy(SEPA_DEBIT, "MDAT+20200101\nMDOS+00000000");
        assert_eq!(base.mandate_signed_date, None);
        let base = legacy(SEPA_DEBIT, "MDAT+20200101\nMDAT+");
        assert_eq!(base.mandate_signed_date, None);
    }

    #[test]
    fn unknown_tags_and_untagged_lines_stay_in_the_purpose() {
        let base = legacy(
            SEPA_DEBIT,
            "XXXX+kept\nabc+not tagged\nEREF\nÄÖÜß+umlauts\n+++++\n    +blank tag",
        );
        assert_eq!(
            base.purpose_lines,
            [
                "XXXX+kept",
                "abc+not tagged",
                "EREF",
                "ÄÖÜß+umlauts",
                "+++++",
                "    +blank tag"
            ]
        );
    }

    #[test]
    fn a_purpose_code_can_carry_text_or_be_a_category_purpose() {
        let base = legacy(SEPA_DEBIT, "PURP+SALAmore text");
        assert_eq!(base.purpose_code.as_deref(), Some("SALA"));
        assert_eq!(base.purpose_lines, ["more text"]);

        for code in ["CORT", "INTC"] {
            let base = legacy(SEPA_DEBIT, &format!("PURP+{code}"));
            assert_eq!(base.category_purpose_code.as_deref(), Some(code));
            assert_eq!(base.purpose_code, None);
        }
        let base = legacy(SEPA_DEBIT, "PURP+RINP\nPURP+AB");
        assert_eq!(base.purpose_code.as_deref(), Some("AB"));
        assert_eq!(base.category_purpose_code, None);
        // Longer than four characters is never a category purpose.
        let base = legacy(SEPA_DEBIT, "PURP+CORTx");
        assert_eq!(base.purpose_code.as_deref(), Some("CORT"));
        assert_eq!(base.category_purpose_code, None);
        assert_eq!(base.purpose_lines, ["x"]);
    }

    #[test]
    fn a_text_key_means_a_purpose_code_for_a_sepa_debit() {
        let base = legacy(SEPA_DEBIT, "TXTS+53");
        assert_eq!(base.purpose_code.as_deref(), Some("SALA"));
        // The extension counts only in a value of five characters or more.
        assert_eq!(
            legacy(SEPA_DEBIT, "TXTS+06073").purpose_code.as_deref(),
            Some("MTUP")
        );
        assert_eq!(
            legacy(SEPA_DEBIT, "TXTS+0607").purpose_code.as_deref(),
            Some("DCRD")
        );
        // A key without a code clears a purpose code set before.
        assert_eq!(legacy(SEPA_DEBIT, "PURP+RINP\nTXTS+51").purpose_code, None);
        // Too short to be a key: ignored altogether.
        assert_eq!(
            legacy(SEPA_DEBIT, "PURP+RINP\nTXTS+5")
                .purpose_code
                .as_deref(),
            Some("RINP")
        );
        assert!(base.purpose_lines.is_empty());
    }

    #[test]
    fn a_text_key_means_instrument_and_sequence_for_a_sepa_credit() {
        let base = legacy(SEPA_CREDIT, "TXTS+04992");
        assert_eq!(base.local_instrument_code.as_deref(), Some("B2B"));
        assert_eq!(base.sequence_type.as_deref(), Some("RCUR"));
        assert_eq!(base.purpose_code, None);
        // The instrument from the key feeds the scheme (04 §5.4).
        assert_eq!(base.scheme_type, SchemeType::SepaDirectDebitB2b);

        let base = legacy(SEPA_CREDIT, "TXTS+05");
        assert_eq!(base.local_instrument_code.as_deref(), Some("CORE"));
        assert_eq!(base.sequence_type, None);
        assert_eq!(base.scheme_type, SchemeType::SepaDirectDebitCore);

        // Both are assigned even when the key says nothing.
        let base = legacy(SEPA_CREDIT, "LCIC+B2B\nSQTP+FRST\nTXTS+00000");
        assert_eq!(base.local_instrument_code, None);
        assert_eq!(base.sequence_type, None);
        for (extension, sequence) in [("991", "FRST"), ("993", "OOFF"), ("994", "FNAL")] {
            let base = legacy(SEPA_CREDIT, &format!("TXTS+09{extension}"));
            assert_eq!(base.sequence_type.as_deref(), Some(sequence));
        }
    }

    #[test]
    fn a_text_key_means_the_method_for_a_foreign_payment_and_nothing_otherwise() {
        let foreign = legacy(0x21, "TXTS+AB123");
        assert_eq!(foreign.intl_remittance_method.as_deref(), Some("AB"));
        assert_eq!(foreign.purpose_code, None);
        for class in [0x01, 0x31, 0x41, 0xF0] {
            let base = legacy(class, "TXTS+53991");
            assert_eq!(base.purpose_code, None);
            assert_eq!(base.local_instrument_code, None);
            assert_eq!(base.sequence_type, None);
            assert_eq!(base.intl_remittance_method, None);
            assert!(base.purpose_lines.is_empty());
        }
    }

    #[test]
    fn deviating_parties_go_by_creditor_id_and_direction() {
        // (first byte, creditor id present, expected ultimate party, line kept)
        let cases = [
            (SEPA_DEBIT, true, "originator", "ABWE+recipient"),
            (SEPA_CREDIT, false, "originator", "ABWE+recipient"),
            (SEPA_DEBIT, false, "recipient", "ABWA+originator"),
            (SEPA_CREDIT, true, "recipient", "ABWA+originator"),
        ];
        for (first_byte, creditor, ultimate, kept) in cases {
            let creditor_line = if creditor { "CRED+id\n" } else { "" };
            let base = legacy(
                first_byte,
                &format!("{creditor_line}ABWA+originator\ntext\nABWE+recipient"),
            );
            assert_eq!(
                base.ultimate_counterparty_name.as_deref(),
                Some(ultimate),
                "{first_byte:#x} {creditor}"
            );
            // The other party goes back into the purpose, at its end.
            assert_eq!(base.purpose_lines, ["text", kept]);
        }
    }

    #[test]
    fn one_deviating_party_alone_may_leave_the_ultimate_party_empty() {
        // A debit without a creditor id takes the recipient; only the
        // originator was given, so it goes back into the purpose.
        let base = legacy(SEPA_DEBIT, "ABWA+originator");
        assert_eq!(base.ultimate_counterparty_name, None);
        assert_eq!(base.purpose_lines, ["ABWA+originator"]);
        let base = legacy(SEPA_DEBIT, "ABWE+recipient");
        assert_eq!(
            base.ultimate_counterparty_name.as_deref(),
            Some("recipient")
        );
        assert!(base.purpose_lines.is_empty());
        // Without either tag nothing is assigned.
        let base = legacy(SEPA_DEBIT, "text");
        assert_eq!(base.ultimate_counterparty_name, None);
        assert_eq!(base.purpose_lines, ["text"]);
    }

    #[test]
    fn an_empty_tag_value_is_no_value() {
        let base = legacy(
            SEPA_DEBIT,
            "MREF+x\nMREF+\nCRED+\nBNAM+\nPTMT+\nPT20+\nCPCD+\nLCIC+\nSQTP+\nPURP+\nEREF+\nPADR+",
        );
        assert_eq!(base.mandate_id, None);
        assert_eq!(base.creditor_id, None);
        assert_eq!(base.counterparty_bank_name, None);
        assert_eq!(base.purpose_code, None);
        assert_eq!(base.end_to_end_id, None);
        assert_eq!(base.counterparty_street, None);
        assert!(base.purpose_lines.is_empty());
        // An empty creditor id is none for the deviating parties as well.
        let base = legacy(SEPA_DEBIT, "CRED+\nABWA+originator\nABWE+recipient");
        assert_eq!(
            base.ultimate_counterparty_name.as_deref(),
            Some("recipient")
        );
    }

    #[test]
    fn a_line_is_tagged_only_with_a_plus_as_fifth_character() {
        assert_eq!(tagged("EREF+abc"), Some(("EREF", "abc")));
        assert_eq!(tagged("EREF+"), Some(("EREF", "")));
        assert_eq!(tagged("abc+x"), None);
        assert_eq!(tagged("EREF"), None);
        assert_eq!(tagged(""), None);
        // A multi-byte character among the first five is never a known tag;
        // the line must come back untagged instead of being cut inside it.
        assert_eq!(tagged("Müll+x"), None);
        assert_eq!(tagged("äöü"), None);
    }

    #[test]
    fn text_is_cut_by_sixteen_bit_characters() {
        assert_eq!(
            split_utf16("SALAtext", 4),
            Some(("SALA".into(), "text".into()))
        );
        assert_eq!(split_utf16("SALA", 4), Some(("SALA".into(), String::new())));
        assert_eq!(split_utf16("SAL", 4), None);
        assert_eq!(split_utf16("äöüßx", 4), Some(("äöüß".into(), "x".into())));
        // U+1F600 takes two 16-bit characters.
        assert_eq!(
            split_utf16("\u{1F600}\u{1F600}x", 4),
            Some(("\u{1F600}\u{1F600}".into(), "x".into()))
        );
        assert_eq!(
            split_utf16("a\u{1F600}\u{1F600}", 4),
            Some(("a\u{1F600}\u{FFFD}".into(), "\u{FFFD}".into()))
        );
    }

    #[test]
    fn text_keys_are_numbers_only_when_all_digits() {
        assert_eq!(key_number(Some("53")), 53);
        assert_eq!(key_number(Some("06")), 6);
        assert_eq!(key_number(Some("+6")), 0);
        assert_eq!(key_number(Some(" 6")), 0);
        assert_eq!(key_number(Some("")), 0);
        assert_eq!(key_number(None), 0);
    }

    #[test]
    fn purpose_codes_follow_the_text_key_table() {
        assert_eq!(purpose_code_of_text_key(6, 0), Some("DCRD"));
        assert_eq!(purpose_code_of_text_key(6, 3), Some("CDCD"));
        assert_eq!(purpose_code_of_text_key(6, 23), Some("CDCD"));
        assert_eq!(purpose_code_of_text_key(6, 73), Some("MTUP"));
        assert_eq!(purpose_code_of_text_key(6, 24), Some("ETUP"));
        assert_eq!(purpose_code_of_text_key(8, 0), Some("RINP"));
        assert_eq!(purpose_code_of_text_key(52, 0), Some("RINP"));
        assert_eq!(purpose_code_of_text_key(53, 0), Some("SALA"));
        assert_eq!(purpose_code_of_text_key(80, 0), Some("SALA"));
        assert_eq!(purpose_code_of_text_key(54, 0), Some("CBFF"));
        assert_eq!(purpose_code_of_text_key(56, 0), Some("GOVT"));
        assert_eq!(purpose_code_of_text_key(17, 0), Some("IVPT"));
        assert_eq!(purpose_code_of_text_key(67, 0), Some("IVPT"));
        assert_eq!(purpose_code_of_text_key(19, 0), Some("CHAR"));
        assert_eq!(purpose_code_of_text_key(69, 0), Some("CHAR"));
        assert_eq!(purpose_code_of_text_key(82, 0), Some("CDCD"));
        assert_eq!(purpose_code_of_text_key(83, 0), Some("CDCD"));
        assert_eq!(purpose_code_of_text_key(107, 0), Some("CGDD"));
        assert_eq!(purpose_code_of_text_key(155, 0), Some("CBFR"));
        assert_eq!(purpose_code_of_text_key(801, 0), Some("NCHG"));
        assert_eq!(purpose_code_of_text_key(813, 0), Some("NCHG"));
        assert_eq!(purpose_code_of_text_key(814, 0), None);
        assert_eq!(purpose_code_of_text_key(51, 0), None);
        assert_eq!(purpose_code_of_text_key(0, 0), None);
    }

    #[test]
    fn mandate_dates_are_eight_digits_with_plausible_parts() {
        let date = |year, month, day| Ok(Some(RawDate { year, month, day }));
        assert_eq!(mandate_date("20110215"), date(2011, 2, 15));
        // Not checked against the calendar.
        assert_eq!(mandate_date("20110231"), date(2011, 2, 31));
        assert_eq!(mandate_date("99991231"), date(9999, 12, 31));
        assert_eq!(mandate_date(""), Ok(None));
        assert_eq!(mandate_date("00000000"), Ok(None));
        for ignored in [
            "2011021",
            "201102150",
            "2011-2-5",
            "00000101",
            "20110015",
            "20111315",
            "20110200",
            "20110232",
            "２０１１０２１５",
        ] {
            assert_eq!(mandate_date(ignored), Err(()), "{ignored}");
        }
    }

    #[test]
    fn addresses_take_the_first_two_non_empty_parts() {
        assert_eq!(
            address("street\u{1}\u{1}town\u{1}more"),
            (Some("street".into()), Some("town".into()))
        );
        assert_eq!(address("street"), (Some("street".into()), None));
        assert_eq!(address("\u{1}\u{1}"), (None, None));
        assert_eq!(address(""), (None, None));
    }

    #[test]
    fn references_drop_the_placeholders() {
        assert_eq!(reference(" abc "), Some("abc".into()));
        assert_eq!(reference(" NONREF "), None);
        assert_eq!(reference("NOTPROVIDED"), None);
        assert_eq!(reference("   "), None);
        assert_eq!(reference("nonref"), Some("nonref".into()));
    }
}
