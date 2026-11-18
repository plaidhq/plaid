//! Unicode helpers matching the Swift/Foundation semantics the original app relied on.
//! Swift `Character` is an extended grapheme cluster; `count` and indices are in graphemes.
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};
use unicode_segmentation::UnicodeSegmentation;

pub fn graphemes(s: &str) -> Vec<&str> {
    s.graphemes(true).collect()
}

pub fn count(s: &str) -> usize {
    s.graphemes(true).count()
}

/// Foundation `.whitespaces`: Unicode Zs plus tab.
pub fn is_ws(c: char) -> bool {
    c == '\t' || c.general_category() == unicode_properties::GeneralCategory::SpaceSeparator
}

/// Foundation `.whitespacesAndNewlines`.
pub fn is_ws_nl(c: char) -> bool {
    is_ws(c) || is_newline(c)
}

/// Foundation `.newlines`: U+000A–U+000D, U+0085, U+2028, U+2029.
pub fn is_newline(c: char) -> bool {
    matches!(
        c,
        '\n' | '\u{b}' | '\u{c}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

pub fn trim_ws(s: &str) -> &str {
    s.trim_matches(is_ws)
}

pub fn trim_ws_nl(s: &str) -> &str {
    s.trim_matches(is_ws_nl)
}

pub fn is_punct(c: char) -> bool {
    c.general_category_group() == GeneralCategoryGroup::Punctuation
}

pub fn is_symbol(c: char) -> bool {
    c.general_category_group() == GeneralCategoryGroup::Symbol
}

/// Foundation `.alphanumerics`: letters, marks and numbers.
pub fn is_alnum(c: char) -> bool {
    matches!(
        c.general_category_group(),
        GeneralCategoryGroup::Letter | GeneralCategoryGroup::Mark | GeneralCategoryGroup::Number
    )
}

/// Unicode `Ideographic` property (PropList.txt), with unassigned gaps inside the big blocks included.
pub fn is_ideographic(c: char) -> bool {
    matches!(c as u32,
        0x3006 | 0x3007 | 0x3021..=0x3029 | 0x3038..=0x303A | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xF900..=0xFA6D | 0xFA70..=0xFAD9 | 0x16FE4 | 0x17000..=0x18CD5 | 0x18CFF..=0x18D1E
        | 0x1B170..=0x1B2FB | 0x20000..=0x2FA1D | 0x30000..=0x323AF)
}

/// Swift `Character.isASCII && (isLetter || isNumber)`.
pub fn is_ascii_word(g: &str) -> bool {
    g.len() == 1 && g.as_bytes()[0].is_ascii_alphanumeric()
}

/// Swift `Character.isWhitespace` (first scalar has White_Space).
pub fn g_is_whitespace(g: &str) -> bool {
    g.chars().next().is_some_and(char::is_whitespace)
}

pub fn g_is_ascii(g: &str) -> bool {
    g.is_ascii()
}

/// Case- and width-insensitive grapheme comparison (`compare(_:options: [.caseInsensitive, .widthInsensitive])`).
pub fn g_eq_fold(a: &str, b: &str) -> bool {
    let fold = |s: &str| {
        s.chars()
            .map(narrow)
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    a == b || fold(a) == fold(b)
}

/// Fullwidth ASCII variants and the ideographic space to their ASCII forms.
pub fn narrow(c: char) -> char {
    match c as u32 {
        0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        0x3000 => ' ',
        _ => c,
    }
}

/// Case-insensitive literal replace-all (`replacingOccurrences(of:with:options: .caseInsensitive)`).
pub fn replace_ci(text: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return text.to_owned();
    }
    let lower: Vec<char> = from.chars().flat_map(char::to_lowercase).collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    'scan: while i < text.len() {
        // Try a match starting at byte i.
        let mut it = text[i..]
            .char_indices()
            .flat_map(|(o, c)| c.to_lowercase().map(move |l| (o + c.len_utf8(), l)));
        let mut end = 0;
        let mut ok = true;
        for &p in &lower {
            match it.next() {
                Some((e, l)) if l == p => end = e,
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        // The match must end on a char boundary of the source (no half of a multi-char lowercase expansion).
        if ok && it.next().is_none_or(|(e, _)| e != end) {
            out.push_str(to);
            i += end;
            continue 'scan;
        }
        let c = text[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

pub fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub fn from_utf16(u: &[u16]) -> String {
    String::from_utf16_lossy(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_case_insensitive() {
        assert_eq!(
            replace_ci("this is WRONG and wrong", "wrong", "right"),
            "this is right and right"
        );
        assert_eq!(replace_ci("ＡＢＣ", "abc", "x"), "ＡＢＣ");
        assert_eq!(replace_ci("Straße", "STRASSE", "x"), "Straße");
    }
}
