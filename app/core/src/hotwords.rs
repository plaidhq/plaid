//! Hotword rules for one utterance. One term per line (sent as ASR hotwords), `#` comments,
//! `heard -> wanted` lines rewrite the result, `Target | alias ~~~ block` lines drive sound-alike matching.
use crate::phonetic::{self, Entry};
use crate::text::{replace_ci, trim_ws};
use std::collections::HashSet;

/// Immutable rules for one utterance; file edits take effect on the next utterance.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub terms: Vec<String>,
    pub entries: Vec<Entry>,
    replacements: Vec<(String, String)>,
    threshold: f64,
    /// Words only sent as ASR hotwords (the wake word and command names): never rewritten into the text and
    /// not learning's wanted spellings.
    boost: Vec<String>,
}

fn target(line: &str) -> &str {
    trim_ws(
        line.split("~~~")
            .next()
            .unwrap_or("")
            .split('|')
            .next()
            .unwrap_or(""),
    )
}

fn words(s: &str) -> Vec<String> {
    s.split('|')
        .map(trim_ws)
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

impl Snapshot {
    pub fn new(lines: &[impl AsRef<str>], threshold: f64) -> Self {
        let lines: Vec<&str> = lines.iter().map(AsRef::as_ref).collect();
        let mut seen = HashSet::new();
        let terms = lines
            .iter()
            .map(|l| {
                if l.contains("->") {
                    trim_ws(l.rsplit("->").next().unwrap())
                } else {
                    target(l)
                }
                .to_owned()
            })
            .filter(|t| seen.insert(t.clone()))
            .collect();
        let replacements = lines
            .iter()
            .filter_map(|l| {
                let p: Vec<&str> = l.split("->").map(trim_ws).collect();
                (p.len() == 2 && !p[0].is_empty()).then(|| (p[0].to_owned(), p[1].to_owned()))
            })
            .collect();
        let entries = lines
            .iter()
            .filter(|l| !l.contains("->"))
            .filter_map(|l| {
                let mut p = l.split("~~~");
                let w = words(p.next().unwrap_or(""));
                let block = p.next().map(words).unwrap_or_default();
                let (target, aliases) = w.split_first()?;
                Some(Entry {
                    target: target.clone(),
                    aliases: aliases.to_vec(),
                    block,
                })
            })
            .collect();
        Snapshot {
            terms,
            entries,
            replacements,
            threshold,
            boost: vec![],
        }
    }

    /// Adds ASR-only hotwords; they go first in the request, ahead of the file's terms.
    pub fn boost(mut self, words: impl IntoIterator<Item = String>) -> Self {
        for w in words {
            let w = trim_ws(&w).to_owned();
            if !w.is_empty() && !self.boost.contains(&w) {
                self.boost.push(w);
            }
        }
        self
    }

    /// Literal `heard -> wanted` rewrites, then sound-alike matching against targets and aliases.
    pub fn apply(&self, text: &str) -> String {
        let literal = self
            .replacements
            .iter()
            .fold(text.to_owned(), |t, (a, b)| replace_ci(&t, a, b));
        phonetic::apply(&literal, &self.entries, self.threshold)
    }

    /// Value for the ASR request's `corpus.context`: hotwords first (the streaming pass keeps only the first
    /// ~100 tokens of this string), then `dialog_ctx` texts, newest first.
    pub fn context(&self, dialog: &[String]) -> Option<String> {
        let mut parts = vec![];
        let words: Vec<&str> = self
            .boost
            .iter()
            .map(String::as_str)
            .chain(
                self.entries
                    .iter()
                    .map(|e| e.target.as_str())
                    .filter(|t| !self.boost.iter().any(|b| b == t)),
            )
            .collect();
        if !words.is_empty() {
            let v: Vec<_> = words
                .iter()
                .map(|w| serde_json::json!({ "word": w }))
                .collect();
            parts.push(format!("\"hotwords\":{}", serde_json::Value::from(v)));
        }
        if !dialog.is_empty() {
            let v: Vec<_> = dialog
                .iter()
                .map(|t| serde_json::json!({ "text": t }))
                .collect();
            parts.push(format!(
                "\"context_type\":\"dialog_ctx\",\"context_data\":{}",
                serde_json::Value::from(v)
            ));
        }
        (!parts.is_empty()).then(|| format!("{{{}}}", parts.join(",")))
    }
}

/// Non-empty, non-comment lines of a hotwords file body.
pub fn lines(body: &str) -> Vec<String> {
    body.split('\n')
        .map(trim_ws)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boost_goes_first_and_is_never_rewritten_into_text() {
        let s = Snapshot::new(&["Claude Code", "指令"], 0.85).boost([
            "指令".to_owned(),
            "打开日历".to_owned(),
            " ".to_owned(),
        ]);
        assert_eq!(
            s.context(&[]).unwrap(),
            r#"{"hotwords":[{"word":"指令"},{"word":"打开日历"},{"word":"Claude Code"}]}"#
        );
        assert_eq!(s.apply("打开日立"), "打开日立");
        assert_eq!(s.terms, ["Claude Code", "指令"]);
    }
}
