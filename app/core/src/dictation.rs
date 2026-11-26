//! The utterance state machine (port of the Phase logic in main.swift). Pure: every input arrives as a `Msg`
//! on one thread, every side effect goes through `Platform`. Inputs from other threads (mic, ASR, timers,
//! focus reads) carry the utterance generation they belong to; stale ones are dropped.
use crate::actions::{self, Action, Parsed};
use crate::cloud;
use crate::gate::Gate;
use crate::llm::{self, Endpoint, Level};
use crate::refine::{self, Plan, Skip};
use crate::rules::Rules;
use crate::session::{Event, Session};
use crate::store::{Attempt, Record, Store};
use crate::{gesture, text};
use chrono::{DateTime, Utc};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// p90 of the 100 ms buffer RMS below this means the input barely carries speech (Mic.quietRMS).
pub const QUIET_RMS: i64 = 150;
pub const SILENCE_DEFAULT: f64 = 500.;
const CONTEXT_WAIT: Duration = Duration::from_millis(250);
/// Learning must not build an unbounded backlog during rapid dictation or with a slow endpoint.
const LEARN_QUEUE: usize = 8;
const LEARN_TIMEOUT: Duration = Duration::from_secs(20);
/// A command waiting for ⏎ is dropped after this.
const CONFIRM_WAIT: Duration = Duration::from_secs(8);
/// Default for `llmMinChars`: shorter results paste as recognized; the LLM rarely changes them, and it adds 0.7-3.6 s.
pub const REFINE_MIN_CHARS: usize = 20;
/// Past the plan's timeout, how long the device waits for Cloud's refine before pasting the recognized text.
const CLOUD_GRACE: Duration = Duration::from_millis(1500);
/// UTF-16 units of the field text before the caret sent as ASR context (commands get all of `Msg::Field`'s).
const ASR_BEFORE: usize = 300;

#[derive(Debug, Clone, PartialEq)]
pub enum Hud {
    Listen,
    /// Buffer RMS (Int16 scale).
    Level(f64),
    Quiet(String),
    Unquiet,
    /// Live transcript; `command` marks text that parses as a voice command.
    Text {
        text: String,
        command: bool,
    },
    /// Tap-started: keeps going after key-up.
    Continuous,
    Finishing,
    /// LLM pass in flight on this raw text; esc pastes it as is.
    Refining(String),
    Done(String),
    Saved(String),
    /// Information or a warning.
    Flash(String),
    /// Recognition, microphone or command failure.
    Failed(String),
    /// A voice command waiting for ⏎ / esc.
    Ask(String),
    /// A voice command in flight (no auto-hide).
    Running(String),
    /// A command's output line.
    Note(String),
    Hide,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Timer {
    /// Begin the ASR request without the focused-field text.
    Context,
    /// Tail cap after release.
    Tail,
    /// Low-input warning after a paste.
    LowInput,
    /// Drop a command still waiting for ⏎.
    Confirm,
    /// Give up on the Cloud refine and paste the recognized text.
    Refine,
}

#[derive(Debug)]
pub enum Msg {
    /// `trigger_ms`: physical press → now.
    Key {
        action: gesture::Action,
        trigger_ms: Option<i64>,
    },
    /// Unmodified ⏎ / esc the key tap already swallowed for us.
    Return,
    Escape,
    /// One mic buffer of 16 kHz mono s16le (100 ms).
    Audio {
        generation: u64,
        pcm: Vec<u8>,
    },
    /// The capture stopped and its last buffer is already queued ahead of this.
    CaptureEnded {
        generation: u64,
    },
    /// Text before the caret in the focused field (up to 2000 UTF-16 units) and the focused window's title, when available.
    Field {
        generation: u64,
        before: Option<String>,
        window: Option<String>,
    },
    Session {
        generation: u64,
        event: Event,
    },
    Timer {
        generation: u64,
        timer: Timer,
    },
    /// Paste target still focused (`ok`), checked off the main thread.
    Verified {
        generation: u64,
        id: DateTime<Utc>,
        text: String,
        ok: bool,
    },
    /// Reply to `Platform::llm(id, …)`.
    Llm {
        id: u64,
        result: Result<String, llm::Failure>,
    },
    /// The user changed pasted text by hand (record `id`).
    Edited {
        id: DateTime<Utc>,
        pasted: String,
        edited: String,
    },
    /// A voice command started by `Platform::run` in `generation` exited.
    Ran {
        generation: u64,
        action: Action,
        outcome: actions::Outcome,
    },
    /// Recognize the saved audio of record `id` again (History's retry). Never pastes.
    Recover {
        id: DateTime<Utc>,
    },
    /// Reply to `Platform::recover(id, …)`.
    Recovered {
        id: DateTime<Utc>,
        result: Result<String, String>,
    },
    /// Delete all records and recordings (History's 清空历史); refused while dictating or retrying.
    Clear,
    /// Delete these records and their recordings (History's 删除); refused while one of them is being retried.
    Delete {
        ids: Vec<DateTime<Utc>>,
    },
    /// Run the LLM again on record `id`'s recognized text (History's 重新润色) and keep the result in the record.
    /// Never pastes; one at a time.
    Refine {
        id: DateTime<Utc>,
    },
}

pub trait Platform {
    fn hud(&mut self, h: Hud);
    fn log(&mut self, line: &str);
    /// Post `msg` to the controller after `after`.
    fn timer(&mut self, after: Duration, msg: Msg);
    fn setting(&mut self, key: &str) -> Option<String>;
    /// Opens an ASR session (Volc, or Plaid Cloud when `cloud`) whose events come back as
    /// `Msg::Session { generation }`.
    fn open_session(&mut self, generation: u64, cloud: bool) -> Result<Session, String>;
    /// Name of the frontmost app; also remembers it as the paste target for this generation.
    fn front_app(&mut self, generation: u64) -> (Option<String>, Option<String>);
    /// Reads the focused field off-thread, then posts `Msg::Field`.
    fn read_field(&mut self, generation: u64);
    /// Starts the mic; buffers come back as `Msg::Audio`. Returns the device name.
    fn start_capture(&mut self, generation: u64) -> Result<String, String>;
    /// Stops the mic, then posts `Msg::CaptureEnded` behind the last buffer.
    fn stop_capture(&mut self, generation: u64);
    /// Checks the generation's paste target off-thread, then posts `Msg::Verified`.
    fn verify_target(&mut self, generation: u64, id: DateTime<Utc>, text: String);
    /// Pastes into the frontmost app if it is still the target; returns whether it posted.
    fn paste(&mut self, generation: u64, text: &str) -> bool;
    fn actions(&mut self) -> (Vec<Action>, String);
    /// Hotword terms (wanted spellings); `auto` includes learned rules.
    fn terms(&mut self, auto: bool) -> Vec<String>;
    /// Stored key for an LLM endpoint.
    fn llm_key(&mut self, base: &str) -> Option<String>;
    /// Sends the request off-thread, then posts `Msg::Llm { id }`.
    fn llm(&mut self, id: u64, req: llm::Request);
    /// Sends `text` through Plaid Cloud's LLM off-thread, then posts `Msg::Llm { id }`.
    fn cloud_llm(&mut self, id: u64, kind: cloud::Kind, text: String, plan: Plan);
    /// Opens a pooled connection to the refine endpoint off-thread, so the request after release skips the handshake.
    fn warm(&mut self, base: &str);
    /// After a paste: watch the generation's field for manual corrections of `text`; posts `Msg::Edited`.
    fn watch_edits(&mut self, generation: u64, id: DateTime<Utc>, text: &str);
    /// Compare a watched field now (the next utterance started).
    fn check_edits(&mut self);
    /// Runs a voice command off-thread (`args` on stdin), then posts `Msg::Ran`.
    fn run(&mut self, generation: u64, action: &Action, args: &str, env: actions::Env);
    /// Streams a saved WAV through a fresh ASR session (no context) off-thread, then posts `Msg::Recovered`.
    fn recover(&mut self, id: DateTime<Utc>, audio: PathBuf, timeout: Duration);
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    /// Mic open. `toggled`: tap-started, keeps going after key-up until the next press or ⏎.
    Recording {
        toggled: bool,
    },
    /// Released: still capturing until the first quiet buffer, at most tailMs.
    Tail,
    /// Mic closing (`finished` false) or closed and waiting for the final result.
    Finalizing {
        finished: bool,
    },
    /// Final text is with the LLM (`Controller::refine`).
    Refining,
}

/// Per-utterance facts for the record.
struct Utterance {
    at: DateTime<Utc>,
    start: Instant,
    audio_end: Instant,
    release: Option<Instant>,
    first_text: Option<Instant>,
    trigger_ms: Option<i64>,
    mic_start_ms: Option<i64>,
    app: Option<String>,
    /// Bundle id (macOS) or executable name (Windows) of the frontmost app; commands only.
    bundle: Option<String>,
    window: Option<String>,
    /// Field text before the caret, for commands.
    before: Option<String>,
    mic: Option<String>,
    use_ctx: bool,
    recent: Vec<String>,
    begun: bool,
    ctx: Vec<String>,
    latest: String,
    audio: Vec<u8>,
    levels: Vec<i64>,
    /// Decides which of `audio` reaches the ASR.
    gate: Gate,
    silence: f64,
    actions: Vec<Action>,
    wake: String,
    /// ASR final before the local rules, set only when they changed it.
    asr: Option<String>,
    /// Recognized and refined by Plaid Cloud (serviceMode, read at key-down).
    cloud: bool,
    /// The refine plan sent with a Cloud `begin`; the device decides with the same one.
    plan: Option<Plan>,
}

impl Utterance {
    fn p90(&self) -> Option<i64> {
        pct(&self.levels, 0.9)
    }

    fn low_input(&self) -> bool {
        self.levels.len() >= 10 && self.p90().unwrap_or(0) < QUIET_RMS
    }

    fn low_note(&self) -> String {
        format!(
            "{} 音量过低，识别会漏字；可在 设置 › 通用 换输入设备",
            self.mic.as_deref().unwrap_or("麦克风")
        )
    }

    /// Context texts, newest first: text before the caret, recent outputs not already in it, the app.
    fn dialog(&self, before: Option<&str>) -> Vec<String> {
        if !self.use_ctx {
            return vec![];
        }
        let before = before.unwrap_or("");
        let mut d = vec![before.to_owned()];
        d.extend(
            self.recent
                .iter()
                .filter(|r| !before.contains(r.as_str()))
                .cloned(),
        );
        d.push(
            self.app
                .as_ref()
                .map(|a| format!("当前应用：{a}"))
                .unwrap_or_default(),
        );
        d.retain(|s| !s.is_empty());
        d
    }
}

pub fn pct(v: &[i64], p: f64) -> Option<i64> {
    let mut s = v.to_vec();
    s.sort_unstable();
    (!s.is_empty()).then(|| s[((s.len() - 1) as f64 * p).round() as usize])
}

/// Int16 RMS of s16le PCM.
pub fn rms(pcm: &[u8]) -> f64 {
    let n = pcm.len() / 2;
    if n == 0 {
        return 0.;
    }
    let sum: f64 = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b) as f64)
        .map(|s| s * s)
        .sum();
    (sum / n as f64).sqrt()
}

/// Attempts left waiting or running when the app last quit.
fn interrupted(mut store: Store) -> Store {
    let ids: Vec<_> = store
        .attempts
        .iter()
        .filter(|a| matches!(a.result.as_str(), "confirm" | "running"))
        .map(|a| a.at)
        .collect();
    for id in ids {
        store.update_attempt(id, |t| t.result = "cancelled".into());
    }
    store
}

fn label(a: &Action, args: &str) -> String {
    if args.is_empty() {
        a.name.clone()
    } else {
        format!("{}：{args}", a.name)
    }
}

/// The last `n` UTF-16 units, trimmed.
fn tail(s: &str, n: usize) -> String {
    let u = text::utf16(s);
    text::trim_ws_nl(&text::from_utf16(&u[u.len().saturating_sub(n)..])).to_owned()
}

/// ` styles=精炼,客气` for the refine log line; empty without styles.
fn style_names(p: &mut impl Platform, app: Option<&str>) -> String {
    let names: Vec<String> = llm::active_styles(|k| p.setting(k), app)
        .into_iter()
        .map(|s| s.name)
        .collect();
    if names.is_empty() {
        String::new()
    } else {
        format!(" styles={}", names.join(","))
    }
}

/// `llmTimeoutMs`.
fn timeout_ms(p: &mut impl Platform) -> u64 {
    p.setting("llmTimeoutMs")
        .and_then(|s| s.parse().ok())
        .unwrap_or(4000)
}

fn ms(from: Instant, to: Instant) -> i64 {
    to.saturating_duration_since(from).as_millis() as i64
}

struct Pending {
    id: DateTime<Utc>,
    release: Instant,
    low: Option<String>,
}

struct Refine {
    id: u64,
    raw: String,
    asr_at: Instant,
    /// The Cloud session doing the refine; dropping it cancels the server's LLM call.
    cloud: Option<Session>,
}

/// Background learning (BackgroundLearner): one request at a time, results applied only while idle.
#[derive(Default)]
struct Learner {
    /// No endpoint: through Plaid Cloud.
    queue: VecDeque<(DateTime<Utc>, String, Option<Endpoint>)>,
    running: Option<(u64, DateTime<Utc>, String)>,
    done: Vec<(DateTime<Utc>, String)>,
}

pub struct Controller {
    phase: Phase,
    generation: u64,
    session: Option<Session>,
    u: Option<Utterance>,
    pending: Vec<Pending>,
    refine: Option<Refine>,
    /// History's 重新润色 in flight: (llm request id, record).
    rerefining: Option<(u64, DateTime<Utc>, Instant)>,
    learn: Learner,
    llm_seq: u64,
    /// A matched command marked `confirm`, waiting for ⏎ (esc drops it). Not a phase: it doesn't block the next press.
    /// The last element is the attempt's id in commands.jsonl.
    confirm: Option<(Action, String, actions::Env, DateTime<Utc>)>,
    /// Commands running, by the generation that started them: the attempt to fill in when they end.
    running: Vec<(u64, DateTime<Utc>)>,
    /// Command output pastes waiting for target verification (not recorded).
    command_pastes: Vec<DateTime<Utc>>,
    /// Record whose audio is being recognized again; dictation waits until it is done.
    recovering: Option<DateTime<Utc>>,
    pub store: Store,
}

/// What the key tap may swallow, mirrored into atomics after every message.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Keys {
    pub toggled: bool,
    pub escape: bool,
    pub enter: bool,
}

impl Controller {
    pub fn new(store: Store) -> Self {
        Controller {
            phase: Phase::Idle,
            generation: 0,
            session: None,
            u: None,
            pending: vec![],
            refine: None,
            rerefining: None,
            learn: Learner::default(),
            llm_seq: 0,
            confirm: None,
            running: vec![],
            command_pastes: vec![],
            recovering: None,
            store: interrupted(store),
        }
    }

    /// Learning work is queued or in flight.
    pub fn learning(&self) -> bool {
        let l = &self.learn;
        l.running.is_some() || !l.queue.is_empty() || !l.done.is_empty()
    }

    /// Record being recognized again, if any.
    pub fn recovering(&self) -> Option<DateTime<Utc>> {
        self.recovering
    }

    pub fn rerefining(&self) -> bool {
        self.rerefining.is_some()
    }

    pub fn idle(&self) -> bool {
        self.phase == Phase::Idle
    }

    pub fn keys(&self) -> Keys {
        Keys {
            toggled: self.phase == Phase::Recording { toggled: true },
            escape: !self.idle() || self.confirm.is_some(),
            enter: matches!(self.phase, Phase::Recording { .. }) || self.confirm.is_some(),
        }
    }

    fn capturing(&self) -> bool {
        matches!(
            self.phase,
            Phase::Recording { .. } | Phase::Tail | Phase::Finalizing { finished: false }
        )
    }

    /// Mic open in the sense of Swift's `capturing`: recording or tail.
    fn mic_open(&self) -> bool {
        matches!(self.phase, Phase::Recording { .. } | Phase::Tail)
    }

    pub fn handle(&mut self, msg: Msg, p: &mut impl Platform) {
        self.dispatch(msg, p);
        if self.idle() {
            self.learn_resume(p);
        }
    }

    fn dispatch(&mut self, msg: Msg, p: &mut impl Platform) {
        let current = |g: u64| g == self.generation;
        match msg {
            Msg::Key { action, trigger_ms } => match action {
                gesture::Action::StartHold => self.start(trigger_ms, p),
                gesture::Action::StartToggle => {
                    self.start(trigger_ms, p);
                    if self.phase == (Phase::Recording { toggled: false }) {
                        self.phase = Phase::Recording { toggled: true };
                        p.hud(Hud::Continuous);
                    }
                }
                gesture::Action::Stop => self.stop(p),
                gesture::Action::Cancel => {
                    p.log("hotkey chord cancelled");
                    self.cancel(p);
                }
            },
            Msg::Return => {
                if matches!(self.phase, Phase::Recording { .. }) {
                    p.log("return");
                    self.stop(p);
                } else if let Some((a, args, env, id)) = self.confirm.take() {
                    if self.idle() {
                        self.launch(&a, &args, env, id, p);
                    } else {
                        self.store.update_attempt(id, |t| t.result = "cancelled".into());
                    }
                }
            }
            Msg::Escape => {
                if self.idle()
                    && let Some((a, ..)) = self.confirm.as_ref()
                {
                    p.log(&format!("action {} cancelled", a.name));
                    self.drop_confirm();
                    p.hud(Hud::Hide);
                } else if self.phase == Phase::Refining {
                    p.log("refine skipped");
                    self.skip_refine(true, p);
                } else if !self.idle() {
                    p.log("esc");
                    self.cancel(p);
                }
            }
            Msg::Audio { generation, pcm } if current(generation) && self.capturing() => {
                self.audio(pcm, p)
            }
            Msg::CaptureEnded { generation }
                if current(generation) && self.phase == (Phase::Finalizing { finished: false }) =>
            {
                self.phase = Phase::Finalizing { finished: true };
                // Begins without context if the field read is still out (a Cloud start must carry the plan).
                self.begin(vec![], p);
                if let Some(u) = &self.u {
                    p.log(&format!(
                        "end levels={} p90={} sentMs={}",
                        u.levels.len(),
                        u.p90().unwrap_or(0),
                        u.gate.sent_bytes / 32
                    ));
                }
                if let Some(s) = &self.session {
                    s.finish();
                }
            }
            Msg::Field {
                generation,
                before,
                window,
            } if current(generation) => {
                let before_asr = before.as_deref().map(|b| tail(b, ASR_BEFORE));
                if let Some(u) = &mut self.u {
                    u.window = window;
                    u.before = before;
                }
                let dialog = self
                    .u
                    .as_ref()
                    .filter(|u| u.use_ctx)
                    .map(|u| u.dialog(before_asr.as_deref()));
                if let Some(d) = dialog {
                    self.begin(d, p);
                }
            }
            Msg::Timer { generation, timer } if current(generation) => match timer {
                Timer::Context => {
                    if let Some(d) = self.u.as_ref().map(|u| u.dialog(None)) {
                        self.begin(d, p);
                    }
                }
                Timer::Tail => {
                    if self.phase == Phase::Tail {
                        self.end(p);
                    }
                }
                Timer::LowInput => {
                    if self.idle()
                        && let Some(n) = self.u.as_ref().map(Utterance::low_note)
                    {
                        p.hud(Hud::Flash(n));
                    }
                }
                Timer::Confirm => self.drop_confirm(),
                Timer::Refine => {
                    if let Some(r) = self.refine.take_if(|r| r.cloud.is_some()) {
                        p.log("refine: Cloud did not answer in time");
                        let f = llm::Failure {
                            message: "Cloud refine timed out".into(),
                            timed_out: true,
                        };
                        self.refined(r, Err(f), p);
                    }
                }
            },
            Msg::Session {
                event: Event::Usage(u),
                ..
            } => p.log(&format!(
                "cloud usage receivedMs={} sentMs={} asrWaitMs={} llmMs={} tokens={}/{} costMicros={} balanceMicros={}",
                u.received_ms, u.sent_ms, u.asr_wait_ms, u.llm_ms, u.llm_in, u.llm_out, u.cost_micros, u.balance_micros
            )),
            Msg::Session {
                generation,
                event: Event::Refined(r),
            } if current(generation) => {
                if let Some(rf) = self.refine.take_if(|r| r.cloud.is_some()) {
                    self.refined(rf, r, p);
                }
            }
            Msg::Session { generation, event } if current(generation) && self.session.is_some() => {
                match event {
                    Event::Text(t) => {
                        let Some(u) = &mut self.u else { return };
                        if t.is_empty() {
                            return;
                        }
                        u.first_text.get_or_insert_with(Instant::now);
                        let command = actions::parse(&t, &u.actions, &u.wake) != Parsed::Dictation;
                        u.latest = t.clone();
                        p.hud(Hud::Text { text: t, command });
                    }
                    Event::Done(r) => self.finished(r, p),
                    Event::Refined(_) | Event::Usage(_) => {}
                }
            }
            Msg::Verified {
                generation,
                id,
                text,
                ok,
            } => self.verified(generation, id, text, ok, p),
            Msg::Llm { id, result } => self.llm_done(id, result, p),
            Msg::Edited { id, pasted, edited } => {
                p.log(&format!("edit learned chars={}", edited.chars().count()));
                self.store.set_edited(id, pasted, edited);
                self.relearn(p);
            }
            Msg::Ran {
                generation,
                action,
                outcome,
            } => self.ran(generation, action, outcome, p),
            Msg::Recover { id } => self.recover(id, p),
            Msg::Recovered { id, result } => self.recovered(id, result, p),
            Msg::Clear => {
                if !self.idle() || self.recovering.is_some() {
                    return p.hud(Hud::Flash("请等当前听写或重试结束后再试".into()));
                }
                self.store.clear();
                self.relearn(p);
                p.log("history cleared");
            }
            Msg::Refine { id } => self.rerefine(id, p),
            Msg::Delete { ids } => {
                if self.recovering.is_some_and(|r| ids.contains(&r)) {
                    return p.hud(Hud::Flash("请等重试结束后再删除".into()));
                }
                let n = self.store.remove(&ids);
                self.relearn(p);
                p.log(&format!("records deleted n={n}"));
            }
            _ => {}
        }
    }

    fn begin(&mut self, dialog: Vec<String>, p: &mut impl Platform) {
        let (Some(u), Some(_)) = (&self.u, &self.session) else {
            return;
        };
        if u.begun {
            return;
        }
        let plan = u.cloud.then(|| self.dictation_plan(p));
        let (Some(u), Some(s)) = (&mut self.u, &self.session) else {
            return;
        };
        u.begun = true;
        u.ctx = dialog.clone();
        u.plan = plan.clone();
        s.begin(dialog, plan);
    }

    fn start(&mut self, trigger_ms: Option<i64>, p: &mut impl Platform) {
        if self.recovering.is_some() {
            return p.hud(Hud::Flash("正在重新识别历史录音，请稍后再试".into()));
        }
        match self.phase {
            Phase::Tail | Phase::Finalizing { .. } => {
                if self.capturing() {
                    p.stop_capture(self.generation);
                    if let Some(u) = &mut self.u {
                        u.audio_end = Instant::now();
                    }
                }
                self.session = None;
                self.phase = Phase::Idle;
                let partial = self
                    .u
                    .as_ref()
                    .map(|u| u.latest.clone())
                    .unwrap_or_default();
                self.preserve_failure("被新的听写中断", &partial, p);
            }
            Phase::Refining => self.skip_refine(false, p),
            Phase::Recording { .. } => {
                return p.log("start skipped: previous utterance still recording");
            }
            Phase::Idle => {}
        }
        self.drop_confirm();
        self.generation += 1;
        let g = self.generation;
        let cloud = p.setting("serviceMode").as_deref() == Some("cloud");
        let session = match p.open_session(g, cloud) {
            Ok(s) => s,
            Err(e) => {
                p.log(&format!("start failed: {e}"));
                return p.hud(Hud::Failed(e));
            }
        };
        p.hud(Hud::Listen);
        let now = Instant::now();
        let use_ctx = p.setting("asrContext").as_deref() != Some("0");
        let silence = p
            .setting("silenceRMS")
            .and_then(|s| s.parse().ok())
            .unwrap_or(SILENCE_DEFAULT);
        let (actions, wake) = p.actions();
        let mut recent = self.store.recent(3, Utc::now());
        recent.reverse();
        let (app, bundle) = p.front_app(g);
        self.u = Some(Utterance {
            at: Utc::now(),
            start: now,
            audio_end: now,
            release: None,
            first_text: None,
            trigger_ms,
            mic_start_ms: None,
            app,
            bundle,
            window: None,
            before: None,
            mic: None,
            use_ctx,
            recent,
            begun: false,
            ctx: vec![],
            latest: String::new(),
            audio: vec![],
            levels: vec![],
            gate: Gate::default(),
            silence,
            actions,
            wake,
            asr: None,
            cloud,
            plan: None,
        });
        self.session = Some(session);
        self.phase = Phase::Recording { toggled: false };
        if !use_ctx {
            self.begin(vec![], p);
        }
        p.read_field(g);
        p.check_edits();
        if use_ctx {
            p.timer(
                CONTEXT_WAIT,
                Msg::Timer {
                    generation: g,
                    timer: Timer::Context,
                },
            );
        }
        match p.start_capture(g) {
            Ok(name) => {
                let u = self.u.as_mut().unwrap();
                u.mic_start_ms = Some(ms(u.start, Instant::now()));
                p.log(&format!(
                    "start mic={name} triggerMs={} micStartMs={}",
                    u.trigger_ms.unwrap_or(-1),
                    u.mic_start_ms.unwrap_or(-1)
                ));
                u.mic = Some(name);
                if !cloud
                    && llm::level(|k| p.setting(k)) > Level::Off
                    && let Some(ep) = Self::configured(p)
                    && !llm::loopback(&ep.base)
                {
                    p.warm(&ep.base);
                }
            }
            Err(e) => {
                p.log(&format!("start failed: {e}"));
                self.session = None;
                self.phase = Phase::Idle;
                let msg = format!("麦克风：{e}");
                self.preserve_failure(&msg, "", p);
                p.hud(Hud::Failed(msg));
            }
        }
    }

    fn audio(&mut self, pcm: Vec<u8>, p: &mut impl Platform) {
        let Some(u) = &mut self.u else { return };
        let r = rms(&pcm);
        u.audio.extend_from_slice(&pcm);
        u.levels.push(r as i64);
        let out = u.gate.push(pcm, r);
        if let Some(s) = &self.session
            && !out.is_empty()
        {
            s.append(out);
        }
        p.hud(Hud::Level(r));
        // 2 s in without any transcript and the input still near silence: say which device is recording.
        if u.first_text.is_none() {
            if r >= QUIET_RMS as f64 {
                p.hud(Hud::Unquiet);
            } else if u.levels.len() == 20 && u.low_input() {
                p.hud(Hud::Quiet(format!(
                    "几乎没有声音 · {}",
                    u.mic.as_deref().unwrap_or("麦克风")
                )));
            }
        }
        if r < u.silence && self.phase == Phase::Tail {
            self.end(p);
        }
    }

    fn stop(&mut self, p: &mut impl Platform) {
        let Phase::Recording { .. } = self.phase else {
            return;
        };
        self.phase = Phase::Tail;
        if let Some(u) = &mut self.u {
            u.release = Some(Instant::now());
        }
        p.log("stop");
        p.hud(Hud::Finishing);
        let tail = p
            .setting("tailMs")
            .and_then(|s| s.parse().ok())
            .unwrap_or(300u64);
        p.timer(
            Duration::from_millis(tail),
            Msg::Timer {
                generation: self.generation,
                timer: Timer::Tail,
            },
        );
    }

    /// Close the mic; the session finishes once the capture's last buffer has been appended.
    fn end(&mut self, p: &mut impl Platform) {
        if !self.mic_open() {
            return;
        }
        p.stop_capture(self.generation);
        if let Some(u) = &mut self.u {
            u.audio_end = Instant::now();
        }
        self.phase = Phase::Finalizing { finished: false };
    }

    /// Drops audio and results in every waiting phase and invalidates pending pastes.
    fn cancel(&mut self, p: &mut impl Platform) {
        if self.mic_open() {
            p.stop_capture(self.generation);
        }
        self.generation += 1;
        self.session = None;
        self.refine = None;
        self.phase = Phase::Idle;
        p.hud(Hud::Hide);
    }

    fn finished(&mut self, r: Result<String, String>, p: &mut impl Platform) {
        match &r {
            Ok(t) => p.log(&format!(
                "finished chars={}{}",
                t.chars().count(),
                if self.mic_open() {
                    " (still recording)"
                } else {
                    ""
                }
            )),
            Err(e) => p.log(&format!("finished error: {e}")),
        }
        // A server error can end the session while toggled and still capturing.
        if self.capturing() {
            p.stop_capture(self.generation);
            if let Some(u) = &mut self.u {
                u.audio_end = Instant::now();
            }
        }
        // A Cloud session stays open for the server's refine.
        let session = self
            .session
            .take()
            .filter(|_| self.u.as_ref().is_some_and(|u| u.cloud));
        self.phase = Phase::Idle;
        let Some(u) = &self.u else { return };
        let partial = u.latest.clone();
        match r {
            Ok(raw) if !raw.is_empty() => {
                match actions::parse(&raw, &u.actions, &u.wake) {
                    Parsed::Dictation => {}
                    Parsed::Unknown(rest) => {
                        p.log(&format!(
                            "action: no match for {} chars",
                            rest.chars().count()
                        ));
                        self.store.put_attempt(Attempt {
                            at: u.at,
                            app: u.app.clone(),
                            heard: raw.clone(),
                            result: "nomatch".into(),
                            ..Default::default()
                        });
                        return p.hud(Hud::Flash(if rest.is_empty() {
                            "没有听到命令".into()
                        } else {
                            format!("没有匹配的命令：{rest}")
                        }));
                    }
                    Parsed::Run(a, args, sound) => {
                        let a = a.clone();
                        if sound {
                            p.log(&format!("action {} matched by sound", a.name));
                        }
                        return self.command(a, args, raw, sound, p);
                    }
                }
                self.refine_or_deliver(raw, session, p);
            }
            Ok(_) => {
                if u.p90().unwrap_or(0) >= QUIET_RMS && u.audio.len() >= 16000 {
                    self.preserve_failure("未识别到文字", &partial, p);
                    p.hud(Hud::Failed("未识别到文字，可在历史里重试".into()));
                } else if u.low_input() {
                    p.hud(Hud::Flash(u.low_note()));
                } else {
                    p.hud(Hud::Hide);
                }
                p.log("empty result");
            }
            Err(e) => {
                self.preserve_failure(&e, &partial, p);
                p.hud(Hud::Failed(format!("{e}；可在历史里查看")));
            }
        }
    }

    fn record(
        &self,
        raw: &str,
        text: &str,
        llm: &str,
        llm_ms: Option<i64>,
        asr_at: Instant,
        p: &mut impl Platform,
    ) -> Record {
        let u = self.u.as_ref().expect("utterance");
        let release = u.release.unwrap_or(u.audio_end);
        let keep = p.setting("keepAudio").as_deref() != Some("0");
        Record {
            at: u.at,
            app: u.app.clone(),
            audio_ms: ms(u.start, u.audio_end),
            sent_ms: Some(u.gate.sent_bytes as i64 / 32),
            first_text_ms: u.first_text.map(|t| ms(u.start, t)),
            asr_ms: ms(release, asr_at),
            // Cloud's refine runs on the server right after the final; release → paste covers it.
            llm_ms: llm_ms.filter(|_| !u.cloud),
            llm: llm.into(),
            engine: u.cloud.then(|| "cloud".into()),
            total_ms: ms(release, Instant::now()),
            raw: raw.into(),
            text: text.into(),
            asr: u.asr.clone(),
            audio: if keep {
                self.store.save_audio(&u.audio, u.at)
            } else {
                None
            },
            noise_rms: pct(&u.levels, 0.1),
            speech_rms: u.p90(),
            ctx: (!u.ctx.is_empty()).then(|| u.ctx.clone()),
            mic: u.mic.clone(),
            trigger_ms: u.trigger_ms,
            mic_start_ms: u.mic_start_ms,
            ..Default::default()
        }
    }

    fn preserve_failure(&mut self, error: &str, partial: &str, p: &mut impl Platform) {
        if self.u.is_none() {
            return;
        }
        let mut r = self.record(partial, partial, "off", None, Instant::now(), p);
        r.outcome = Some("failed".into());
        r.error = Some(error.into());
        self.store.add(r);
        p.log(&format!(
            "saved failed utterance chars={}",
            partial.chars().count()
        ));
    }

    /// Persists first (as "saved"): target verification may time out or a new utterance may supersede it.
    /// `paste` false: only saved (a refine superseded by the next utterance).
    #[allow(clippy::too_many_arguments)]
    fn deliver(
        &mut self,
        raw: &str,
        text: &str,
        llm: &str,
        llm_ms: Option<i64>,
        asr_at: Instant,
        paste: bool,
        p: &mut impl Platform,
    ) {
        let mut r = self.record(raw, text, llm, llm_ms, asr_at, p);
        r.outcome = Some("saved".into());
        let id = r.at;
        self.store.add(r);
        if !paste {
            return;
        }
        let u = self.u.as_ref().expect("utterance");
        self.pending.push(Pending {
            id,
            release: u.release.unwrap_or(u.audio_end),
            low: u.low_input().then(|| u.low_note()),
        });
        p.verify_target(self.generation, id, text.into());
    }

    fn verified(
        &mut self,
        generation: u64,
        id: DateTime<Utc>,
        text: String,
        ok: bool,
        p: &mut impl Platform,
    ) {
        if let Some(i) = self.command_pastes.iter().position(|c| *c == id) {
            self.command_pastes.remove(i);
            let mine = self.idle() && generation == self.generation;
            let posted = ok && mine && p.paste(generation, &text);
            p.log(&format!(
                "command delivery {}",
                if posted { "posted" } else { "dropped" }
            ));
            if mine {
                p.hud(if posted {
                    Hud::Done(text)
                } else {
                    Hud::Flash("输入位置已变化，命令输出未粘贴".into())
                });
            }
            return;
        }
        let Some(i) = self.pending.iter().position(|d| d.id == id) else {
            return;
        };
        let d = self.pending.remove(i);
        let mine = self.idle() && generation == self.generation;
        let posted = ok && mine && p.paste(generation, &text);
        p.log(&format!(
            "delivery {}",
            if posted { "posted" } else { "saved" }
        ));
        let total = posted.then(|| ms(d.release, Instant::now()));
        self.store.set_delivery(
            id,
            posted,
            (!posted).then(|| "输入位置已变化或无法确认，未自动粘贴".into()),
            total,
        );
        self.relearn(p);
        if posted {
            if p.setting("learnEdits").as_deref() != Some("0") {
                p.watch_edits(generation, id, &text);
            }
            let raw = self
                .store
                .all
                .iter()
                .rev()
                .find(|r| r.at == id && r.llm == "off")
                .map(|r| r.raw.clone());
            if let Some(raw) = raw.filter(|r| r.chars().any(|c| c as u32 >= 0x3400)) {
                self.learn_submit(id, raw, p);
            }
        }
        if !mine {
            return;
        }
        if posted {
            p.hud(Hud::Done(text));
            if d.low.is_some() {
                p.timer(
                    Duration::from_millis(600),
                    Msg::Timer {
                        generation,
                        timer: Timer::LowInput,
                    },
                );
            }
        } else {
            p.hud(Hud::Saved(
                "输入位置已变化或无法确认，本次结果没有自动粘贴。".into(),
            ));
        }
    }

    /// A voice command replaces the paste. Not recorded in records.jsonl (it would feed ASR/LLM context and
    /// learning) but in commands.jsonl; the trace has one line per run, without text.
    fn command(
        &mut self,
        a: Action,
        args: String,
        raw: String,
        sound: bool,
        p: &mut impl Platform,
    ) {
        let u = self.u.as_ref().expect("utterance");
        let id = u.at;
        self.store.put_attempt(Attempt {
            at: id,
            app: u.app.clone(),
            heard: raw.clone(),
            action: Some(a.name.clone()),
            args: args.clone(),
            sound,
            result: if a.confirm { "confirm" } else { "running" }.into(),
            ..Default::default()
        });
        let env = vec![
            ("PLAID_TEXT", raw),
            ("PLAID_ARGS", args.clone()),
            ("PLAID_APP", u.app.clone().unwrap_or_default()),
            ("PLAID_BUNDLE", u.bundle.clone().unwrap_or_default()),
            ("PLAID_WINDOW", u.window.clone().unwrap_or_default()),
            ("PLAID_BEFORE", u.before.clone().unwrap_or_default()),
        ];
        if !a.confirm {
            return self.launch(&a, &args, env, id, p);
        }
        p.hud(Hud::Ask(label(&a, &args)));
        p.timer(
            CONFIRM_WAIT,
            Msg::Timer {
                generation: self.generation,
                timer: Timer::Confirm,
            },
        );
        self.confirm = Some((a, args, env, id));
    }

    /// A command waiting for ⏎ that won't run: esc, its timer, or the next dictation.
    fn drop_confirm(&mut self) {
        if let Some((.., id)) = self.confirm.take() {
            self.store
                .update_attempt(id, |t| t.result = "cancelled".into());
        }
    }

    fn launch(
        &mut self,
        a: &Action,
        args: &str,
        env: actions::Env,
        id: DateTime<Utc>,
        p: &mut impl Platform,
    ) {
        self.store
            .update_attempt(id, |t| t.result = "running".into());
        self.running.push((self.generation, id));
        p.hud(Hud::Running(label(a, args)));
        p.run(self.generation, a, args, env);
    }

    /// HUD only for the utterance that started it, and only while nothing newer is on screen.
    fn ran(&mut self, generation: u64, a: Action, r: actions::Outcome, p: &mut impl Platform) {
        if let Some(i) = self.running.iter().position(|x| x.0 == generation) {
            let (_, id) = self.running.remove(i);
            self.store.update_attempt(id, |t| t.finish(&r));
        }
        let mine = self.idle() && generation == self.generation;
        if !r.ok() {
            if mine {
                p.hud(Hud::Failed(format!("{} 失败：{}", a.name, r.failure())));
            }
            return;
        }
        match a.output {
            actions::Output::None if mine => p.hud(Hud::Done(a.name)),
            actions::Output::Hud if mine => {
                let line = r.last_line();
                p.hud(Hud::Note(if line.is_empty() {
                    a.name.clone()
                } else {
                    line.to_owned()
                }));
            }
            actions::Output::Paste => {
                let t = r.out.strip_suffix('\n').unwrap_or(&r.out);
                if t.is_empty() {
                    if mine {
                        p.hud(Hud::Done(a.name));
                    }
                } else if mine {
                    let id = Utc::now();
                    self.command_pastes.push(id);
                    p.verify_target(generation, id, t.to_owned());
                }
            }
            _ => {}
        }
    }

    /// The refine/learning endpoint from settings (refine needn't be enabled for learning to use it).
    fn recover(&mut self, id: DateTime<Utc>, p: &mut impl Platform) {
        if !self.idle() || self.recovering.is_some() {
            return p.hud(Hud::Flash("请等当前听写或重试结束后再试".into()));
        }
        let Some(r) = self.store.all.iter().rev().find(|r| r.at == id) else {
            return;
        };
        let Some(audio) = r
            .audio
            .as_ref()
            .map(|a| self.store.audio_dir().join(a))
            .filter(|f| f.exists())
        else {
            return p.hud(Hud::Flash("这条记录没有保留录音".into()));
        };
        // The whole file goes up at once; the server still needs about real time for the final pass.
        let timeout = Duration::from_millis(30_000.max(r.audio_ms.max(0) as u64 + 10_000));
        p.log(&format!("recover audioMs={}", r.audio_ms));
        self.recovering = Some(id);
        p.hud(Hud::Running("正在重新识别；不会自动粘贴…".into()));
        p.recover(id, audio, timeout);
    }

    fn recovered(
        &mut self,
        id: DateTime<Utc>,
        result: Result<String, String>,
        p: &mut impl Platform,
    ) {
        if self.recovering != Some(id) {
            return;
        }
        self.recovering = None;
        let exists = self.store.all.iter().any(|r| r.at == id);
        let hud = match result {
            Ok(t) if text::trim_ws_nl(&t).is_empty() => {
                Hud::Flash("没有识别到文字，原记录和录音已保留".into())
            }
            Ok(_) if !exists => Hud::Flash("原记录已删除，无法保存重试结果".into()),
            Ok(t) => {
                p.log(&format!("recovered chars={}", t.chars().count()));
                let ruled = Rules::load(|k| p.setting(k)).apply(&t);
                self.store.recovered(id, t, ruled);
                Hud::Note("识别结果已保存在原记录中".into())
            }
            Err(e) => {
                p.log(&format!("recover failed: {e}"));
                Hud::Failed(format!("重新识别失败：{e}"))
            }
        };
        p.hud(hud);
    }

    fn configured(p: &mut impl Platform) -> Option<Endpoint> {
        let ep = llm::endpoint(|k| p.setting(k));
        (!ep.base.is_empty()).then_some(ep)
    }

    /// The refine plan for the current utterance from the settings now. The prompt is left empty when the level
    /// runs no LLM.
    fn dictation_plan(&self, p: &mut impl Platform) -> Plan {
        let u = self.u.as_ref().expect("utterance");
        let level = llm::level(|k| p.setting(k));
        let (system, user) = if level > Level::Off {
            let app = if p.setting("llmAppContext").as_deref() != Some("0") {
                [u.app.clone(), u.window.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" — ")
            } else {
                String::new()
            };
            let n = p
                .setting("llmHistory")
                .and_then(|s| s.parse().ok())
                .unwrap_or(3);
            let history = self.store.recent(n, Utc::now());
            let hotwords = p.terms(true).join(", ");
            let fixes = self.store.fixes();
            let fill = refine::Fill {
                app: &app,
                front: u.app.as_deref(),
                history: &history,
                hotwords: &hotwords,
                fixes: &fixes,
            };
            refine::prompt(|k| p.setting(k), level == Level::Light, &fill)
        } else {
            Default::default()
        };
        Plan {
            level,
            rules: Rules::load(|k| p.setting(k)),
            min_chars: p
                .setting("llmMinChars")
                .and_then(|s| s.parse().ok())
                .unwrap_or(REFINE_MIN_CHARS),
            system,
            user,
            commands: u
                .actions
                .iter()
                .filter(|a| !text::trim_ws_nl(&a.run).is_empty())
                .flat_map(|a| a.triggers().into_iter().map(str::to_owned))
                .collect(),
            wake: u.wake.clone(),
            timeout_ms: timeout_ms(p),
        }
    }

    /// A plan for text that always goes to the LLM (重新润色, learning).
    fn direct_plan(
        &self,
        light: bool,
        app: &str,
        front: Option<&str>,
        timeout_ms: u64,
        p: &mut impl Platform,
    ) -> Plan {
        let hotwords = p.terms(true).join(", ");
        let fixes = self.store.fixes();
        let fill = refine::Fill {
            app,
            front,
            history: &[],
            hotwords: &hotwords,
            fixes: &fixes,
        };
        refine::direct(|k| p.setting(k), light, &fill, timeout_ms)
    }

    /// The request to the configured endpoint (an empty base fails with "no base URL").
    fn byok_request(
        ep: &Endpoint,
        plan: &Plan,
        user: &str,
        p: &mut impl Platform,
    ) -> Result<llm::Request, String> {
        let key = p.llm_key(&ep.base);
        let extra = p.setting("llmExtraBody");
        plan.request(ep, key.as_deref(), user, extra.as_deref())
    }

    /// Optional LLM pass between the ASR final and the paste. Fails open: errors and timeouts paste the raw text.
    /// With a Cloud session the server is already refining with the plan sent at `begin`.
    fn refine_or_deliver(&mut self, asr: String, cloud: Option<Session>, p: &mut impl Platform) {
        let asr_at = Instant::now();
        let plan = match self.u.as_mut().and_then(|u| u.plan.take()) {
            Some(plan) => plan,
            None => self.dictation_plan(p),
        };
        let d = plan.decide(&asr);
        let raw = d.text;
        if raw != asr {
            p.log(&format!(
                "rules: {} → {} chars",
                asr.chars().count(),
                raw.chars().count()
            ));
            if let Some(u) = self.u.as_mut() {
                u.asr = Some(asr);
            }
        }
        let user = match d.llm {
            Ok(user) => user,
            Err(skip) => {
                if skip == Skip::Short {
                    p.log("refine skipped: short");
                }
                return self.deliver(&raw, &raw, "off", None, asr_at, true, p);
            }
        };
        let front = self.u.as_ref().and_then(|u| u.app.clone());
        let styles = style_names(p, front.as_deref());
        self.llm_seq += 1;
        if let Some(session) = cloud {
            p.log(&format!(
                "refine cloud chars={}{styles}",
                raw.chars().count()
            ));
            p.timer(
                Duration::from_millis(plan.timeout_ms) + CLOUD_GRACE,
                Msg::Timer {
                    generation: self.generation,
                    timer: Timer::Refine,
                },
            );
            self.refine = Some(Refine {
                id: self.llm_seq,
                raw: raw.clone(),
                asr_at,
                cloud: Some(session),
            });
        } else {
            let ep = Self::configured(p).unwrap_or_else(|| Endpoint::new("openai", "", ""));
            let req = match Self::byok_request(&ep, &plan, &user, p) {
                Ok(req) => req,
                Err(e) => return self.deliver(&raw, &raw, &e, Some(0), asr_at, true, p),
            };
            p.log(&format!(
                "refine model={} chars={}{styles}",
                ep.model,
                raw.chars().count(),
            ));
            p.llm(self.llm_seq, req);
            self.refine = Some(Refine {
                id: self.llm_seq,
                raw: raw.clone(),
                asr_at,
                cloud: None,
            });
        }
        self.phase = Phase::Refining;
        p.hud(Hud::Refining(raw));
    }

    /// History's 重新润色: the record's text after the local rules goes through the LLM at the level its app gets
    /// (通顺 when that level skips the LLM, since the user asked for it), without history context.
    fn rerefine(&mut self, id: DateTime<Utc>, p: &mut impl Platform) {
        if self.rerefining.is_some() {
            return p.hud(Hud::Flash("上一条还在润色".into()));
        }
        let Some(r) = self.store.all.iter().rev().find(|r| r.at == id).cloned() else {
            return;
        };
        if r.raw.trim().is_empty() {
            return p.hud(Hud::Flash("这条记录没有可润色的文字".into()));
        }
        let cloud = p.setting("serviceMode").as_deref() == Some("cloud");
        let ep = Self::configured(p);
        if ep.is_none() && !cloud {
            return p.hud(Hud::Flash(
                "没有可用的 LLM：在「设置 → 服务」里填好 LLM 接口和 key".into(),
            ));
        }
        let light = llm::level(|k| p.setting(k)) == Level::Light;
        let app = r.app.clone().unwrap_or_default();
        let plan = self.direct_plan(light, &app, r.app.as_deref(), timeout_ms(p).max(15_000), p);
        let Some(ep) = ep.filter(|_| !cloud) else {
            self.llm_seq += 1;
            self.rerefining = Some((self.llm_seq, id, Instant::now()));
            p.log(&format!("rerefine cloud chars={}", r.raw.chars().count()));
            return p.cloud_llm(self.llm_seq, cloud::Kind::Refine, r.raw.clone(), plan);
        };
        match Self::byok_request(&ep, &plan, &plan.user_with(&r.raw), p) {
            Err(e) => p.hud(Hud::Flash(format!("重新润色失败：{e}"))),
            Ok(req) => {
                self.llm_seq += 1;
                self.rerefining = Some((self.llm_seq, id, Instant::now()));
                let styles = style_names(p, r.app.as_deref());
                p.log(&format!(
                    "rerefine model={} chars={}{styles}",
                    ep.model,
                    r.raw.chars().count(),
                ));
                p.llm(self.llm_seq, req);
            }
        }
    }

    /// esc (paste the raw text) or a new utterance (only save it).
    fn skip_refine(&mut self, paste: bool, p: &mut impl Platform) {
        self.phase = Phase::Idle;
        if let Some(r) = self.refine.take() {
            self.deliver(&r.raw, &r.raw, "skipped", None, Instant::now(), paste, p);
        }
    }

    /// The dictation's refine ended (BYOK reply, Cloud `refined`, or the Cloud deadline): paste.
    fn refined(&mut self, r: Refine, result: Result<String, llm::Failure>, p: &mut impl Platform) {
        self.phase = Phase::Idle;
        let elapsed = ms(r.asr_at, Instant::now());
        let llm_ms = Some(elapsed);
        match result {
            Ok(t) => {
                let t = Rules::load(|k| p.setting(k)).format(&t);
                p.log(&format!("refined ms={elapsed} chars={}", t.chars().count()));
                self.deliver(&r.raw, &t, "ok", llm_ms, r.asr_at, true, p);
            }
            Err(f) => {
                p.log(&format!("refine failed: {}", f.message));
                let llm = if f.timed_out {
                    "timeout"
                } else {
                    f.message.as_str()
                };
                self.deliver(&r.raw, &r.raw, llm, llm_ms, r.asr_at, true, p);
            }
        }
    }

    fn llm_done(&mut self, id: u64, result: Result<String, llm::Failure>, p: &mut impl Platform) {
        if let Some(r) = self.refine.take_if(|r| r.id == id && r.cloud.is_none()) {
            self.refined(r, result, p);
        } else if let Some((_, rid, at)) = self.rerefining.take_if(|j| j.0 == id) {
            let note = match result {
                Ok(t) => {
                    let t = Rules::load(|k| p.setting(k)).format(&t);
                    p.log(&format!("rerefined chars={}", t.chars().count()));
                    // llm_ms always moves, so 历史 sees the run end even when the text comes back unchanged.
                    let llm_ms = Some(ms(at, Instant::now()));
                    self.store.update(rid, |r| {
                        r.text = t;
                        r.llm = "ok".into();
                        r.llm_ms = llm_ms;
                    });
                    "已重新润色，结果在历史里".to_string()
                }
                Err(f) => {
                    p.log(&format!("rerefine failed: {}", f.message));
                    format!(
                        "重新润色失败：{}",
                        if f.timed_out { "超时" } else { &f.message }
                    )
                }
            };
            if self.idle() {
                p.hud(Hud::Flash(note));
            }
        } else if let Some((_, rid, raw)) = self.learn.running.take_if(|j| j.0 == id) {
            match result {
                Ok(t) if t != raw && p.setting("autoLearn").as_deref() != Some("0") => {
                    self.learn.done.push((rid, t))
                }
                Ok(_) => {}
                Err(f) => p.log(&format!("learn failed: {}", f.message)),
            }
        }
    }

    /// The learning endpoint: the refine settings when filled in and usable (a stored key, or a local server).
    /// Otherwise no learning, rather than a failing request per dictation.
    fn learn_submit(&mut self, id: DateTime<Utc>, raw: String, p: &mut impl Platform) {
        if p.setting("autoLearn").as_deref() == Some("0") {
            return;
        }
        let ep = if p.setting("serviceMode").as_deref() == Some("cloud") {
            None
        } else {
            let Some(ep) = Self::configured(p)
                .filter(|ep| llm::loopback(&ep.base) || p.llm_key(&ep.base).is_some())
            else {
                return;
            };
            Some(ep)
        };
        let q = &mut self.learn.queue;
        q.push_back((id, raw, ep));
        q.drain(..q.len().saturating_sub(LEARN_QUEUE));
    }

    /// While idle: apply finished learning results, then start the next request.
    fn learn_resume(&mut self, p: &mut impl Platform) {
        if p.setting("autoLearn").as_deref() == Some("0") {
            self.learn.queue.clear();
            self.learn.done.clear();
            return;
        }
        let done = std::mem::take(&mut self.learn.done);
        let mut changed = false;
        for (id, text) in done {
            if self.store.all.iter().any(|r| r.at == id) {
                self.store.set_checked(id, text);
                changed = true;
            }
        }
        // New rules take effect quietly; 词典 lists them (and the weaker ones waiting to be confirmed).
        if changed {
            self.relearn(p);
        }
        while self.learn.running.is_none() {
            let Some((id, raw, ep)) = self.learn.queue.pop_front() else {
                break;
            };
            if !self.store.all.iter().any(|r| r.at == id) {
                continue;
            }
            let plan = self.direct_plan(false, "", None, LEARN_TIMEOUT.as_millis() as u64, p);
            let Some(ep) = ep else {
                self.llm_seq += 1;
                self.learn.running = Some((self.llm_seq, id, raw.clone()));
                p.cloud_llm(self.llm_seq, cloud::Kind::Learn, raw, plan);
                continue;
            };
            match Self::byok_request(&ep, &plan, &plan.user_with(&raw), p) {
                Ok(req) => {
                    self.llm_seq += 1;
                    self.learn.running = Some((self.llm_seq, id, raw));
                    p.llm(self.llm_seq, req);
                }
                Err(e) => p.log(&format!("learn: {e}")),
            }
        }
    }

    /// Regenerates hotwords.auto.txt.
    fn relearn(&mut self, p: &mut impl Platform) {
        let terms = p.terms(false);
        let learn = crate::store::Learn::from_settings(|k| p.setting(k));
        self.store.write_auto_rules(&terms, &learn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session;
    use gesture::Action::*;

    /// Records effects; messages "posted" by the platform are queued for the test to deliver.
    #[derive(Default)]
    struct Fake {
        out: Vec<String>,
        queued: Vec<Msg>,
        key: bool,
        mic_fails: bool,
        target_ok: bool,
        settings: Vec<(&'static str, &'static str)>,
        actions: Vec<Action>,
        terms: Vec<String>,
        llm_key: bool,
        warmed: Vec<String>,
        /// Commands sent to the latest session.
        cmds: Option<async_channel::Receiver<session::Cmd>>,
        /// Bodies of LLM requests.
        bodies: Vec<Vec<u8>>,
    }

    impl Platform for Fake {
        fn hud(&mut self, h: Hud) {
            if !matches!(h, Hud::Level(_) | Hud::Unquiet) {
                self.out.push(format!("hud {h:?}"));
            }
        }
        fn log(&mut self, _: &str) {}
        fn timer(&mut self, _: Duration, msg: Msg) {
            self.out.push(format!("timer {msg:?}"));
        }
        fn setting(&mut self, key: &str) -> Option<String> {
            self.settings
                .iter()
                .find(|s| s.0 == key)
                .map(|s| s.1.to_string())
        }
        fn open_session(&mut self, g: u64, cloud: bool) -> Result<Session, String> {
            if !self.key {
                return Err("未设置火山引擎 API Key".into());
            }
            self.out
                .push(format!("session {g}{}", if cloud { " cloud" } else { "" }));
            let (s, rx) = Session::channel();
            self.cmds = Some(rx);
            Ok(s)
        }
        fn front_app(&mut self, _: u64) -> (Option<String>, Option<String>) {
            (Some("Notes".into()), Some("com.apple.Notes".into()))
        }
        fn read_field(&mut self, g: u64) {
            self.out.push(format!("field {g}"));
        }
        fn start_capture(&mut self, g: u64) -> Result<String, String> {
            if self.mic_fails {
                return Err("no input device".into());
            }
            self.out.push(format!("capture {g}"));
            Ok("Mic".into())
        }
        fn stop_capture(&mut self, g: u64) {
            self.out.push(format!("stop {g}"));
            self.queued.push(Msg::CaptureEnded { generation: g });
        }
        fn verify_target(&mut self, g: u64, id: DateTime<Utc>, text: String) {
            self.out.push(format!("verify {g} {text}"));
            self.queued.push(Msg::Verified {
                generation: g,
                id,
                text,
                ok: self.target_ok,
            });
        }
        fn paste(&mut self, _: u64, text: &str) -> bool {
            self.out.push(format!("paste {text}"));
            true
        }
        fn actions(&mut self) -> (Vec<Action>, String) {
            (self.actions.clone(), "Plaid".into())
        }
        fn terms(&mut self, _: bool) -> Vec<String> {
            self.terms.clone()
        }
        fn llm_key(&mut self, _: &str) -> Option<String> {
            self.llm_key.then(|| "k".into())
        }
        fn llm(&mut self, id: u64, req: llm::Request) {
            self.out.push(format!("llm {id}"));
            self.bodies.push(req.body);
        }
        fn cloud_llm(&mut self, id: u64, kind: cloud::Kind, text: String, _: Plan) {
            self.out.push(format!("cloud {} {id} {text}", kind.key()));
        }
        fn warm(&mut self, base: &str) {
            self.warmed.push(base.into());
        }
        fn watch_edits(&mut self, g: u64, _: DateTime<Utc>, text: &str) {
            self.out.push(format!("watch {g} {text}"));
        }
        fn check_edits(&mut self) {
            self.out.push("check".into());
        }
        fn run(&mut self, g: u64, a: &Action, args: &str, env: actions::Env) {
            let var = |k| {
                env.iter()
                    .find(|e| e.0 == k)
                    .map(|e| e.1.clone())
                    .unwrap_or_default()
            };
            let (before, bundle) = (var("PLAID_BEFORE"), var("PLAID_BUNDLE"));
            self.out.push(format!(
                "run {g} {} [{args}] before={before} bundle={bundle}",
                a.name
            ));
        }
        fn recover(&mut self, id: DateTime<Utc>, audio: PathBuf, timeout: Duration) {
            let name = audio.file_name().unwrap().to_string_lossy().into_owned();
            self.out.push(format!(
                "recover {} {name} {}s",
                id.timestamp(),
                timeout.as_secs()
            ));
        }
    }

    struct T {
        c: Controller,
        p: Fake,
        dir: std::path::PathBuf,
    }

    impl T {
        fn new() -> T {
            let dir = std::env::temp_dir().join(format!("plaid-ctl-{}", uuid::Uuid::new_v4()));
            T {
                c: Controller::new(Store::load(&dir)),
                p: Fake {
                    key: true,
                    target_ok: true,
                    ..Default::default()
                },
                dir,
            }
        }
        fn send(&mut self, m: Msg) -> Vec<String> {
            self.c.handle(m, &mut self.p);
            std::mem::take(&mut self.p.out)
        }
        /// Deliver everything the platform queued (capture ended, verification).
        fn flush(&mut self) -> Vec<String> {
            let mut out = vec![];
            while !self.p.queued.is_empty() {
                for m in std::mem::take(&mut self.p.queued) {
                    out.extend(self.send(m));
                }
            }
            out
        }
        fn key(&mut self, a: gesture::Action) -> Vec<String> {
            self.send(Msg::Key {
                action: a,
                trigger_ms: Some(5),
            })
        }
        fn loud(&mut self, g: u64, n: usize) {
            let pcm: Vec<u8> = (0..1600)
                .flat_map(|i| (if i % 2 == 0 { 3000i16 } else { -3000 }).to_le_bytes())
                .collect();
            for _ in 0..n {
                self.send(Msg::Audio {
                    generation: g,
                    pcm: pcm.clone(),
                });
            }
        }
        fn quiet(&mut self, g: u64) -> Vec<String> {
            self.send(Msg::Audio {
                generation: g,
                pcm: vec![0; 3200],
            })
        }
        fn done(&mut self, g: u64, r: Result<&str, &str>) -> Vec<String> {
            self.send(Msg::Session {
                generation: g,
                event: Event::Done(r.map(String::from).map_err(String::from)),
            })
        }
    }

    impl Drop for T {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn has(v: &[String], s: &str) -> bool {
        v.iter().any(|x| x.contains(s))
    }

    #[test]
    fn hold_dictation_pastes_and_records() {
        let mut t = T::new();
        let out = t.key(StartHold);
        assert!(
            has(&out, "session 1")
                && has(&out, "hud Listen")
                && has(&out, "field 1")
                && has(&out, "capture 1"),
            "{out:?}"
        );
        t.send(Msg::Field {
            generation: 1,
            before: Some("前文".into()),
            window: None,
        });
        t.loud(1, 5);
        t.send(Msg::Session {
            generation: 1,
            event: Event::Text("你好".into()),
        });
        let out = t.key(Stop);
        assert!(has(&out, "hud Finishing") && has(&out, "Tail"), "{out:?}");
        assert!(t.c.keys().escape && !t.c.keys().enter);
        // The first quiet buffer in the tail closes the mic; finish waits for CaptureEnded.
        let out = t.quiet(1);
        assert_eq!(out, vec!["stop 1"]);
        t.flush();
        let out = t.done(1, Ok("你好。"));
        assert!(has(&out, "verify 1 你好。"), "{out:?}");
        let out = t.flush();
        assert!(
            has(&out, "paste 你好。") && has(&out, "hud Done(\"你好。\")"),
            "{out:?}"
        );
        let r = t.c.store.all.last().unwrap();
        assert_eq!(
            (r.outcome.as_deref(), r.text.as_str(), r.app.as_deref()),
            (None, "你好。", Some("Notes"))
        );
        assert_eq!(
            r.ctx.as_deref(),
            Some(&["前文".to_string(), "当前应用：Notes".into()][..])
        );
        assert_eq!(r.trigger_ms, Some(5));
        assert!(r.audio.is_some() && r.speech_rms.unwrap() >= 2999);
        assert!(t.c.idle());
    }

    #[test]
    fn toggle_then_enter() {
        let mut t = T::new();
        let out = t.key(StartToggle);
        assert!(has(&out, "hud Continuous"), "{out:?}");
        assert!(t.c.keys().toggled && t.c.keys().enter);
        let out = t.send(Msg::Return);
        assert!(has(&out, "hud Finishing"), "{out:?}");
        // Tail cap expires without a quiet buffer.
        let out = t.send(Msg::Timer {
            generation: 1,
            timer: Timer::Tail,
        });
        assert_eq!(out, vec!["stop 1"]);
    }

    #[test]
    fn escape_and_chord_cancel_drop_everything() {
        for how in [0, 1] {
            let mut t = T::new();
            t.key(StartHold);
            t.loud(1, 3);
            let out = if how == 0 {
                t.send(Msg::Escape)
            } else {
                t.key(Cancel)
            };
            assert_eq!(out, vec!["stop 1", "hud Hide"]);
            // The capture's end, late audio and the cancelled session's Done are all stale.
            assert!(t.flush().is_empty());
            assert!(t.done(1, Err("cancelled")).is_empty());
            assert!(t.c.store.all.is_empty());
        }
        // esc while finalizing.
        let mut t = T::new();
        t.key(StartHold);
        t.key(Stop);
        t.send(Msg::Timer {
            generation: 1,
            timer: Timer::Tail,
        });
        t.flush();
        assert_eq!(t.send(Msg::Escape), vec!["hud Hide"]);
        assert!(t.done(1, Ok("迟到的结果")).is_empty());
    }

    #[test]
    fn new_press_while_finalizing_preserves_the_old_one() {
        let mut t = T::new();
        t.key(StartHold);
        t.send(Msg::Session {
            generation: 1,
            event: Event::Text("半句".into()),
        });
        t.key(Stop);
        t.send(Msg::Timer {
            generation: 1,
            timer: Timer::Tail,
        });
        t.flush();
        let out = t.key(StartHold);
        assert!(has(&out, "session 2"), "{out:?}");
        let r = t.c.store.all.last().unwrap();
        assert_eq!(
            (r.outcome.as_deref(), r.error.as_deref(), r.raw.as_str()),
            (Some("failed"), Some("被新的听写中断"), "半句")
        );
        assert!(t.done(1, Ok("旧的")).is_empty());
        assert!(!t.c.idle());
    }

    #[test]
    fn empty_result_and_errors() {
        // Loud audio, no text: preserved for retry.
        let mut t = T::new();
        t.key(StartHold);
        t.loud(1, 6);
        let out = t.done(1, Ok(""));
        assert!(
            has(&out, "stop 1") && has(&out, "未识别到文字，可在历史里重试"),
            "{out:?}"
        );
        assert_eq!(
            t.c.store.all.last().unwrap().error.as_deref(),
            Some("未识别到文字")
        );
        // Short silent press: just hide.
        let mut t = T::new();
        t.key(StartHold);
        t.quiet(1);
        assert!(has(&t.done(1, Ok("")), "hud Hide"));
        assert!(t.c.store.all.is_empty());
        // Server error keeps the partial.
        let mut t = T::new();
        t.key(StartHold);
        t.send(Msg::Session {
            generation: 1,
            event: Event::Text("部分".into()),
        });
        let out = t.done(1, Err("HTTP 401"));
        assert!(has(&out, "HTTP 401；可在历史里查看"), "{out:?}");
        assert_eq!(t.c.store.all.last().unwrap().raw, "部分");
    }

    #[test]
    fn no_key_and_mic_failure() {
        let mut t = T::new();
        t.p.key = false;
        let out = t.key(StartHold);
        assert_eq!(out, vec!["hud Failed(\"未设置火山引擎 API Key\")"]);
        assert!(t.c.idle());
        let mut t = T::new();
        t.p.mic_fails = true;
        let out = t.key(StartHold);
        assert!(has(&out, "麦克风：no input device"), "{out:?}");
        assert!(t.c.idle());
        assert_eq!(
            t.c.store.all.last().unwrap().outcome.as_deref(),
            Some("failed")
        );
    }

    #[test]
    fn target_moved_saves_without_paste() {
        let mut t = T::new();
        t.p.target_ok = false;
        t.key(StartHold);
        t.done(1, Ok("文本"));
        let out = t.flush();
        assert!(!has(&out, "paste") && has(&out, "hud Saved"), "{out:?}");
        let r = t.c.store.all.last().unwrap();
        assert_eq!(r.outcome.as_deref(), Some("saved"));
    }

    #[test]
    fn unknown_command_flashes() {
        let mut t = T::new();
        t.p.actions = vec![serde_json::from_str(r#"{"name":"搜索","run":"open x"}"#).unwrap()];
        t.key(StartHold);
        let out = t.done(1, Ok("Plaid，飞起来"));
        assert!(has(&out, "没有匹配的命令：飞起来"), "{out:?}");
        assert!(t.c.store.all.is_empty());
        let a = &t.c.store.attempts[0];
        assert_eq!(
            (a.heard.as_str(), a.result.as_str(), a.action.as_ref()),
            ("Plaid，飞起来", "nomatch", None)
        );
        // It survives a reload; 清空历史 removes it.
        assert_eq!(Store::load(t.c.store.dir()).attempts.len(), 1);
        t.c.store.clear();
        assert!(Store::load(t.c.store.dir()).attempts.is_empty());
    }

    #[test]
    fn context_timer_begins_once() {
        let mut t = T::new();
        t.key(StartHold);
        t.send(Msg::Timer {
            generation: 1,
            timer: Timer::Context,
        });
        t.send(Msg::Field {
            generation: 1,
            before: Some("late".into()),
            window: None,
        });
        t.done(1, Ok("x"));
        assert_eq!(
            t.c.store.all.last().unwrap().ctx.as_deref(),
            Some(&["当前应用：Notes".to_string()][..])
        );
    }

    fn llm(t: &mut T, id: u64, r: Result<&str, &str>) -> Vec<String> {
        let result = r.map(String::from).map_err(|m| llm::Failure {
            message: m.into(),
            timed_out: m == "timeout",
        });
        t.send(Msg::Llm { id, result })
    }

    fn refining(t: &mut T, g: u64, raw: &str) -> Vec<String> {
        t.p.settings.extend([
            ("llmEnabled", "1"),
            ("llmBaseUrl", "http://127.0.0.1:1/v1"),
            ("llmModel", "m"),
        ]);
        t.key(StartHold);
        t.send(Msg::Field {
            generation: g,
            before: None,
            window: Some("Inbox".into()),
        });
        t.done(g, Ok(raw))
    }

    #[test]
    fn rerefine_rewrites_the_record_without_pasting() {
        let mut t = T::new();
        refining(&mut t, 1, "嗯你好，我们今天下午三点在公司楼下见面吧好吗");
        llm(&mut t, 1, Err("timeout"));
        t.flush();
        let id = t.c.store.all.last().unwrap().at;
        let out = t.send(Msg::Refine { id });
        assert!(has(&out, "llm 2") && !has(&out, "verify"), "{out:?}");
        assert!(has(&t.send(Msg::Refine { id }), "上一条还在润色"));
        let out = llm(&mut t, 2, Ok("你好，我们今天下午三点在公司楼下见面吧。"));
        assert!(has(&out, "已重新润色") && !has(&out, "verify"), "{out:?}");
        let r = t.c.store.all.last().unwrap();
        assert_eq!(
            (r.text.as_str(), r.llm.as_str()),
            ("你好，我们今天下午三点在公司楼下见面吧。", "ok")
        );
        let reloaded = crate::store::Store::load(t.c.store.dir());
        assert_eq!(reloaded.all.last().unwrap().llm, "ok");
        // The same text again still rewrites the record, so 历史 sees the run end.
        t.c.store.update(id, |r| r.llm_ms = None);
        t.send(Msg::Refine { id });
        llm(&mut t, 3, Ok("你好，我们今天下午三点在公司楼下见面吧。"));
        assert!(t.c.store.all.last().unwrap().llm_ms.is_some());
    }

    #[test]
    fn short_results_skip_refine() {
        let mut t = T::new();
        let out = refining(&mut t, 1, "先就这样吧。");
        assert!(
            has(&out, "verify 1 先就这样吧。") && !has(&out, "llm 1"),
            "{out:?}"
        );
        t.flush();
        assert_eq!(t.c.store.all.last().unwrap().llm, "off");
        // llmMinChars 0: every result goes through the LLM.
        let mut t = T::new();
        t.p.settings.push(("llmMinChars", "0"));
        let out = refining(&mut t, 1, "先就这样吧。");
        assert!(has(&out, "llm 1"), "{out:?}");
    }

    #[test]
    fn key_down_warms_the_refine_endpoint_only_when_it_will_be_used() {
        let warmed = |settings: &[(&'static str, &'static str)]| {
            let mut t = T::new();
            t.p.settings.extend_from_slice(settings);
            t.key(StartHold);
            t.p.warmed.clone()
        };
        let remote = ("llmBaseUrl", "https://api.example.com/v1");
        assert_eq!(
            warmed(&[("llmEnabled", "1"), remote]),
            ["https://api.example.com/v1"]
        );
        assert!(warmed(&[remote]).is_empty(), "LLM off");
        assert!(
            warmed(&[
                ("llmEnabled", "1"),
                ("llmBaseUrl", "http://localhost:11434/v1")
            ])
            .is_empty(),
            "loopback has no handshake to save"
        );
    }

    #[test]
    fn local_rules_run_before_the_llm_gate_and_on_its_output() {
        // Short result: rules alone, original ASR kept for History.
        let mut t = T::new();
        t.p.settings
            .extend([("ruleFillers", "1"), ("ruleNoPeriod", "1")]);
        let out = refining(&mut t, 1, "嗯，先就这样吧。");
        assert!(
            has(&out, "verify 1 先就这样吧") && !has(&out, "llm 1"),
            "{out:?}"
        );
        t.flush();
        let r = t.c.store.all.last().unwrap();
        assert_eq!(
            (r.asr.as_deref(), r.raw.as_str(), r.text.as_str()),
            (Some("嗯，先就这样吧。"), "先就这样吧", "先就这样吧")
        );
        // Long result: the LLM sees the ruled text; its output gets the format rules.
        let mut t = T::new();
        t.p.settings
            .extend([("ruleFillers", "1"), ("ruleNoPeriod", "1")]);
        let out = refining(
            &mut t,
            1,
            "嗯，我们今天下午三点在公司楼下见面吧，记得带电脑。",
        );
        assert!(
            has(
                &out,
                "hud Refining(\"我们今天下午三点在公司楼下见面吧，记得带电脑\")"
            ),
            "{out:?}"
        );
        let out = llm(&mut t, 1, Ok("今天下午三点公司楼下见，记得带电脑。"));
        assert!(
            has(&out, "verify 1 今天下午三点公司楼下见，记得带电脑"),
            "{out:?}"
        );
        // Rules off: nothing recorded twice.
        let mut t = T::new();
        refining(&mut t, 1, "嗯，先就这样吧。");
        t.flush();
        assert_eq!(t.c.store.all.last().unwrap().asr, None);
    }

    #[test]
    fn refine_pastes_the_rewrite_or_fails_open() {
        let mut t = T::new();
        let out = refining(&mut t, 1, "嗯你好，我们今天下午三点在公司楼下见面吧好吗");
        assert!(
            has(&out, "llm 1")
                && has(
                    &out,
                    "hud Refining(\"嗯你好，我们今天下午三点在公司楼下见面吧好吗\")"
                )
                && !has(&out, "verify"),
            "{out:?}"
        );
        assert!(t.c.keys().escape && !t.c.idle());
        let out = llm(&mut t, 1, Ok("你好。"));
        assert!(has(&out, "verify 1 你好。"), "{out:?}");
        t.flush();
        let r = t.c.store.all.last().unwrap();
        assert_eq!(
            (
                r.raw.as_str(),
                r.text.as_str(),
                r.llm.as_str(),
                r.outcome.as_deref()
            ),
            (
                "嗯你好，我们今天下午三点在公司楼下见面吧好吗",
                "你好。",
                "ok",
                None
            )
        );
        assert!(r.llm_ms.is_some());

        for (err, llm_field) in [("HTTP 500: x", "HTTP 500: x"), ("timeout", "timeout")] {
            let mut t = T::new();
            refining(&mut t, 1, "这是一段识别出来的原文，长度已经超过二十个字");
            let out = llm(&mut t, 1, Err(err));
            assert!(
                has(
                    &out,
                    "verify 1 这是一段识别出来的原文，长度已经超过二十个字"
                ),
                "{out:?}"
            );
            t.flush();
            assert_eq!(t.c.store.all.last().unwrap().llm, llm_field);
        }
    }

    #[test]
    fn refine_interrupted() {
        // esc: paste the raw text now; the late reply is ignored.
        let mut t = T::new();
        refining(&mut t, 1, "这是一段识别出来的原文，长度已经超过二十个字");
        let out = t.send(Msg::Escape);
        assert!(
            has(
                &out,
                "verify 1 这是一段识别出来的原文，长度已经超过二十个字"
            ),
            "{out:?}"
        );
        let out = t.flush();
        assert!(
            has(&out, "paste 这是一段识别出来的原文，长度已经超过二十个字"),
            "{out:?}"
        );
        assert_eq!(t.c.store.all.last().unwrap().llm, "skipped");
        assert!(llm(&mut t, 1, Ok("改写")).is_empty());
        // New press: the raw text is only saved.
        let mut t = T::new();
        refining(&mut t, 1, "这是一段识别出来的原文，长度已经超过二十个字");
        let out = t.key(StartHold);
        assert!(has(&out, "session 2") && !has(&out, "verify"), "{out:?}");
        let r = t.c.store.all.last().unwrap();
        assert_eq!(
            (r.llm.as_str(), r.outcome.as_deref()),
            ("skipped", Some("saved"))
        );
        // Chord cancel: dropped.
        let mut t = T::new();
        refining(&mut t, 1, "这是一段识别出来的原文，长度已经超过二十个字");
        assert_eq!(t.key(Cancel), vec!["hud Hide"]);
        assert!(llm(&mut t, 1, Ok("改写")).is_empty());
        assert!(t.c.store.all.is_empty());
    }

    #[test]
    fn learner_checks_pasted_text_quietly() {
        let mut t = T::new();
        t.p.llm_key = true;
        t.p.settings = vec![("llmBaseUrl", "https://api.deepseek.com")];
        t.p.terms = vec!["Claude".into()];
        for g in 1..=2 {
            let out = t.key(StartHold);
            assert!(has(&out, "check"), "{out:?}");
            t.done(g, Ok("问一下克劳德吧"));
            let out = t.flush();
            assert!(has(&out, "watch") && has(&out, "llm"), "{out:?}");
            assert!(t.c.learning());
            // The DeepSeek fallback is used: no refine endpoint configured.
            let out = llm(&mut t, g, Ok("问一下 Claude 吧"));
            assert!(
                !has(&out, "timer") && !has(&out, "hud"),
                "no announcement: {out:?}"
            );
            assert!(!t.c.learning());
        }
        assert_eq!(
            t.c.store.all.last().unwrap().checked.as_deref(),
            Some("问一下 Claude 吧")
        );
        // Two background checks on one day: a suggestion in 词典, not applied.
        let rules = t.c.store.auto_rules(&t.p.terms, &Default::default());
        assert_eq!(rules.len(), 1);
        assert!(!rules[0].active);
        let auto = std::fs::read_to_string(t.dir.join(crate::store::AUTO_RULES)).unwrap();
        assert!(!auto.contains("克劳德"), "{auto}");
    }

    #[test]
    fn learner_needs_a_key_or_a_local_server() {
        // Refine endpoint filled in, no key stored: no learning request.
        let mut t = T::new();
        t.p.settings = vec![("llmBaseUrl", "https://api.deepseek.com")];
        t.key(StartHold);
        t.done(1, Ok("问一下克劳德吧"));
        let out = t.flush();
        assert!(!has(&out, "llm"), "{out:?}");
        // A local server needs none.
        let mut t = T::new();
        t.p.settings = vec![("llmBaseUrl", "http://localhost:11434/v1")];
        t.key(StartHold);
        t.done(1, Ok("问一下克劳德吧"));
        let out = t.flush();
        assert!(has(&out, "llm"), "{out:?}");
    }

    #[test]
    fn learner_waits_while_dictating() {
        let mut t = T::new();
        t.p.llm_key = true;
        t.p.settings = vec![("llmBaseUrl", "https://api.deepseek.com")];
        t.key(StartHold);
        t.done(1, Ok("第一句"));
        t.flush();
        // A reply that arrives mid-utterance is held until idle; the record must still exist.
        t.key(StartHold);
        llm(&mut t, 1, Ok("第一句。"));
        assert_eq!(t.c.store.all.last().unwrap().checked, None);
        t.done(2, Ok("abc"));
        t.flush();
        assert_eq!(t.c.store.all[0].checked.as_deref(), Some("第一句。"));
        // Non-CJK text is not sent to the learner.
        assert!(!t.c.learning());
    }

    #[test]
    fn manual_edits_make_rules_and_switches_turn_learning_off() {
        let mut t = T::new();
        t.p.terms = vec!["Claude".into()];
        for _ in 0..2 {
            t.key(StartHold);
            let g = t.c.generation;
            t.done(g, Ok("问一下克劳德吧"));
            let out = t.flush();
            assert!(
                has(&out, "watch") && !has(&out, "llm"),
                "no learning endpoint: {out:?}"
            );
            let id = t.c.store.all.last().unwrap().at;
            t.send(Msg::Edited {
                id,
                pasted: "问一下克劳德吧".into(),
                edited: "问一下 Claude 吧".into(),
            });
        }
        assert_eq!(
            t.c.store.all.last().unwrap().edited.as_deref(),
            Some("问一下 Claude 吧")
        );
        let auto = std::fs::read_to_string(t.dir.join(crate::store::AUTO_RULES)).unwrap();
        assert!(auto.contains("克劳德 -> Claude"), "{auto}");

        let mut t = T::new();
        t.p.llm_key = true;
        t.p.settings = vec![
            ("llmBaseUrl", "https://api.deepseek.com"),
            ("learnEdits", "0"),
            ("autoLearn", "0"),
        ];
        t.key(StartHold);
        t.done(1, Ok("问一下克劳德吧"));
        let out = t.flush();
        assert!(!has(&out, "watch") && !has(&out, "llm"), "{out:?}");
        assert!(!t.c.learning());
    }

    fn ran(
        t: &mut T,
        g: u64,
        action: &str,
        code: Option<i32>,
        out: &str,
        err: &str,
    ) -> Vec<String> {
        let action =
            t.p.actions
                .iter()
                .find(|a| a.name == action)
                .unwrap()
                .clone();
        let outcome = actions::Outcome {
            code,
            out: out.into(),
            err: err.into(),
            ..Default::default()
        };
        t.send(Msg::Ran {
            generation: g,
            action,
            outcome,
        })
    }

    fn with_actions(t: &mut T) {
        t.p.actions = serde_json::from_str(
            r#"[{"name":"日期","run":"date","output":"paste"},{"name":"天气","run":"w"},
                {"name":"删除","run":"rm x","output":"none","confirm":true}]"#,
        )
        .unwrap();
    }

    #[test]
    fn command_runs_and_pastes_its_output_without_a_record() {
        let mut t = T::new();
        with_actions(&mut t);
        t.key(StartHold);
        let long = "字".repeat(400);
        t.send(Msg::Field {
            generation: 1,
            before: Some(long.clone()),
            window: None,
        });
        let out = t.done(1, Ok("Plaid，日期，今天"));
        assert!(
            has(&out, "hud Running(\"日期：今天\")")
                && has(
                    &out,
                    &format!("run 1 日期 [今天] before={long} bundle=com.apple.Notes")
                ),
            "{out:?}"
        );
        assert!(!has(&out, "verify"), "{out:?}");
        assert_eq!(t.c.store.attempts[0].result, "running");
        let out = ran(&mut t, 1, "日期", Some(0), "2026-11-18\n", "");
        assert!(has(&out, "verify 1 2026-11-18"), "{out:?}");
        let a = &t.c.store.attempts[0];
        assert_eq!(
            (a.action.as_deref(), a.args.as_str(), a.result.as_str()),
            (Some("日期"), "今天", "ok")
        );
        assert_eq!((a.code, a.output.as_deref()), (Some(0), Some("2026-11-18")));
        let out = t.flush();
        assert!(
            has(&out, "paste 2026-11-18")
                && has(&out, "hud Done(\"2026-11-18\")")
                && !has(&out, "watch"),
            "{out:?}"
        );
        assert!(t.c.store.all.is_empty());
        // The ASR got only the last 300 units of the field text.
        // (Session::begin is not observable in the fake; tail() is checked directly.)
        assert_eq!(tail(&long, ASR_BEFORE).chars().count(), 300);

        // HUD output, failure, and a result from a superseded utterance.
        let out = ran(&mut t, 1, "天气", Some(0), "a\n晴 12°\n", "");
        assert_eq!(out, vec!["hud Note(\"晴 12°\")"]);
        let out = ran(&mut t, 1, "天气", Some(1), "", "warn\ncurl: (6) no host\n");
        assert_eq!(out, vec!["hud Failed(\"天气 失败：curl: (6) no host\")"]);
        t.key(StartHold);
        t.done(2, Ok("Plaid 天气"));
        ran(&mut t, 2, "天气", Some(6), "", "warn\ncurl: (6) no host\n");
        let a = t.c.store.attempts.last().unwrap();
        assert_eq!(
            (a.result.as_str(), a.code, a.output.as_deref()),
            ("failed", Some(6), Some("warn\ncurl: (6) no host"))
        );
        t.key(StartHold);
        assert!(ran(&mut t, 1, "日期", Some(0), "x", "").is_empty());
    }

    #[test]
    fn command_confirm() {
        let mut t = T::new();
        with_actions(&mut t);
        t.key(StartHold);
        let out = t.done(1, Ok("Plaid 删除"));
        assert!(
            has(&out, "hud Ask(\"删除\")") && has(&out, "Confirm") && !has(&out, "run"),
            "{out:?}"
        );
        assert!(t.c.idle() && t.c.keys().enter && t.c.keys().escape);
        let out = t.send(Msg::Return);
        assert!(has(&out, "run 1 删除 []"), "{out:?}");
        assert!(!t.c.keys().enter);
        assert_eq!(
            ran(&mut t, 1, "删除", Some(0), "", ""),
            vec!["hud Done(\"删除\")"]
        );

        // esc drops it; so does the timer.
        t.key(StartHold);
        t.done(2, Ok("Plaid 删除"));
        assert_eq!(t.send(Msg::Escape), vec!["hud Hide"]);
        assert!(t.send(Msg::Return).is_empty());
        t.key(StartHold);
        t.done(3, Ok("Plaid 删除"));
        t.send(Msg::Timer {
            generation: 3,
            timer: Timer::Confirm,
        });
        assert!(t.send(Msg::Return).is_empty() && !t.c.keys().enter);
        // A new press drops it too.
        t.key(StartHold);
        t.done(4, Ok("Plaid 删除"));
        t.key(StartHold);
        assert!(t.c.confirm.is_none());
        let results: Vec<&str> =
            t.c.store
                .attempts
                .iter()
                .map(|a| a.result.as_str())
                .collect();
        assert_eq!(results, ["ok", "cancelled", "cancelled", "cancelled"]);
        // One left waiting when the app quit reads as cancelled next launch.
        t.done(5, Ok("Plaid 删除"));
        let c = Controller::new(Store::load(t.c.store.dir()));
        assert_eq!(c.store.attempts.last().unwrap().result, "cancelled");
    }

    #[test]
    fn recover_saves_in_place_and_blocks_dictation() {
        let mut t = T::new();
        t.key(gesture::Action::StartHold);
        t.loud(1, 30);
        t.key(gesture::Action::Stop);
        t.quiet(1);
        t.flush();
        let out = t.done(1, Err("连接断开"));
        assert!(has(&out, "Failed"), "{out:?}");
        let id = t.c.store.all.last().unwrap().at;
        assert!(t.c.store.all.last().unwrap().is_failure());
        let usage_before: i64 = t.c.store.usage.values().map(|u| u.n).sum();

        let out = t.send(Msg::Recover { id });
        assert!(
            has(&out, "Running")
                && has(&out, &format!("recover {} ", id.timestamp()))
                && has(&out, " 30s"),
            "{out:?}"
        );
        assert!(has(
            &t.key(gesture::Action::StartHold),
            "正在重新识别历史录音"
        ));
        assert!(t.c.idle());
        assert!(has(
            &t.send(Msg::Recover { id }),
            "请等当前听写或重试结束后再试"
        ));

        // A stale reply is ignored; empty text keeps the record as it was.
        assert!(
            t.send(Msg::Recovered {
                id: id + chrono::Duration::seconds(1),
                result: Ok("x".into())
            })
            .is_empty()
        );
        assert!(has(
            &t.send(Msg::Recovered {
                id,
                result: Ok(" ".into())
            }),
            "没有识别到文字"
        ));
        assert!(t.c.store.all.last().unwrap().is_failure());

        t.send(Msg::Recover { id });
        let out = t.send(Msg::Recovered {
            id,
            result: Ok("找回来了".into()),
        });
        assert!(has(&out, "Note(\"识别结果已保存在原记录中\")"), "{out:?}");
        let r = t.c.store.all.last().unwrap();
        assert_eq!(
            (
                r.raw.as_str(),
                r.text.as_str(),
                r.outcome.as_deref(),
                r.error.as_deref()
            ),
            ("找回来了", "找回来了", Some("saved"), None)
        );
        assert_eq!(
            t.c.store.usage.values().map(|u| u.n).sum::<i64>(),
            usage_before + 1
        );
        assert!(has(&t.key(gesture::Action::StartHold), "Listen"));
    }

    #[test]
    fn recover_needs_audio() {
        let mut t = T::new();
        t.p.settings.push(("keepAudio", "0"));
        t.key(gesture::Action::StartHold);
        t.loud(1, 30);
        t.key(gesture::Action::Stop);
        t.quiet(1);
        t.flush();
        t.done(1, Err("x"));
        let id = t.c.store.all.last().unwrap().at;
        assert!(has(&t.send(Msg::Recover { id }), "没有保留录音"));
        assert!(t.c.recovering().is_none());
    }

    #[test]
    fn clear_waits_for_idle_and_keeps_usage() {
        let mut t = T::new();
        t.key(gesture::Action::StartHold);
        t.loud(1, 30);
        assert!(has(&t.send(Msg::Clear), "请等当前听写或重试结束后再试"));
        t.key(gesture::Action::Stop);
        t.quiet(1);
        t.flush();
        t.done(1, Ok("你好"));
        let usage: i64 = t.c.store.usage.values().map(|u| u.n).sum();
        assert!(!t.c.store.all.is_empty() && usage > 0);
        t.send(Msg::Clear);
        assert!(t.c.store.all.is_empty());
        assert!(!t.c.store.dir().join("records.jsonl").exists());
        assert_eq!(t.c.store.usage.values().map(|u| u.n).sum::<i64>(), usage);
    }

    #[test]
    fn delete_removes_records_and_audio_but_keeps_usage() {
        let mut t = T::new();
        for (g, text) in [(1, "一"), (2, "二")] {
            t.key(gesture::Action::StartHold);
            t.loud(g, 30);
            t.key(gesture::Action::Stop);
            t.quiet(g);
            t.flush();
            t.done(g, Ok(text));
        }
        let first = t.c.store.all[0].clone();
        let audio = first.audio.clone().map(|a| t.c.store.audio_dir().join(a));
        let usage: i64 = t.c.store.usage.values().map(|u| u.n).sum();
        t.send(Msg::Delete {
            ids: vec![first.at],
        });
        assert_eq!(t.c.store.all.len(), 1);
        assert_eq!(t.c.store.all[0].text, "二");
        assert!(audio.is_none_or(|a| !a.exists()));
        let reloaded = crate::store::Store::load(t.c.store.dir());
        assert_eq!(reloaded.all.len(), 1);
        assert_eq!(t.c.store.usage.values().map(|u| u.n).sum::<i64>(), usage);
    }

    const LONG: &str = "今天下午三点我们在会议室讨论一下下个季度的计划";

    /// A Cloud dictation up to the final; returns the plan its `begin` carried.
    fn cloud_final(t: &mut T, raw: &str) -> (Option<Plan>, Vec<String>) {
        t.p.settings
            .extend([("serviceMode", "cloud"), ("llmEnabled", "1")]);
        let out = t.key(StartHold);
        assert!(
            has(&out, "session 1 cloud") && t.p.warmed.is_empty(),
            "{out:?}"
        );
        t.send(Msg::Field {
            generation: 1,
            before: None,
            window: Some("Inbox".into()),
        });
        let plan = match t.p.cmds.as_ref().unwrap().try_recv() {
            Ok(session::Cmd::Begin(_, plan)) => plan.map(|p| *p),
            _ => panic!("no begin"),
        };
        (plan, t.done(1, Ok(raw)))
    }

    fn cloud_refined(t: &mut T, r: Result<&str, llm::Failure>) -> Vec<String> {
        t.send(Msg::Session {
            generation: 1,
            event: Event::Refined(r.map(String::from)),
        })
    }

    #[test]
    fn cloud_begin_carries_the_plan_and_the_server_refines() {
        let mut t = T::new();
        let (plan, out) = cloud_final(&mut t, LONG);
        let plan = plan.expect("plan");
        assert_eq!(plan.level, Level::Fluent);
        assert!(
            plan.user.contains("当前应用：Notes — Inbox"),
            "{}",
            plan.user
        );
        assert!(
            has(&out, "hud Refining") && has(&out, "Refine") && !has(&out, "llm "),
            "{out:?}"
        );
        let out = cloud_refined(&mut t, Ok("润色后"));
        assert!(has(&out, "verify 1 润色后"), "{out:?}");
        t.flush();
        let r = t.c.store.all.last().unwrap();
        assert_eq!((r.text.as_str(), r.llm.as_str()), ("润色后", "ok"));
        assert!(r.is_cloud() && r.llm_ms.is_none(), "{r:?}");
        // The usage line after `done` comes when the session is gone and is only logged.
        let out = t.send(Msg::Session {
            generation: 1,
            event: Event::Usage(cloud::Usage::default()),
        });
        assert!(out.is_empty());
    }

    #[test]
    fn cloud_refine_fails_open() {
        // The server never answers: the deadline pastes the recognized text.
        let mut t = T::new();
        cloud_final(&mut t, LONG);
        let out = t.send(Msg::Timer {
            generation: 1,
            timer: Timer::Refine,
        });
        assert!(has(&out, &format!("verify 1 {LONG}")), "{out:?}");
        t.flush();
        assert_eq!(t.c.store.all.last().unwrap().llm, "timeout");
        // A late answer is stale.
        assert!(cloud_refined(&mut t, Ok("迟到")).is_empty());

        // The socket drops mid-refine.
        let mut t = T::new();
        cloud_final(&mut t, LONG);
        let f = llm::Failure {
            message: "Cloud 连接已断开".into(),
            timed_out: false,
        };
        let out = cloud_refined(&mut t, Err(f));
        assert!(has(&out, &format!("verify 1 {LONG}")), "{out:?}");
    }

    #[test]
    fn esc_during_cloud_refine_pastes_raw_and_drops_the_session() {
        let mut t = T::new();
        cloud_final(&mut t, LONG);
        let cmds = t.p.cmds.clone().unwrap();
        assert!(!cmds.is_closed());
        let out = t.send(Msg::Escape);
        assert!(has(&out, &format!("verify 1 {LONG}")), "{out:?}");
        assert!(
            cmds.is_closed(),
            "the session must be dropped to cancel the server's LLM call"
        );
    }

    #[test]
    fn cloud_short_text_skips_the_wait() {
        let mut t = T::new();
        let (plan, out) = cloud_final(&mut t, "好的");
        assert!(plan.is_some());
        assert!(
            has(&out, "verify 1 好的") && !has(&out, "Refining"),
            "{out:?}"
        );
        assert!(t.p.cmds.as_ref().unwrap().is_closed());
    }

    /// BYOK sends exactly the prompt the Cloud plan carries.
    #[test]
    fn byok_and_cloud_send_the_same_prompt() {
        let mut byok = T::new();
        byok.p.llm_key = true;
        refining(&mut byok, 1, LONG);
        let body: serde_json::Value = serde_json::from_slice(&byok.p.bodies[0]).unwrap();
        let mut cloud = T::new();
        let plan = cloud_final(&mut cloud, LONG).0.unwrap();
        let msgs = &body["messages"];
        assert_eq!(msgs[0]["content"], plan.system.as_str());
        assert_eq!(msgs[1]["content"], plan.decide(LONG).llm.unwrap().as_str());
    }

    #[test]
    fn cloud_rerefine_and_learning_go_through_the_server() {
        let mut t = T::new();
        // Pasted text the LLM did not touch is learned from, through Cloud.
        cloud_final(&mut t, "好的我知道了");
        let out = t.flush();
        assert!(has(&out, "cloud learn"), "{out:?}");
        let id = t.c.store.all.last().unwrap().at;
        let out = t.send(Msg::Refine { id });
        assert!(has(&out, "cloud refine"), "{out:?}");
    }
}
