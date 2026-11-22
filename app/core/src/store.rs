//! records.jsonl, usage.json and saved audio, file-compatible with the Swift app (Records.swift).
//! Synchronous: files are small (≤ 1000 lines) and every caller is on one thread.
use crate::records::chars;
use chrono::{DateTime, Local, SubsecRound, Utc};
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
    /// Key-down → first partial in the HUD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_text_ms: Option<i64>,
    /// Release → ASR final.
    pub asr_ms: i64,
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
}

/// One local calendar day of dictation; totals survive records.jsonl dropping old lines.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub n: i64,
    pub ms: i64,
    pub chars: i64,
    /// Characters per app.
    pub apps: BTreeMap<String, i64>,
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
    }
}

pub const LIMIT: usize = 1000;
pub const AUDIO_KEEP: usize = 500;

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

pub struct Store {
    dir: PathBuf,
    pub all: Vec<Record>,
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
            usage: BTreeMap::new(),
        };
        match std::fs::read(s.usage_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            Some(u) => s.usage = u,
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
        let _ = std::fs::remove_file(self.records_path());
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

    /// `heard -> wanted` rules learned without the user typing them: a refine (or background learning) rewrite or a
    /// manual edit that turned some span into a known term, seen at least twice. Derived from records each time, so a
    /// rule disappears with its evidence. `terms`: the user's own terms (no learned rules, so a deleted term can't keep
    /// teaching itself); `dismissed`: "from -> to" lines the user ignored; a manual edit reverting the term vetoes it.
    pub fn auto_rules(
        &self,
        terms: &[String],
        dismissed: &HashSet<String>,
    ) -> Vec<(String, String, usize)> {
        let rules = crate::records::rules;
        let key = |(f, t): &(String, String)| format!("{f} -> {t}");
        let (mut count, mut vetoed) = (HashMap::<String, usize>::new(), HashSet::new());
        for r in self.delivered() {
            let mut found = vec![];
            if r.llm == "ok" {
                found.extend(rules(&r.raw, &r.text, terms));
            } else if let Some(c) = &r.checked {
                found.extend(rules(&r.raw, c, terms));
            }
            if let Some(e) = &r.edited {
                let pasted = r.pasted.as_deref().unwrap_or(&r.text);
                found.extend(rules(pasted, e, terms));
                vetoed.extend(rules(e, pasted, terms).iter().map(key));
            }
            for k in found.iter().map(key).collect::<HashSet<_>>() {
                *count.entry(k).or_default() += 1;
            }
        }
        let mut v: Vec<(String, String, usize)> = count
            .into_iter()
            .filter(|(k, n)| *n >= 2 && !vetoed.contains(k) && !dismissed.contains(k))
            .filter_map(|(k, n)| {
                k.split_once(" -> ")
                    .map(|(f, t)| (f.to_owned(), t.to_owned(), n))
            })
            .collect();
        v.sort_by(|a, b| (b.2, &b.0).cmp(&(a.2, &a.0)));
        v
    }

    /// Regenerates hotwords.auto.txt, which the hotword snapshot reads with the user's own file. Returns the rules that
    /// weren't in the file before, so the caller can announce them once.
    pub fn write_auto_rules(&self, terms: &[String], dismissed: &HashSet<String>) -> Vec<String> {
        let lines: Vec<String> = self
            .auto_rules(terms, dismissed)
            .into_iter()
            .map(|(f, t, _)| format!("{f} -> {t}"))
            .collect();
        let body = format!(
            "# Learned by Plaid from refine rewrites and manual edits; regenerated, do not edit. Remove a rule via 忽略.\n{}",
            lines.iter().map(|l| format!("{l}\n")).collect::<String>()
        );
        let path = self.dir.join(AUTO_RULES);
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        if old != body {
            let _ = write_atomic(&path, body.as_bytes());
        }
        let before: HashSet<&str> = old.split('\n').collect();
        lines
            .into_iter()
            .filter(|l| !before.contains(l.as_str()))
            .collect()
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
        let terms = vec!["Claude".to_string()];
        let none = HashSet::new();
        let at = Utc::now();
        let rec = |i: i64, checked: Option<&str>| Record {
            at: at + chrono::Duration::seconds(i),
            raw: "我用克劳德写代码".into(),
            text: "我用克劳德写代码".into(),
            llm: "off".into(),
            checked: checked.map(String::from),
            ..Default::default()
        };
        s.add(rec(0, Some("我用Claude写代码")));
        assert!(s.write_auto_rules(&terms, &none).is_empty(), "seen once");
        s.add(rec(1, None));
        s.set_checked(at + chrono::Duration::seconds(1), "我用Claude写代码".into());
        assert_eq!(
            s.write_auto_rules(&terms, &none),
            vec!["克劳德 -> Claude".to_string()]
        );
        assert!(
            s.write_auto_rules(&terms, &none).is_empty(),
            "announced once"
        );
        assert!(
            std::fs::read_to_string(dir.join(AUTO_RULES))
                .unwrap()
                .contains("克劳德 -> Claude\n")
        );
        let dismissed: HashSet<String> = ["克劳德 -> Claude".to_string()].into();
        assert!(s.auto_rules(&terms, &dismissed).is_empty());
        // A saved (not pasted) record doesn't count; an edit back to the heard text vetoes.
        s.add(Record {
            outcome: Some("saved".into()),
            ..rec(2, Some("我用Claude写代码"))
        });
        s.add(rec(3, None));
        s.set_edited(
            at + chrono::Duration::seconds(3),
            "我用Claude写代码".into(),
            "我用克劳德写代码".into(),
        );
        assert!(s.auto_rules(&terms, &none).is_empty());
        // Manual edits become fixes.
        s.add(rec(4, None));
        s.set_edited(
            at + chrono::Duration::seconds(4),
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
}
