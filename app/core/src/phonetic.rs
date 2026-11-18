//! Sound-alike hotword correction after recognition. Hotword lines `Target | alias | alias ~~~ block | block`:
//! any span that sounds close enough to the target or an alias becomes the target, unless a block word is within
//! 5 tokens. Chinese is compared as toneless pinyin with common confusions folded (zh/z, ang/an, l/n); Latin as
//! lowercase letters. A span must mix scripts exactly as the pattern does, so an English target never eats Chinese
//! syllables and 高 Code never matches 高考; 克劳德 -> Claude needs an explicit alias.
use crate::text;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub target: String,
    pub aliases: Vec<String>,
    pub block: Vec<String>,
}

/// key: folded toneless pinyin or lowercase letters; tone: pinyin with tone marks, unfolded (Latin: same as key).
struct Token {
    range: Range<usize>,
    key: String,
    tone: String,
    cjk: bool,
}

pub fn apply(text: &str, entries: &[Entry], threshold: f64) -> String {
    let toks = tokens(text);
    if toks.is_empty() {
        return text.to_owned();
    }
    // joined[i]: token i+1 follows token i with only whitespace between, so a span may cover both.
    let joined: Vec<bool> = toks
        .windows(2)
        .map(|w| text[w[0].range.end..w[1].range.start].graphemes_all(text::g_is_whitespace))
        .collect();
    struct Found<'a> {
        lo: usize,
        hi: usize,
        score: f64,
        target: &'a str,
    }
    let mut found: Vec<Found> = vec![];
    for e in entries {
        for p in std::iter::once(&e.target).chain(&e.aliases) {
            let pt = tokens(p);
            let key: String = pt.iter().map(|t| t.key.as_str()).collect();
            let key_n = key.chars().count();
            let all_cjk = pt.iter().all(|t| t.cjk);
            let any_cjk = pt.iter().any(|t| t.cjk);
            let scripts = (all_cjk, any_cjk);
            // One- or two-character Chinese words collide with everyday speech even at full similarity (李娜/你那,
            // 刘洋/留言, 林峰/两分 on the public sets), so they must match exactly, tones included (付平 -> 傅平).
            let short = !pt.is_empty() && all_cjk && pt.len() <= 2;
            let tone: String = pt.iter().map(|t| t.tone.as_str()).collect();
            let lens: Vec<usize> = if short {
                vec![pt.len()]
            } else {
                (pt.len().saturating_sub(1).max(1)..=pt.len() + 1).collect()
            };
            for lo in 0..toks.len() {
                for &n in &lens {
                    let hi = lo + n - 1;
                    if hi >= toks.len() || !(lo..hi).all(|i| joined[i]) {
                        continue;
                    }
                    let span_toks = &toks[lo..=hi];
                    if (
                        span_toks.iter().all(|t| t.cjk),
                        span_toks.iter().any(|t| t.cjk),
                    ) != scripts
                        || pt.is_empty()
                    {
                        continue;
                    }
                    let span = &text[toks[lo].range.start..toks[hi].range.end];
                    if span == e.target {
                        continue;
                    }
                    let k: String = span_toks.iter().map(|t| t.key.as_str()).collect();
                    let k_n = k.chars().count();
                    // Short keys (PR, CI, MLX) fuzzy-match too many ordinary words; only an explicit alias spelled
                    // the same way (pr for PR) may replace them.
                    if key_n < 4 && !(*p != e.target && k == key) {
                        continue;
                    }
                    if short
                        && span_toks
                            .iter()
                            .map(|t| t.tone.as_str())
                            .collect::<String>()
                            != tone
                    {
                        continue;
                    }
                    // An inflection, fragment or neighbour-extended copy of the pattern (prompts, Claude for
                    // Claude Code, a Plaid) is not a mishearing.
                    if k != key && (k.contains(&key) || key.contains(&k)) {
                        continue;
                    }
                    // Same text up to case: only worth changing when the target's casing says something (GitHub);
                    // an all-lowercase target would just lowercase a sentence-initial Agent.
                    if span.to_lowercase() == e.target.to_lowercase()
                        && !e.target.chars().any(char::is_uppercase)
                    {
                        continue;
                    }
                    let m = k_n.max(key_n);
                    if (k_n.abs_diff(key_n) as f64) > m as f64 * (1. - threshold) {
                        continue;
                    }
                    let a: Vec<char> = k.chars().collect();
                    let b: Vec<char> = key.chars().collect();
                    let score = 1. - lev(&a, &b) as f64 / m as f64;
                    if score < threshold {
                        continue;
                    }
                    let ctx = &text[toks[lo.saturating_sub(5)].range.start
                        ..toks[(hi + 5).min(toks.len() - 1)].range.end];
                    if e.block.iter().any(|b| ctx.contains(b.as_str())) {
                        continue;
                    }
                    found.push(Found {
                        lo,
                        hi,
                        score,
                        target: &e.target,
                    });
                }
            }
        }
    }
    // Best score first, longer span on ties; skip anything overlapping a span already taken.
    found.sort_by(|a, b| {
        (b.score, b.hi - b.lo)
            .partial_cmp(&(a.score, a.hi - a.lo))
            .unwrap()
    });
    let mut taken: Vec<&Found> = vec![];
    for f in &found {
        if !taken.iter().any(|t| f.lo <= t.hi && t.lo <= f.hi) {
            taken.push(f);
        }
    }
    taken.sort_by_key(|a| std::cmp::Reverse(a.lo));
    let mut out = text.to_owned();
    for t in taken {
        out.replace_range(toks[t.lo].range.start..toks[t.hi].range.end, t.target);
    }
    out
}

trait GraphemesAll {
    fn graphemes_all(&self, f: fn(&str) -> bool) -> bool;
}
impl GraphemesAll for str {
    fn graphemes_all(&self, f: fn(&str) -> bool) -> bool {
        unicode_segmentation::UnicodeSegmentation::graphemes(self, true).all(f)
    }
}

/// One token per Chinese character, one per run of ASCII letters/digits; everything else separates.
fn tokens(s: &str) -> Vec<Token> {
    let mut out = vec![];
    let gs: Vec<(usize, &str)> =
        unicode_segmentation::UnicodeSegmentation::grapheme_indices(s, true).collect();
    let mut i = 0;
    while i < gs.len() {
        let (start, g) = gs[i];
        if text::is_ascii_word(g) {
            let mut j = i + 1;
            while j < gs.len() && text::is_ascii_word(gs[j].1) {
                j += 1;
            }
            let end = gs.get(j).map_or(s.len(), |x| x.0);
            let k = s[start..end].to_lowercase();
            out.push(Token {
                range: start..end,
                tone: k.clone(),
                key: k,
                cjk: false,
            });
            i = j;
        } else {
            let mut cs = g.chars();
            if let (Some(c), None) = (cs.next(), cs.next())
                && let Some((tone, key)) = pinyin(c)
            {
                out.push(Token {
                    range: start..start + g.len(),
                    key: key.to_owned(),
                    tone: tone.to_owned(),
                    cjk: true,
                });
            }
            i += 1;
        }
    }
    out
}

/// (tone-marked pinyin, folded toneless key) per Han character, generated from macOS CFStringTransform by
/// scripts/pinyin_dump.swift. Polyphones take the default reading.
fn pinyin(c: char) -> Option<(&'static str, &'static str)> {
    static TABLE: OnceLock<HashMap<char, (&'static str, &'static str)>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            include_str!("../data/pinyin.tsv")
                .lines()
                .filter(|l| !l.starts_with('#'))
                .filter_map(|l| {
                    let mut f = l.split('\t');
                    let c = f.next()?.chars().next()?;
                    Some((c, (f.next()?, f.next()?)))
                })
                .collect()
        })
        .get(&c)
        .copied()
}

fn lev(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, y) in b.iter().enumerate() {
            cur.push(
                (prev[j + 1] + 1)
                    .min(cur[j] + 1)
                    .min(prev[j] + usize::from(x != y)),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}
