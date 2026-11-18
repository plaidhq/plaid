//! Voice commands: an utterance that opens with an action's name (or alias), optionally after a wake word,
//! runs a shell line instead of being pasted.
use crate::text::{
    g_eq_fold, g_is_whitespace, is_ascii_word, is_punct, is_symbol, trim_ws, trim_ws_nl,
};
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    None,
    #[default]
    Hud,
    Paste,
}

/// One entry of actions.json. Hand-edited files may leave out everything but name and run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// One shell line (zsh -c on macOS).
    pub run: String,
    /// What happens to stdout: dropped, shown in the HUD (last line), or pasted at the caret.
    #[serde(default)]
    pub output: Output,
    /// Ask for ⏎ before running.
    #[serde(default)]
    pub confirm: bool,
}

impl Action {
    pub fn triggers(&self) -> Vec<&str> {
        std::iter::once(&self.name)
            .chain(&self.aliases)
            .map(|s| trim_ws(s))
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// Environment a command runs with (PLAID_TEXT, PLAID_ARGS, …).
pub type Env = Vec<(&'static str, String)>;

/// A run is killed (with its process group) after this.
pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Result of one run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// Exit status; None when killed by a signal or never started.
    pub code: Option<i32>,
    pub timed_out: bool,
    pub out: String,
    pub err: String,
    pub ms: u64,
}

impl Outcome {
    pub fn failed(err: impl Into<String>) -> Outcome {
        Outcome {
            err: err.into(),
            ..Default::default()
        }
    }

    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    /// One line for the HUD: last stderr line, else the exit status.
    pub fn failure(&self) -> String {
        if self.timed_out {
            return format!("超时（{} 秒）", TIMEOUT.as_secs());
        }
        match (self.err.lines().last().filter(|l| !l.is_empty()), self.code) {
            (Some(l), _) => l.to_owned(),
            (None, Some(c)) => format!("退出码 {c}"),
            (None, None) => "被信号终止".into(),
        }
    }

    pub fn last_line(&self) -> &str {
        self.out.lines().last().unwrap_or("")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed<'a> {
    Dictation,
    /// Wake word said, no command after it (text after the wake word).
    Unknown(String),
    Run(&'a Action, String),
}

/// Leading fillers and punctuation are skipped; wake word and names compare case- and width-insensitively with
/// spaces and punctuation ignored ("嘿，Plaid。" matches "嘿 Plaid"). The longest matching name wins.
/// An empty `wake` means bare mode: any utterance that opens with a command name is a command.
pub fn parse<'a>(text: &str, actions: &'a [Action], wake: &str) -> Parsed<'a> {
    let actions: Vec<&Action> = actions
        .iter()
        .filter(|a| !trim_ws_nl(&a.run).is_empty() && !a.triggers().is_empty())
        .collect();
    if actions.is_empty() {
        return Parsed::Dictation; // no commands defined: the wake word is just a word
    }
    let gs: Vec<&str> = text.graphemes(true).collect();
    let mut rest: &[&str] = &gs[gs.iter().take_while(|g| is_sep(g) || is_filler(g)).count()..];
    let wake = trim_ws(wake);
    if !wake.is_empty() {
        let Some(r) = strip(wake, rest) else {
            return Parsed::Dictation;
        };
        rest = r;
    }
    let mut best: Option<(&Action, &[&str], usize)> = None;
    for a in actions {
        for t in a.triggers() {
            let n = t.graphemes(true).count();
            if n > best.map_or(0, |b| b.2)
                && let Some(r) = strip(t, rest)
            {
                best = Some((a, r, n));
            }
        }
    }
    match best {
        Some((a, r, _)) => Parsed::Run(a, trim(r)),
        None if wake.is_empty() => Parsed::Dictation,
        None => Parsed::Unknown(trim(rest)),
    }
}

fn is_filler(g: &str) -> bool {
    g.chars().count() == 1 && "嗯呃额啊哦诶唉".contains(g)
}

/// Swift `Character.isWhitespace || isPunctuation || isSymbol` (decided by the first scalar).
fn is_sep(g: &str) -> bool {
    g_is_whitespace(g)
        || g.chars()
            .next()
            .is_some_and(|c| is_punct(c) || is_symbol(c))
}

/// The rest of `s` after `prefix`, or None. A Latin prefix must end at a word boundary ("Plaid" doesn't match "Plaids").
fn strip<'s, 'g>(prefix: &str, s: &'s [&'g str]) -> Option<&'s [&'g str]> {
    let mut i = 0;
    for p in prefix.graphemes(true).filter(|p| !is_sep(p)) {
        while i < s.len() && is_sep(s[i]) {
            i += 1;
        }
        if i >= s.len() || !g_eq_fold(s[i], p) {
            return None;
        }
        i += 1;
    }
    let last = prefix.graphemes(true).rev().find(|p| !is_sep(p));
    if last.is_some_and(is_ascii_word) && s.get(i).is_some_and(|g| is_ascii_word(g)) {
        return None;
    }
    Some(&s[i..])
}

/// Whitespace and sentence punctuation off both ends; other symbols stay ("-v", "#tag", "$5").
fn trim(s: &[&str]) -> String {
    let end = |g: &&str| {
        g_is_whitespace(g) || (g.chars().count() == 1 && "。.！!？?，,、；;：:".contains(*g))
    };
    let a = s.iter().take_while(|g| end(g)).count();
    let b = s.len() - s[a..].iter().rev().take_while(|g| end(g)).count();
    s[a..b].concat()
}
