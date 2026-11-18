//! Text alignment between what Plaid pasted and what ended up in the field. Offsets in `inserted_span` and
//! `edited_span` are UTF-16 code units (what accessibility APIs report); everything else counts graphemes.
use crate::text::{self, from_utf16, is_punct, is_symbol, is_ws_nl, trim_ws, trim_ws_nl, utf16};
use unicode_segmentation::UnicodeSegmentation;

/// Region of `after` that differs from `before`: what the paste inserted.
pub fn inserted_span(before: &str, after: &str) -> Option<(usize, usize, String)> {
    let (a, b) = (utf16(before), utf16(after));
    let p = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let s = a[p..]
        .iter()
        .rev()
        .zip(b[p..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let end = b.len() - s;
    (end > p).then(|| (p, end, from_utf16(&b[p..end])))
}

/// The span's current text, if the text around it is unchanged (typing after the span counts as part of it;
/// alignment later ignores trailing insertions).
pub fn edited_span(base: &str, start: usize, end: usize, now: &str) -> Option<String> {
    let (a, b) = (utf16(base), utf16(now));
    let (pre, suf) = (a.get(..start)?, a.get(end..)?);
    if b.len() < pre.len() + suf.len() || !b.starts_with(pre) || !b.ends_with(suf) {
        return None;
    }
    let s = from_utf16(&b[start..b.len() - suf.len()]);
    (!trim_ws_nl(&s).is_empty()).then_some(s)
}

/// EditWatch, shortly after the paste: the span it inserted, when that is (nearly) the pasted text.
pub fn pasted_span(before: &str, after: &str, pasted: &str) -> Option<(usize, usize, String)> {
    let s = inserted_span(before, after)?;
    (overlap(pasted, &s.2) > 0.8 && overlap(&s.2, pasted) > 0.8).then_some(s)
}

/// EditWatch, later: the user's version of the pasted span, when they changed it but kept at least half of it.
pub fn user_edit(base: &str, start: usize, end: usize, now: &str, pasted: &str) -> Option<String> {
    if now == base {
        return None;
    }
    let e = edited_span(base, start, end, now)?;
    (e != pasted && overlap(pasted, &e) >= 0.5).then_some(e)
}

/// Share of `a`'s characters that survive in `b` (LCS / |a|).
pub fn overlap(a: &str, b: &str) -> f64 {
    let (x, y) = (text::graphemes(a), text::graphemes(b));
    if x.is_empty() || y.is_empty() || x.len() * y.len() > 4_000_000 {
        return 0.;
    }
    lcs(&x, &y).len() as f64 / x.len() as f64
}

#[derive(Clone, Copy)]
struct G {
    a0: usize,
    a1: usize,
    b0: usize,
    b1: usize,
}

fn merged(gs: Vec<G>) -> Vec<G> {
    let mut out: Vec<G> = vec![];
    for g in gs {
        match out.last_mut() {
            Some(last) if g.a0 <= last.a1 => {
                last.a1 = last.a1.max(g.a1);
                last.b1 = last.b1.max(g.b1);
            }
            _ => out.push(g),
        }
    }
    out
}

/// Text with punctuation, symbols and whitespace removed.
fn core(s: &str) -> String {
    s.chars()
        .filter(|&c| !(is_punct(c) || is_symbol(c) || is_ws_nl(c)))
        .collect()
}

/// Substitutions from `a` to `b`, widened to whole ASCII words and to at least 2 characters per side,
/// so a rule never rewrites a lone CJK character. Insertions at the edges (the user kept typing) are dropped.
pub fn replacements(a: &str, b: &str) -> Vec<(String, String)> {
    let (x, y) = (text::graphemes(a), text::graphemes(b));
    if x.len() * y.len() > 4_000_000 {
        return vec![];
    }
    let m = lcs(&x, &y);
    // Gaps between consecutive matches are the changed groups.
    let mut groups = vec![];
    let (mut pi, mut pj) = (0, 0);
    for (i, j) in m.into_iter().chain([(x.len(), y.len())]) {
        if i > pi || j > pj {
            groups.push(G {
                a0: pi,
                a1: i,
                b0: pj,
                b1: j,
            });
        }
        (pi, pj) = (i + 1, j + 1);
    }
    let word = |g: &str| text::is_ascii_word(g);
    // 1. Widen to whole ASCII words, so "keep a life" → "Keeplife" stays one pair.
    let groups = merged(
        groups
            .into_iter()
            .map(|mut g| {
                while g.a0 > 0 && g.b0 > 0 && x[g.a0 - 1] == y[g.b0 - 1] && word(x[g.a0 - 1]) {
                    g.a0 -= 1;
                    g.b0 -= 1;
                }
                while g.a1 < x.len() && g.b1 < y.len() && x[g.a1] == y[g.b1] && word(x[g.a1]) {
                    g.a1 += 1;
                    g.b1 += 1;
                }
                g
            })
            .collect(),
    );
    // 2. Only substitutions; pure insertions (kept typing) and deletions (dropped fillers) aren't ASR fixes.
    // Punctuation-only sides are formatting, not recognition errors.
    let groups: Vec<G> = groups
        .into_iter()
        .filter(|g| {
            !(g.a0 == g.a1
                || g.b0 == g.b1
                || core(&x[g.a0..g.a1].concat()).is_empty()
                || core(&y[g.b0..g.b1].concat()).is_empty())
        })
        .collect();
    // 3. Widen to whole words of the corrected text ("工成" → "工程师").
    let words = word_spans(b);
    let groups = merged(
        groups
            .into_iter()
            .map(|mut g| {
                let hit: Vec<&(usize, usize)> =
                    words.iter().filter(|w| w.0 < g.b1 && w.1 > g.b0).collect();
                let lo = hit.iter().map(|w| w.0).min().unwrap_or(g.b0);
                let hi = hit.iter().map(|w| w.1).max().unwrap_or(g.b1);
                let l = (g.b0 as isize - lo as isize).min(g.a0 as isize).max(0) as usize;
                let r = (hi as isize - g.b1 as isize)
                    .min((x.len() - g.a1) as isize)
                    .max(0) as usize;
                g.a0 -= l;
                g.b0 -= l;
                g.a1 += r;
                g.b1 += r;
                g
            })
            .collect(),
    );
    // 4. At least 2 characters per side, alternating left/right context.
    let out = merged(
        groups
            .into_iter()
            .map(|mut g| {
                let mut left = true;
                while g.a1 - g.a0 < 2 || g.b1 - g.b0 < 2 {
                    if left && g.a0 > 0 && g.b0 > 0 {
                        g.a0 -= 1;
                        g.b0 -= 1;
                    } else if g.a1 < x.len() && g.b1 < y.len() {
                        g.a1 += 1;
                        g.b1 += 1;
                    } else if g.a0 > 0 && g.b0 > 0 {
                        g.a0 -= 1;
                        g.b0 -= 1;
                    } else {
                        break;
                    }
                    left = !left;
                }
                g
            })
            .collect(),
    );
    out.into_iter()
        .filter_map(|g| {
            let f = trim_ws(&x[g.a0..g.a1].concat()).to_owned();
            let t = trim_ws(&y[g.b0..g.b1].concat()).to_owned();
            let ok = !f.is_empty()
                && !t.is_empty()
                && text::count(&f) <= 12
                && text::count(&t) <= 12
                && core(&f) != core(&t)
                && !core(&t).is_empty();
            ok.then_some((f, t))
        })
        .collect()
}

/// Word-like segments of `s` as grapheme ranges (dictionary segmentation for CJK, UAX #29 otherwise).
/// Stands in for NLTokenizer(unit: .word), which skips punctuation and whitespace.
fn word_spans(s: &str) -> Vec<(usize, usize)> {
    use icu_segmenter::{WordSegmenter, options::WordBreakInvariantOptions};
    // Byte offset → grapheme index.
    let mut gi = vec![0usize; s.len() + 1];
    let mut n = 0;
    for (o, _) in s.grapheme_indices(true) {
        gi[o] = n;
        n += 1;
    }
    gi[s.len()] = n;
    let seg = WordSegmenter::new_dictionary(WordBreakInvariantOptions::default());
    let mut out = vec![];
    let mut prev = 0;
    for (b, ty) in seg.segment_str(s).iter_with_word_type() {
        if b > prev && ty.is_word_like() && s.is_char_boundary(prev) {
            out.push((gi[prev], gi[b]));
        }
        prev = b;
    }
    out
}

/// Matched index pairs of a longest common subsequence.
fn lcs(x: &[&str], y: &[&str]) -> Vec<(usize, usize)> {
    let (n, m) = (x.len(), y.len());
    let w = m + 1;
    let mut t = vec![0i32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * w + j] = if x[i] == y[j] {
                t[(i + 1) * w + j + 1] + 1
            } else {
                t[(i + 1) * w + j].max(t[i * w + j + 1])
            };
        }
    }
    let (mut out, mut i, mut j) = (vec![], 0, 0);
    while i < n && j < m {
        if x[i] == y[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}
