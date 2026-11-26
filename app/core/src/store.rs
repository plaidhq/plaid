//! records.jsonl, usage.json and saved audio, file-compatible with the Swift app (Records.swift).
//! Synchronous: files are small (≤ 1000 lines) and every caller is on one thread.
use crate::records::chars;
use chrono::{DateTime, Local, SubsecRound, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Swift `JSONEncoder.dateEncodingStrategy = .iso8601`: UTC, whole seconds.
mod iso {
    use chrono::{DateTime, SecondsFormat, Utc};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(d: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&d.to_rfc3339_opts(SecondsFormat::Secs, true))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        let s = String::deserialize(d)?;
        DateTime::parse_from_rfc3339(&s)
            .map(|t| t.to_utc())
            .map_err(serde::de::Error::custom)
    }
}

/// One utterance. Latencies in ms; "release" is key-up, or the key that ends a toggled dictation.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    /// Key-down. Also the record's id.
    #[serde(with = "iso")]
    pub at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    pub audio_ms: i64,
    /// Audio sent to the ASR (billed); silence the gate held back is the difference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent_ms: Option<i64>,
    /// Key-down → first partial in the HUD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_text_ms: Option<i64>,
    /// Release → ASR final.
    pub asr_ms: i64,
    /// ASR final → refine end, BYOK only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_ms: Option<i64>,
    /// off | ok | timeout | skipped | error message
    pub llm: String,
    /// Release → paste.
    pub total_ms: i64,
    /// ASR final after the local rules (what the LLM sees).
    pub raw: String,
    pub text: String,
    /// ASR final before the local rules, when they changed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asr: Option<String>,
    /// File name under audio/.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    /// The pasted span after the user's manual correction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited: Option<String>,
    /// The span as the target app showed it right after the paste (baseline for `edited`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pasted: Option<String>,
    #[serde(rename = "noiseRMS", skip_serializing_if = "Option::is_none")]
    pub noise_rms: Option<i64>,
    #[serde(rename = "speechRMS", skip_serializing_if = "Option::is_none")]
    pub speech_rms: Option<i64>,
    /// ASR dialog context sent with this utterance, newest first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx: Option<Vec<String>>,
    /// "cloud": recognized and refined through Plaid Cloud. None: BYOK ("volc" / "local" on old records).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vp: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mic: Option<String>,
    /// None = pasted, "failed" = no result, "saved" = text available without automatic paste.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Physical press → capture start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_ms: Option<i64>,
    /// Capture start → mic running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mic_start_ms: Option<i64>,
}

impl Record {
    pub fn is_failure(&self) -> bool {
        self.outcome.as_deref() == Some("failed")
    }

    /// Through Plaid Cloud: no separate LLM time, the refine is inside release → paste.
    pub fn is_cloud(&self) -> bool {
        self.engine.as_deref() == Some("cloud")
    }
}

/// One local calendar day of dictation; totals survive records.jsonl dropping old lines.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub n: i64,
    pub ms: i64,
    pub chars: i64,
    /// Characters per app.
    pub apps: BTreeMap<String, i64>,
    /// (dictations, audio ms, characters) per local hour 0..24; empty for days recorded before it existed and
    /// whose records were gone when it was backfilled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hours: Vec<[i64; 3]>,
}

impl Usage {
    pub fn add(&mut self, r: &Record) {
        if r.is_failure() {
            return;
        }
        let c = chars(&r.text) as i64;
        self.n += 1;
        self.ms += r.audio_ms;
        self.chars += c;
        *self
            .apps
            .entry(r.app.clone().unwrap_or_else(|| "未知应用".into()))
            .or_default() += c;
        self.hours.resize(24, [0; 3]);
        let h = &mut self.hours[r.at.with_timezone(&Local).hour() as usize];
        h[0] += 1;
        h[1] += r.audio_ms;
        h[2] += c;
    }
}

pub const LIMIT: usize = 1000;
pub const AUDIO_KEEP: usize = 500;
/// Voice-command attempts kept in commands.jsonl.
pub const ATTEMPT_LIMIT: usize = 300;
pub const COMMANDS: &str = "commands.jsonl";

/// One voice-command attempt (commands.jsonl): an utterance that opened with the wake word or a command name.
/// Kept apart from records.jsonl so it never feeds ASR/LLM context, learning or usage; History shows it under 命令.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    /// Key-down of the utterance; the attempt's id.
    #[serde(with = "iso")]
    pub at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// The final recognized text.
    pub heard: String,
    /// The matched action; None when the wake word was heard without a known command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args: String,
    /// The wake word or name matched only by sound.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sound: bool,
    /// nomatch | confirm | running | ok | failed | cancelled
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
    /// Last lines of stderr (failure) or stdout (success), capped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
}

impl Attempt {
    /// Fills in a finished run: result, exit status, timing and a short tail of the relevant stream.
    pub fn finish(&mut self, o: &crate::actions::Outcome) {
        self.result = if o.ok() { "ok" } else { "failed" }.into();
        self.code = o.code;
        self.timed_out = o.timed_out;
        self.ms = Some(o.ms);
        let stream = if o.ok() { &o.out } else { &o.err };
        let t = tail(stream.trim_end(), 600);
        self.output = (!t.is_empty()).then(|| t.to_owned());
    }
}

/// The last `max` bytes of `s`, cut at a char boundary.
fn tail(s: &str, max: usize) -> &str {
    let mut i = s.len().saturating_sub(max);
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

/// A substitution the user made by hand in pasted text.
#[derive(Clone, Debug, PartialEq)]
pub struct Fix {
    pub from: String,
    pub to: String,
    pub count: usize,
    pub last: DateTime<Utc>,
    /// The edited text it was last seen in.
    pub context: String,
}

pub const AUTO_RULES: &str = "hotwords.auto.txt";

/// A learned `from -> to` candidate and its evidence, counted in records.
#[derive(Clone, Debug, PartialEq)]
pub struct AutoRule {
    pub from: String,
    pub to: String,
    /// Reviews that replaced `from` with `to`.
    pub hits: usize,
    /// Reviews that left `from` in place.
    pub kept: usize,
    /// Hits that were manual edits.
    pub edits: usize,
    /// Local days with a hit.
    pub days: usize,
    /// Applied to every result (hotwords.auto.txt) under the user's thresholds; otherwise only suggested.
    pub active: bool,
}

/// The user's learning settings: ignored lines and when a rule applies by itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Learn {
    /// "from -> to" lines the user ignored (`dismissedFixes`).
    pub dismissed: HashSet<String>,
    /// Manual edits that apply a rule (`learnEditHits`); 0 never.
    pub edits: usize,
    /// Refine or background-check rewrites, on at least two days, that apply a rule (`learnRewriteHits`); 0 never.
    pub rewrites: usize,
}

impl Default for Learn {
    fn default() -> Self {
        Learn {
            dismissed: HashSet::new(),
            edits: 2,
            rewrites: 3,
        }
    }
}

impl Learn {
    pub fn from_settings(mut get: impl FnMut(&str) -> Option<String>) -> Self {
        let d = Learn::default();
        let n =
            |v: Option<String>, def: usize| v.and_then(|v| v.trim().parse().ok()).unwrap_or(def);
        Learn {
            dismissed: get("dismissedFixes")
                .unwrap_or_default()
                .split('\n')
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect(),
            edits: n(get("learnEditHits"), d.edits),
            rewrites: n(get("learnRewriteHits"), d.rewrites),
        }
    }

    /// Replaced at least 3 times in 4, and by hand `edits` times or `rewrites` times over two days.
    fn applies(&self, a: &AutoRule) -> bool {
        a.hits * 4 >= (a.hits + a.kept) * 3
            && ((self.edits > 0 && a.edits >= self.edits)
                || (self.rewrites > 0 && a.hits >= self.rewrites && a.days >= 2))
    }
}

/// A record's reviews as (before, after, manual edit): the refine or background check of the raw text, and a small
/// manual edit of the pasted text.
fn review_pairs(r: &Record) -> Vec<(&str, &str, bool)> {
    let mut v = vec![];
    if r.llm == "ok" {
        v.push((r.raw.as_str(), r.text.as_str(), false));
    } else if let Some(c) = &r.checked {
        v.push((r.raw.as_str(), c.as_str(), false));
    }
    if let Some(e) = &r.edited {
        let pasted = r.pasted.as_deref().unwrap_or(&r.text);
        if small_edit(pasted, e) {
            v.push((pasted, e.as_str(), true));
        }
    }
    v
}

/// An edit that fixes a few words rather than rewriting the text: rewrites pair unrelated spans.
fn small_edit(pasted: &str, edited: &str) -> bool {
    let n = crate::text::count(edited);
    let changed = n as f64 * (1. - crate::diff::overlap(edited, pasted));
    changed <= 12f64.max(n as f64 * 0.3)
}

pub struct Store {
    dir: PathBuf,
    pub all: Vec<Record>,
    /// Voice-command attempts, oldest first.
    pub attempts: Vec<Attempt>,
    /// Keyed by local day (yyyy-MM-dd).
    pub usage: BTreeMap<String, Usage>,
}

pub fn day(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%Y-%m-%d").to_string()
}

/// Writes through a temp file and rename, creating the folder if needed.
pub fn write_atomic(path: &Path, body: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(tmp, path)
}

fn encoded(rs: &[Record]) -> Vec<u8> {
    let mut out = vec![];
    for r in rs {
        if let Ok(line) = serde_json::to_vec(r) {
            out.extend(line);
            out.push(b'\n');
        }
    }
    out
}

impl Store {
    pub fn load(dir: &Path) -> Store {
        let body = std::fs::read_to_string(dir.join("records.jsonl")).unwrap_or_default();
        let parsed: Vec<Record> = body
            .split('\n')
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let mut s = Store {
            dir: dir.to_owned(),
            all: parsed[parsed.len().saturating_sub(LIMIT)..].to_vec(),
            attempts: std::fs::read_to_string(dir.join(COMMANDS))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect(),
            usage: BTreeMap::new(),
        };
        match std::fs::read(s.usage_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            Some(u) => {
                s.usage = u;
                if s.backfill_hours(&parsed) {
                    s.save_usage();
                }
            }
            None => {
                // Backfill from the complete log before retention; existing totals are never rebuilt from the tail.
                for r in &parsed {
                    s.usage.entry(day(r.at)).or_default().add(r);
                }
                if !parsed.is_empty() {
                    s.save_usage();
                }
            }
        }
        if parsed.len() > LIMIT {
            s.persist();
        }
        s
    }

    /// Per-hour counts for days that predate them, from whatever records of that day are left. Day totals are
    /// left alone. Returns whether any day changed.
    fn backfill_hours(&mut self, parsed: &[Record]) -> bool {
        let mut rebuilt: BTreeMap<String, Usage> = BTreeMap::new();
        for r in parsed {
            let d = day(r.at);
            if self.usage.get(&d).is_some_and(|u| u.hours.is_empty()) {
                rebuilt.entry(d).or_default().add(r);
            }
        }
        let changed = rebuilt.values().any(|u| !u.hours.is_empty());
        for (d, u) in rebuilt {
            if let Some(t) = self.usage.get_mut(&d) {
                t.hours = u.hours;
            }
        }
        changed
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn records_path(&self) -> PathBuf {
        self.dir.join("records.jsonl")
    }

    fn usage_path(&self) -> PathBuf {
        self.dir.join("usage.json")
    }

    pub fn audio_dir(&self) -> PathBuf {
        self.dir.join("audio")
    }

    fn save_usage(&self) {
        if let Ok(b) = serde_json::to_vec(&self.usage) {
            let _ = write_atomic(&self.usage_path(), &b);
        }
    }

    fn persist(&self) {
        let _ = write_atomic(&self.records_path(), &encoded(&self.all));
    }

    pub fn add(&mut self, r: Record) {
        let trimmed = self.all.len() >= LIMIT;
        if !r.is_failure() {
            self.usage.entry(day(r.at)).or_default().add(&r);
            self.save_usage();
        }
        let line = encoded(std::slice::from_ref(&r));
        self.all.push(r);
        if trimmed {
            self.all.drain(..self.all.len() - LIMIT);
            self.persist();
        } else {
            let path = self.records_path();
            let appended = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(&line));
            if appended.is_err() {
                let _ = write_atomic(&path, &line);
            }
        }
    }

    /// Deletes every record and saved recording; usage totals are kept (History's 清空历史).
    pub fn clear(&mut self) {
        self.all.clear();
        self.attempts.clear();
        let _ = std::fs::remove_file(self.records_path());
        let _ = std::fs::remove_file(self.dir.join(COMMANDS));
        let _ = std::fs::remove_dir_all(self.audio_dir());
    }

    /// Deletes the records with these ids and their recordings; usage totals are kept. Returns how many went.
    pub fn remove(&mut self, ids: &[DateTime<Utc>]) -> usize {
        let before = self.all.len();
        let audio = self.audio_dir();
        self.all.retain(|r| {
            let gone = ids.contains(&r.at);
            if gone && let Some(a) = &r.audio {
                let _ = std::fs::remove_file(audio.join(a));
            }
            !gone
        });
        let n = before - self.all.len();
        if n > 0 {
            self.persist();
        }
        n
    }

    /// Adds a voice-command attempt, or replaces the one with the same id, and rewrites commands.jsonl.
    pub fn put_attempt(&mut self, a: Attempt) {
        match self.attempts.iter_mut().rev().find(|x| x.at == a.at) {
            Some(x) => *x = a,
            None => self.attempts.push(a),
        }
        let n = self.attempts.len();
        self.attempts.drain(..n.saturating_sub(ATTEMPT_LIMIT));
        let mut body = vec![];
        for a in &self.attempts {
            if let Ok(line) = serde_json::to_vec(a) {
                body.extend(line);
                body.push(b'\n');
            }
        }
        let _ = write_atomic(&self.dir.join(COMMANDS), &body);
    }

    /// Changes the attempt with this id, if it is still kept.
    pub fn update_attempt(&mut self, id: DateTime<Utc>, change: impl FnOnce(&mut Attempt)) {
        if let Some(mut a) = self.attempts.iter().rev().find(|a| a.at == id).cloned() {
            change(&mut a);
            self.put_attempt(a);
        }
    }

    /// Changes the newest record with this id (key-down time) and rewrites the log.
    pub fn update(&mut self, id: DateTime<Utc>, change: impl FnOnce(&mut Record)) {
        if let Some(r) = self.all.iter_mut().rev().find(|r| r.at == id) {
            change(r);
            self.persist();
        }
    }

    /// Delivery result for a record added as "saved"; only pasted text becomes recent context.
    pub fn set_delivery(
        &mut self,
        id: DateTime<Utc>,
        pasted: bool,
        error: Option<String>,
        total_ms: Option<i64>,
    ) {
        self.update(id, |r| {
            r.outcome = if pasted { None } else { Some("saved".into()) };
            r.error = error;
            if let Some(t) = total_ms {
                r.total_ms = t;
            }
        });
    }

    /// What the user changed inside the pasted span (EditWatch).
    pub fn set_edited(&mut self, id: DateTime<Utc>, pasted: String, edited: String) {
        self.update(id, |r| {
            r.pasted = Some(pasted);
            r.edited = Some(edited);
        });
    }

    /// Retry result saved in place (no paste, learning or refine); a failed record now counts in usage.
    /// A retried recognition: `asr` is the ASR final, `text` the same after the local rules.
    pub fn recovered(&mut self, id: DateTime<Utc>, asr: String, text: String) {
        let Some(was_failure) = self
            .all
            .iter()
            .rev()
            .find(|r| r.at == id)
            .map(Record::is_failure)
        else {
            return;
        };
        self.update(id, |r| {
            r.asr = (asr != text).then_some(asr);
            r.raw = text.clone();
            r.text = text;
            r.outcome = Some("saved".into());
            r.error = None;
        });
        if was_failure {
            if let Some(r) = self.all.iter().rev().find(|r| r.at == id) {
                self.usage.entry(day(r.at)).or_default().add(r);
            }
            self.save_usage();
        }
    }

    /// Background LLM rewrite of `raw`, kept for learning only.
    pub fn set_checked(&mut self, id: DateTime<Utc>, text: String) {
        self.update(id, |r| r.checked = Some(text));
    }

    /// Records that reached the user's field.
    fn delivered(&self) -> impl Iterator<Item = &Record> {
        self.all
            .iter()
            .filter(|r| !r.is_failure() && r.outcome.as_deref() != Some("saved"))
    }

    /// Substitutions learned from manual edits, most frequent first.
    pub fn fixes(&self) -> Vec<Fix> {
        let mut seen: HashMap<(String, String), Fix> = HashMap::new();
        for r in self.delivered() {
            let Some(e) = &r.edited else { continue };
            for (from, to) in crate::diff::replacements(r.pasted.as_deref().unwrap_or(&r.text), e) {
                let f = seen.entry((from.clone(), to.clone())).or_insert(Fix {
                    from,
                    to,
                    count: 0,
                    last: r.at,
                    context: String::new(),
                });
                f.count += 1;
                f.last = r.at;
                f.context = e.clone();
            }
        }
        let mut v: Vec<Fix> = seen.into_values().collect();
        v.sort_by_key(|f| std::cmp::Reverse((f.count, f.last)));
        v
    }

    /// `heard -> wanted` candidates learned without the user typing them, with their evidence. A candidate comes from
    /// a refine (or background learning) rewrite or a small manual edit that turned some span into a known term. It
    /// is then scored over every reviewed record (refined, checked or edited) whose text before review contains the
    /// heard span: a hit when the review removed it and added the term, otherwise kept. Derived from records each
    /// time, so a rule disappears with its evidence. `terms`: the user's own terms (no learned rules, so a deleted
    /// term can't keep teaching itself); `dismissed`: "from -> to" lines the user ignored; a manual edit reverting
    /// the term vetoes it. Records hold text after the active rules, so an active rule gathers no new hits or keeps.
    pub fn auto_rules(&self, terms: &[String], learn: &Learn) -> Vec<AutoRule> {
        let mut v: Vec<AutoRule> = self
            .auto_candidates(terms, learn)
            .into_iter()
            .filter(|a| a.active || (a.hits >= 2 && a.hits >= a.kept))
            .collect();
        v.sort_by(|a, b| (b.active, b.hits, &b.from).cmp(&(a.active, a.hits, &a.from)));
        v
    }

    /// Every scored candidate, before the shown filter (`plaid --learn-report`).
    pub fn auto_candidates(&self, terms: &[String], learn: &Learn) -> Vec<AutoRule> {
        let rules = crate::records::rules;
        let pairs = review_pairs;
        let (mut found, mut vetoed) = (HashSet::<(String, String)>::new(), HashSet::new());
        for r in self.delivered() {
            for (before, after, _) in pairs(r) {
                found.extend(rules(before, after, terms));
            }
            if let Some(e) = &r.edited {
                vetoed.extend(rules(e, r.pasted.as_deref().unwrap_or(&r.text), terms));
            }
        }
        let mut out: Vec<AutoRule> = found
            .into_iter()
            .filter(|(f, t)| {
                !vetoed.contains(&(f.clone(), t.clone()))
                    && !learn.dismissed.contains(&format!("{f} -> {t}"))
            })
            .map(|(from, to)| {
                let mut a = AutoRule {
                    from,
                    to,
                    hits: 0,
                    kept: 0,
                    edits: 0,
                    days: 0,
                    active: false,
                };
                let mut days = HashSet::new();
                for r in self.delivered() {
                    let (mut hit, mut seen) = (false, false);
                    for (before, after, edit) in pairs(r) {
                        let n = |s: &str, w: &str| s.matches(w).count();
                        if n(before, &a.from) == 0 {
                            continue;
                        }
                        seen = true;
                        if n(after, &a.from) < n(before, &a.from)
                            && n(after, &a.to) > n(before, &a.to)
                        {
                            hit = true;
                            a.edits += edit as usize;
                        }
                    }
                    if hit {
                        a.hits += 1;
                        days.insert(r.at.with_timezone(&Local).date_naive());
                    } else if seen {
                        a.kept += 1;
                    }
                }
                a.days = days.len();
                a.active = learn.applies(&a);
                a
            })
            .collect();
        out.sort_by(|a, b| (b.hits, &b.from).cmp(&(a.hits, &a.from)));
        out
    }

    /// Regenerates hotwords.auto.txt from the active rules; the hotword snapshot reads it with the user's own file.
    pub fn write_auto_rules(&self, terms: &[String], learn: &Learn) {
        let lines: String = self
            .auto_rules(terms, learn)
            .into_iter()
            .filter(|a| a.active)
            .map(|a| format!("{} -> {}\n", a.from, a.to))
            .collect();
        let body = format!(
            "# Learned by Plaid from refine rewrites and manual edits; regenerated, do not edit. Remove a rule via 忽略.\n{lines}"
        );
        let path = self.dir.join(AUTO_RULES);
        if std::fs::read_to_string(&path).unwrap_or_default() != body {
            let _ = write_atomic(&path, body.as_bytes());
        }
    }

    /// Pasted outputs from the last 10 minutes, oldest first.
    pub fn recent(&self, n: usize, now: DateTime<Utc>) -> Vec<String> {
        let v: Vec<_> = self
            .all
            .iter()
            .filter(|r| r.outcome.is_none() && (now - r.at).num_seconds() < 600)
            .collect();
        v[v.len().saturating_sub(n)..]
            .iter()
            .map(|r| r.text.clone())
            .collect()
    }

    /// Saves 16 kHz mono s16le as WAV named by local key-down time, keeping the newest 500. Returns the file name.
    pub fn save_audio(&self, pcm: &[u8], at: DateTime<Utc>) -> Option<String> {
        if pcm.is_empty() {
            return None;
        }
        let name = format!(
            "{}.wav",
            at.with_timezone(&Local)
                .trunc_subsecs(3)
                .format("%Y%m%d-%H%M%S%.3f")
        );
        let dir = self.audio_dir();
        write_atomic(&dir.join(&name), &crate::wav::encode(pcm)).ok()?;
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .map(|d| {
                d.filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .filter(|n| n.ends_with(".wav"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        for old in &files[..files.len().saturating_sub(AUDIO_KEEP)] {
            let _ = std::fs::remove_file(dir.join(old));
        }
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_swift_line_and_reencodes_compatibly() {
        let line = r#"{"text":"好","llm":"off","raw":"好","app":"飞书","asrMs":798,"firstTextMs":2328,"at":"2026-10-28T08:45:32Z","audioMs":9050,"totalMs":809,"noiseRMS":12,"outcome":"saved"}"#;
        let r: Record = serde_json::from_str(line).unwrap();
        assert_eq!(r.noise_rms, Some(12));
        assert_eq!(r.outcome.as_deref(), Some("saved"));
        let back = serde_json::to_string(&r).unwrap();
        assert!(back.contains(r#""at":"2026-10-28T08:45:32Z""#), "{back}");
        assert!(
            back.contains(r#""noiseRMS":12"#) && !back.contains("speechRMS"),
            "{back}"
        );
        assert_eq!(serde_json::from_str::<Record>(&back).unwrap(), r);
    }

    #[test]
    fn learns_rules_from_rewrites_and_edits() {
        let dir = std::env::temp_dir().join(format!("plaid-rules-{}", uuid::Uuid::new_v4()));
        let mut s = Store::load(&dir);
        let terms = vec!["Claude".to_string(), "冗余".to_string()];
        let none = Learn::default();
        let at = Utc::now() - chrono::Duration::days(3);
        // Record `i` on day `day`, raw text `raw`, background check `checked`.
        let rec = |day: i64, i: i64, raw: &str, checked: Option<&str>| Record {
            at: at + chrono::Duration::days(day) + chrono::Duration::seconds(i),
            raw: raw.into(),
            text: raw.into(),
            llm: "off".into(),
            checked: checked.map(String::from),
            ..Default::default()
        };
        let file = |s: &Store| {
            s.write_auto_rules(&terms, &none);
            std::fs::read_to_string(dir.join(AUTO_RULES)).unwrap()
        };
        let got = |s: &Store| -> Vec<(String, bool)> {
            s.auto_rules(&terms, &none)
                .into_iter()
                .map(|a| (format!("{} -> {}", a.from, a.to), a.active))
                .collect()
        };
        let rule = |active| vec![("克劳德 -> Claude".to_string(), active)];

        // Background checks: once is nothing, twice on one day is a suggestion, a third on another day applies.
        s.add(rec(0, 0, "我用克劳德写代码", Some("我用Claude写代码")));
        assert!(got(&s).is_empty(), "seen once");
        // A rewrite of the neighbours still counts as a hit when the heard span is gone and the term appeared.
        s.add(rec(
            0,
            1,
            "对标的克劳德模型上",
            Some("对标的是哪个 Claude 模型"),
        ));
        assert_eq!(got(&s), rule(false));
        assert!(!file(&s).contains("克劳德"), "suggestions aren't applied");
        s.add(rec(1, 2, "问问克劳德吧", Some("问问 Claude 吧")));
        assert_eq!(got(&s), rule(true));
        assert!(file(&s).contains("克劳德 -> Claude\n"));
        // Reviews that keep the heard span count against it: 3 of 4 still applies, 3 of 5 doesn't.
        s.add(rec(1, 3, "克劳德是个人名", Some("克劳德是个人名")));
        assert_eq!(got(&s), rule(true));
        s.add(rec(2, 4, "克劳德来了", None));
        s.set_checked(s.all.last().unwrap().at, "克劳德来了".into());
        assert_eq!(got(&s), rule(false));
        assert!(!file(&s).contains("克劳德"));
        // An unreviewed record (no refine, no check, no edit) is no evidence either way.
        s.add(rec(2, 5, "克劳德又来了", None));
        assert_eq!(got(&s), rule(false));
        // Ignored, or vetoed by an edit back to the heard text: gone.
        let dismissed = Learn {
            dismissed: ["克劳德 -> Claude".to_string()].into(),
            ..Learn::default()
        };
        assert!(s.auto_rules(&terms, &dismissed).is_empty());
        s.add(rec(2, 6, "我用Claude写代码", None));
        s.set_edited(
            s.all.last().unwrap().at,
            "我用Claude写代码".into(),
            "我用克劳德写代码".into(),
        );
        assert!(got(&s).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();

        // Manual edits: two small ones apply at once; a saved (not pasted) record doesn't count.
        let mut s = Store::load(&dir);
        for i in 0..2 {
            s.add(rec(0, i, "问一下克劳德吧", None));
            s.set_edited(
                s.all.last().unwrap().at,
                "问一下克劳德吧".into(),
                "问一下 Claude 吧".into(),
            );
        }
        assert_eq!(got(&s), rule(true));
        // The user's thresholds: 3 edits, or edits never, leave it a suggestion; 1 edit applies one alone.
        let with = |edits, rewrites| Learn {
            edits,
            rewrites,
            ..Learn::default()
        };
        assert!(!s.auto_rules(&terms, &with(3, 3))[0].active);
        assert!(!s.auto_rules(&terms, &with(0, 3))[0].active);
        let mut one = Store::load(&dir.join("one"));
        one.add(rec(0, 0, "问一下克劳德吧", None));
        one.set_edited(
            one.all.last().unwrap().at,
            "问一下克劳德吧".into(),
            "问一下 Claude 吧".into(),
        );
        assert!(
            one.auto_rules(&terms, &none).is_empty(),
            "one edit, default"
        );
        assert!(one.auto_rules(&terms, &with(1, 3))[0].active);
        // Settings parse, unset keeps the defaults.
        let l = Learn::from_settings(|k| match k {
            "learnEditHits" => Some("0".into()),
            "dismissedFixes" => Some("a -> b\n".into()),
            _ => None,
        });
        assert_eq!((l.edits, l.rewrites, l.dismissed.len()), (0, 3, 1));
        // Rewriting a whole passage pairs unrelated spans (荣誉 -> 冗余): no evidence.
        let mut s = Store::load(&dir.join("b"));
        let pasted = "关于资源的预留，目前比较悲观，整体可以供应的卡非常紧张，荣誉的部分先不谈。";
        let edited = "资源预留目前偏悲观：可供应的卡很紧张，多卡算法会更尴尬，冗余也要重新算。";
        for i in 0..3 {
            s.add(rec(i, 0, pasted, None));
            s.set_edited(s.all.last().unwrap().at, pasted.into(), edited.into());
        }
        assert!(got(&s).is_empty());
        // A particle the review dropped next to the term isn't part of the heard span.
        assert_eq!(
            crate::records::rules("还有，克劳德的点完设置", "还有，Claude 点完设置", &terms),
            vec![("克劳德".to_string(), "Claude".to_string())]
        );
        // Manual edits become fixes.
        let mut s = Store::load(&dir.join("c"));
        s.add(rec(0, 0, "我用克劳德写代码", None));
        s.set_edited(
            s.all.last().unwrap().at,
            "我用克劳德写代码".into(),
            "我用克劳德写程序".into(),
        );
        assert_eq!(
            s.fixes().first().map(|f| (f.from.as_str(), f.to.as_str())),
            Some(("代码", "程序"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn add_update_reload() {
        let dir = std::env::temp_dir().join(format!("plaid-store-{}", uuid::Uuid::new_v4()));
        let mut s = Store::load(&dir);
        let at = Utc::now();
        s.add(Record {
            at,
            app: Some("A".into()),
            text: "你好 world".into(),
            audio_ms: 1000,
            outcome: Some("saved".into()),
            ..Default::default()
        });
        s.add(Record {
            at: at + chrono::Duration::seconds(1),
            outcome: Some("failed".into()),
            ..Default::default()
        });
        s.set_delivery(at, true, None, Some(42));
        assert_eq!(s.recent(3, Utc::now()), vec!["你好 world".to_string()]);
        let s2 = Store::load(&dir);
        assert_eq!(s2.all.len(), 2);
        assert_eq!(s2.all[0].total_ms, 42);
        assert_eq!(s2.all[0].outcome, None);
        let u = &s2.usage[&day(at)];
        assert_eq!((u.n, u.chars, u.apps["A"]), (1, 3, 3));
        let name = s.save_audio(&[0u8; 3200], at).unwrap();
        assert!(dir.join("audio").join(&name).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_usage_keeps_totals_and_gains_hours() {
        let dir = std::env::temp_dir().join(format!("plaid-hours-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let at: DateTime<Utc> = "2026-10-28T08:45:32Z".parse().unwrap();
        let d = day(at);
        let line = r#"{"text":"你好","llm":"off","raw":"你好","at":"2026-10-28T08:45:32Z","audioMs":900,"asrMs":1,"totalMs":1}"#;
        std::fs::write(dir.join("records.jsonl"), format!("{line}\n")).unwrap();
        // Totals larger than the one kept record: they must survive, only the hours come from the record.
        let old = format!(
            r#"{{"{d}":{{"n":9,"ms":9000,"chars":90,"apps":{{"A":90}}}},"2026-01-01":{{"n":1,"ms":1,"chars":1,"apps":{{}}}}}}"#
        );
        std::fs::write(dir.join("usage.json"), &old).unwrap();
        let s = Store::load(&dir);
        let u = &s.usage[&d];
        assert_eq!((u.n, u.ms, u.chars), (9, 9000, 90));
        let h = at.with_timezone(&Local).hour() as usize;
        assert_eq!((u.hours.len(), u.hours[h]), (24, [1, 900, 2]));
        assert!(s.usage["2026-01-01"].hours.is_empty());
        let saved = std::fs::read_to_string(dir.join("usage.json")).unwrap();
        assert!(
            saved.contains("hours")
                && !saved.contains(r#""2026-01-01":{"n":1,"ms":1,"chars":1,"apps":{},"hours""#)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
