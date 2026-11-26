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
    /// The action, its arguments, and whether the wake word or name only sounded like it (see `parse`).
    Run(&'a Action, String, bool),
}

/// Leading fillers and punctuation are skipped; wake word and names compare case- and width-insensitively with
/// spaces and punctuation ignored ("嘿，Plaid。" matches "嘿 Plaid"). The longest matching name wins.
/// An empty `wake` means bare mode: any utterance that opens with a command name is a command.
///
/// Chinese also matches by sound (same toneless pinyin with zh/z, l/n, ing/in folded), since the ASR often
/// writes a homophone (智灵 for 指令). Only one side may be a sound match: a sound-alike wake word needs an exact
/// name after it (alone it stays dictation), and a sound-alike name needs the exact wake word. Bare mode is exact,
/// and a sound-alike name that fits two actions equally runs neither.
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
    let mut wake_by_sound = false;
    if !wake.is_empty() {
        if let Some(r) = strip(wake, rest, false) {
            rest = r;
        } else if let Some(r) = strip(wake, rest, true) {
            rest = r;
            wake_by_sound = true;
        } else {
            return Parsed::Dictation;
        }
    }
    let best = |sound: bool| {
        // (action, rest, trigger length, tied with another action at that length)
        let mut best: Option<(&Action, &[&str], usize, bool)> = None;
        for a in &actions {
            for t in a.triggers() {
                let n = t.graphemes(true).count();
                let Some(r) = strip(t, rest, sound) else {
                    continue;
                };
                match best {
                    Some(b) if n < b.2 => {}
                    Some(b) if n == b.2 => {
                        best = Some((b.0, b.1, n, b.3 || !std::ptr::eq(b.0, *a)))
                    }
                    _ => best = Some((a, r, n, false)),
                }
            }
        }
        best
    };
    if let Some((a, r, ..)) = best(false) {
        return Parsed::Run(a, trim(r), wake_by_sound);
    }
    if !wake.is_empty()
        && !wake_by_sound
        && let Some((a, r, _, false)) = best(true)
    {
        return Parsed::Run(a, trim(r), true);
    }
    match wake.is_empty() || wake_by_sound {
        true => Parsed::Dictation,
        false => Parsed::Unknown(trim(rest)),
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
/// With `sound`, a Chinese character also matches one that reads the same (`phonetic::alike`); a prefix with no
/// Chinese in it can't match by sound.
fn strip<'s, 'g>(prefix: &str, s: &'s [&'g str], sound: bool) -> Option<&'s [&'g str]> {
    if sound && !prefix.graphemes(true).any(|p| crate::phonetic::alike(p, p)) {
        return None;
    }
    let mut i = 0;
    for p in prefix.graphemes(true).filter(|p| !is_sep(p)) {
        while i < s.len() && is_sep(s[i]) {
            i += 1;
        }
        if i >= s.len() || !(g_eq_fold(s[i], p) || sound && crate::phonetic::alike(s[i], p)) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn act(name: &str) -> Action {
        Action {
            name: name.into(),
            run: "true".into(),
            ..Default::default()
        }
    }

    fn run(text: &str, list: &[Action], wake: &str) -> Option<(String, String, bool)> {
        match parse(text, list, wake) {
            Parsed::Run(a, args, sound) => Some((a.name.clone(), args, sound)),
            _ => None,
        }
    }

    #[test]
    fn sound_alike_name_after_exact_wake() {
        let list = [act("打开日历"), act("天气")];
        assert_eq!(
            run("指令，打开日立。", &list, "指令"),
            Some(("打开日历".into(), "".into(), true))
        );
        assert_eq!(
            run("指令添起北京", &list, "指令"),
            Some(("天气".into(), "北京".into(), true))
        );
        // Exact still wins and isn't flagged.
        assert_eq!(
            run("指令天气", &list, "指令"),
            Some(("天气".into(), "".into(), false))
        );
    }

    #[test]
    fn sound_alike_wake_needs_exact_name() {
        let list = [act("天气")];
        assert_eq!(
            run("智灵，天气。", &list, "指令"),
            Some(("天气".into(), "".into(), true))
        );
        // Both halves only sound alike, or the wake word alone: dictation, not 没有匹配的命令.
        assert_eq!(parse("智灵添起", &list, "指令"), Parsed::Dictation);
        assert_eq!(parse("智灵是个好名字", &list, "指令"), Parsed::Dictation);
        assert_eq!(
            parse("指令是个好名字", &list, "指令"),
            Parsed::Unknown("是个好名字".into())
        );
    }

    #[test]
    fn sound_matching_limits() {
        // Ordinary openings that share a syllable don't match.
        let list = [act("天气")];
        for s in ["只能这样了", "至今没有消息", "指南针在哪", "治理天气"] {
            assert_eq!(parse(s, &list, "指令"), Parsed::Dictation, "{s}");
        }
        // Bare mode is exact.
        assert_eq!(parse("添起不错", &list, ""), Parsed::Dictation);
        assert!(run("天气不错", &list, "").is_some());
        // A Latin wake word never matches by sound.
        assert_eq!(parse("Preload 天气", &list, "Plaid"), Parsed::Dictation);
        // A sound-alike name that fits two actions runs neither.
        let two = [act("天气"), act("添起")];
        assert_eq!(
            run("指令天气", &two, "指令").map(|r| r.0),
            Some("天气".into())
        );
        let two = [act("天器"), act("添起")];
        assert_eq!(
            parse("指令天气", &two, "指令"),
            Parsed::Unknown("天气".into())
        );
    }
}
