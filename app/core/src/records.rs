//! Record-derived numbers and learned rules.
use crate::text::{self, is_alnum, is_ideographic, is_newline, is_punct, is_symbol, trim_ws};
use unicode_segmentation::UnicodeSegmentation;

/// Characters as a reader counts them in mixed text: one per CJK ideograph, one per run of letters/digits.
pub fn chars(s: &str) -> usize {
    let (mut n, mut in_word) = (0, false);
    for c in s.chars() {
        if is_ideographic(c) {
            n += 1;
            in_word = false;
        } else if is_alnum(c) {
            n += !in_word as usize;
            in_word = true;
        } else {
            in_word = false;
        }
    }
    n
}

/// For each hotword term that occurs more often in `after` than in `before`, the span of `before` that sits between
/// the same neighbouring characters: `heard -> term`. The heard side must be 2-12 characters, free of punctuation,
/// not already the term in another case, and contain CJK. All-ASCII heards are skipped because rewrites are literal
/// substring replacements ("Cloud -> Claude" would hit "iCloud").
pub fn rules(before: &str, after: &str, terms: &[String]) -> Vec<(String, String)> {
    let mut out = vec![];
    for t in terms.iter().filter(|t| {
        !t.is_empty() && after.matches(t.as_str()).count() > before.matches(t.as_str()).count()
    }) {
        for (at, _) in after.match_indices(t.as_str()) {
            let left = suffix(trim_ws(&after[..at]), 2);
            let right = prefix(trim_ws(&after[at + t.len()..]), 2);
            let starts: Vec<usize> = if left.is_empty() {
                vec![0]
            } else {
                before
                    .match_indices(left)
                    .map(|(i, _)| i + left.len())
                    .collect()
            };
            let heard = starts
                .into_iter()
                .filter_map(|s| {
                    let end = if right.is_empty() {
                        Some(before.len())
                    } else {
                        before[s..].find(right).map(|e| s + e)
                    };
                    end.map(|e| trim_ws(&before[s..e]))
                })
                .min_by_key(|h| text::count(h));
            let Some(heard) = heard else { continue };
            let ok = (2..=12).contains(&text::count(heard))
                && !heard
                    .chars()
                    .any(|c| is_punct(c) || is_symbol(c) || is_newline(c))
                && !heard.to_lowercase().contains(&t.to_lowercase())
                && heard.graphemes(true).any(|g| !g.is_ascii());
            if ok {
                out.push((heard.to_owned(), t.clone()));
            }
        }
    }
    out
}

fn suffix(s: &str, n: usize) -> &str {
    let start = s
        .grapheme_indices(true)
        .rev()
        .nth(n - 1)
        .map_or(0, |(i, _)| i);
    &s[start..]
}

fn prefix(s: &str, n: usize) -> &str {
    let end = s.grapheme_indices(true).nth(n).map_or(s.len(), |(i, _)| i);
    &s[..end]
}
