//! Platform side of the dictation controller. The controller runs on one thread (GPUI's main thread, or the
//! main thread of a headless replay); everything slow (ASR socket, AX reads, timers) runs elsewhere and comes
//! back through `tx` as a `Msg`. HUD changes are collected in `hud` for the caller to apply.
use crate::{config, secret, trace};
use async_channel::Sender;
use chrono::{DateTime, Utc};
use plaid_core::actions::Action;
use plaid_core::dictation::{Hud, Msg, Platform};
use plaid_core::session::{self, Session};
use plaid_core::{diff, gesture, llm};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::os;

/// AX calls run one at a time on a worker thread (they can block up to their messaging timeout).
type Job = Box<dyn FnOnce() + Send>;

fn ax_worker() -> std::sync::mpsc::Sender<Job> {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("ax".into())
        .spawn(move || {
            for job in rx {
                job();
            }
        })
        .expect("ax thread");
    tx
}

/// Paste target check: AX must answer within this, else the text is only saved.
const VERIFY_TIMEOUT: Duration = Duration::from_millis(300);
/// UTF-16 units of field text before the caret read at key-down (PLAID_BEFORE; the ASR gets the last 300).
const FIELD_CONTEXT: usize = 2000;
/// The pasted text has landed in the field by then.
const PASTE_SETTLE: Duration = Duration::from_millis(600);
/// A pasted span not followed by another utterance is compared after this.
const EDIT_WINDOW: Duration = Duration::from_secs(60);

/// A pasted span being watched for manual corrections (EditWatch). Lives on the AX thread side.
struct Watch {
    seq: u64,
    id: DateTime<Utc>,
    field: os::Field,
    /// Field value right after the paste, and the pasted span in it (UTF-16 offsets).
    base: String,
    start: usize,
    end: usize,
    pasted: String,
}

/// Compares and clears the watch. AX thread.
fn check(watch: &Mutex<Option<Watch>>, tx: &Sender<Msg>) {
    let Some(w) = watch.lock().unwrap().take() else {
        return;
    };
    let Some(now) = w.field.reread() else { return };
    if let Some(edited) = diff::user_edit(&w.base, w.start, w.end, &now, &w.pasted) {
        let _ = tx.try_send(Msg::Edited {
            id: w.id,
            pasted: w.pasted,
            edited,
        });
    }
}

#[allow(clippy::large_enum_variant)] // One per process.
enum Mode {
    /// Mic, focused app, real paste.
    Live {
        capture: Option<crate::audio::Capture>,
        front: Option<(u64, i32)>,
        target: Arc<Mutex<Option<(u64, os::Target)>>>,
        watch: Arc<Mutex<Option<Watch>>>,
        watch_seq: u64,
        ax: std::sync::mpsc::Sender<Job>,
        clipboard: std::rc::Rc<std::cell::RefCell<crate::paste::Clipboard>>,
        /// Clipboard restore timer.
        fg: gpui_kit::ForegroundExecutor,
        bg: gpui_kit::BackgroundExecutor,
    },
    /// A WAV fed in real time instead of the mic; the paste is printed.
    Replay {
        pcm: Arc<Vec<u8>>,
        stop: Option<Arc<AtomicBool>>,
    },
}

pub struct Driver {
    tx: Sender<Msg>,
    pub hud: Vec<Hud>,
    mode: Mode,
}

impl Driver {
    pub fn live(
        tx: Sender<Msg>,
        fg: gpui_kit::ForegroundExecutor,
        bg: gpui_kit::BackgroundExecutor,
    ) -> Driver {
        let mode = Mode::Live {
            capture: None,
            front: None,
            target: Default::default(),
            watch: Default::default(),
            watch_seq: 0,
            ax: ax_worker(),
            clipboard: Default::default(),
            fg,
            bg,
        };
        Driver {
            tx,
            hud: vec![],
            mode,
        }
    }

    /// Quitting inside the 0.5 s after a paste would leave our text on the clipboard; put the user's back.
    pub fn restore_clipboard_on_quit(&self, cx: &gpui_kit::App) {
        if let Mode::Live { clipboard, .. } = &self.mode {
            let clipboard = clipboard.clone();
            cx.on_app_quit(move |_| {
                clipboard.borrow_mut().restore(None);
                async {}
            })
            .detach();
        }
    }

    pub fn replay(tx: Sender<Msg>, pcm: Vec<u8>) -> Driver {
        Driver {
            tx,
            hud: vec![],
            mode: Mode::Replay {
                pcm: Arc::new(pcm),
                stop: None,
            },
        }
    }

    fn post(&self, m: Msg) {
        let _ = self.tx.try_send(m);
    }

    /// Queues an AX job after `after` without holding the AX thread.
    fn ax_later(ax: &std::sync::mpsc::Sender<Job>, after: Duration, job: Job) {
        let ax = ax.clone();
        std::thread::spawn(move || {
            std::thread::sleep(after);
            let _ = ax.send(job);
        });
    }
}

impl Platform for Driver {
    fn hud(&mut self, h: Hud) {
        self.hud.push(h);
    }

    fn log(&mut self, line: &str) {
        trace::log(line);
    }

    fn timer(&mut self, after: Duration, msg: Msg) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(after);
            let _ = tx.try_send(msg);
        });
    }

    fn setting(&mut self, key: &str) -> Option<String> {
        config::value(key)
    }

    fn open_session(&mut self, generation: u64) -> Result<Session, String> {
        let key = secret::api_key()?;
        let (s, events, fut) = session::start(config::session_options(key), config::hotwords());
        std::thread::spawn(move || async_io::block_on(fut));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            while let Ok(event) = events.recv_blocking() {
                let _ = tx.try_send(Msg::Session { generation, event });
            }
        });
        Ok(s)
    }

    fn front_app(&mut self, generation: u64) -> (Option<String>, Option<String>) {
        match &mut self.mode {
            Mode::Live { front, .. } => {
                let Some((name, bundle, pid)) = os::front() else {
                    return (None, None);
                };
                *front = Some((generation, pid));
                (Some(name).filter(|n| !n.is_empty()), bundle)
            }
            Mode::Replay { .. } => (None, None),
        }
    }

    fn read_field(&mut self, generation: u64) {
        let tx = self.tx.clone();
        match &self.mode {
            Mode::Live {
                front: Some((g, pid)),
                target,
                ax,
                ..
            } if *g == generation => {
                let (pid, target) = (*pid, target.clone());
                let _ = ax.send(Box::new(move || {
                    let (t, before) = os::focus(pid, FIELD_CONTEXT);
                    let window = t.title.clone();
                    *target.lock().unwrap() = Some((generation, t));
                    let _ = tx.try_send(Msg::Field {
                        generation,
                        before,
                        window,
                    });
                }));
            }
            _ => self.post(Msg::Field {
                generation,
                before: None,
                window: None,
            }),
        }
    }

    fn start_capture(&mut self, generation: u64) -> Result<String, String> {
        let tx = self.tx.clone();
        match &mut self.mode {
            Mode::Live { capture, .. } => {
                let c = crate::audio::start(tx, generation)?;
                let name = c.device.clone();
                *capture = Some(c);
                Ok(name)
            }
            Mode::Replay { pcm, stop } => {
                let flag = Arc::new(AtomicBool::new(false));
                *stop = Some(flag.clone());
                let pcm = pcm.clone();
                std::thread::spawn(move || feed(&pcm, generation, &flag, &tx));
                Ok("replay".into())
            }
        }
    }

    fn stop_capture(&mut self, generation: u64) {
        match &mut self.mode {
            Mode::Live { capture, .. } => {
                // Joins the stream; the last partial buffer is queued before CaptureEnded.
                if let Some(c) = capture.take() {
                    c.stop();
                }
                self.post(Msg::CaptureEnded { generation });
            }
            Mode::Replay { stop, .. } => {
                if let Some(f) = stop.take() {
                    f.store(true, Relaxed);
                }
                // A buffer the feeder sends after this arrives once the controller stopped capturing; it is dropped.
                self.post(Msg::CaptureEnded { generation });
            }
        }
    }

    fn verify_target(&mut self, generation: u64, id: DateTime<Utc>, text: String) {
        let tx = self.tx.clone();
        let Mode::Live {
            front, target, ax, ..
        } = &self.mode
        else {
            return self.post(Msg::Verified {
                generation,
                id,
                text,
                ok: true,
            });
        };
        let pid = front.filter(|f| f.0 == generation).map(|f| f.1);
        if pid.is_none() || os::front().map(|f| f.2) != pid {
            return self.post(Msg::Verified {
                generation,
                id,
                text,
                ok: false,
            });
        }
        let (target, ax) = (target.clone(), ax.clone());
        std::thread::spawn(move || {
            let (rtx, rrx) = std::sync::mpsc::channel();
            let _ = ax.send(Box::new(move || {
                let ok = target
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|(g, t)| *g == generation && t.matches());
                let _ = rtx.send(ok);
            }));
            let ok = rrx.recv_timeout(VERIFY_TIMEOUT).unwrap_or(false);
            let _ = tx.try_send(Msg::Verified {
                generation,
                id,
                text,
                ok,
            });
        });
    }

    fn paste(&mut self, generation: u64, text: &str) -> bool {
        match &self.mode {
            Mode::Replay { .. } => {
                println!("paste: {text}");
                true
            }
            Mode::Live {
                front,
                clipboard,
                fg,
                bg,
                ..
            } => {
                let Some((_, pid)) = front.filter(|f| f.0 == generation) else {
                    return false;
                };
                if os::front().map(|f| f.2) != Some(pid) {
                    return false;
                }
                let Some(owned) = clipboard.borrow_mut().paste(text) else {
                    return false;
                };
                let (clipboard, timer) = (clipboard.clone(), bg.timer(crate::paste::RESTORE_AFTER));
                fg.spawn(async move {
                    timer.await;
                    clipboard.borrow_mut().restore(Some(owned));
                })
                .detach();
                true
            }
        }
    }

    fn actions(&mut self) -> (Vec<Action>, String) {
        config::actions()
    }

    fn terms(&mut self, auto: bool) -> Vec<String> {
        config::terms(auto)
    }

    /// PLAID_LLM_KEY wins (replays); the Keychain read never prompts.
    fn llm_key(&mut self, base: &str) -> Option<String> {
        secret::llm_key(base)
    }

    fn llm(&mut self, id: u64, req: llm::Request) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = llm::send(&req, os::proxy().as_ref());
            let _ = tx.try_send(Msg::Llm { id, result });
        });
    }

    /// Baseline is the field value read at key-down; the pasted span is located once the paste has landed.
    fn watch_edits(&mut self, generation: u64, id: DateTime<Utc>, text: &str) {
        let Mode::Live {
            target,
            watch,
            watch_seq,
            ax,
            ..
        } = &mut self.mode
        else {
            return;
        };
        *watch_seq += 1;
        let (seq, text, tx) = (*watch_seq, text.to_owned(), self.tx.clone());
        let (target, w1, w2, tx2) = (target.clone(), watch.clone(), watch.clone(), tx.clone());
        Self::ax_later(
            ax,
            PASTE_SETTLE,
            Box::new(move || {
                let field = target
                    .lock()
                    .unwrap()
                    .as_ref()
                    .filter(|(g, _)| *g == generation)
                    .and_then(|(_, t)| t.field.clone());
                let Some(field) = field else { return };
                let Some(after) = field.reread() else { return };
                let Some((start, end, pasted)) = diff::pasted_span(&field.value, &after, &text)
                else {
                    return;
                };
                check(&w1, &tx);
                *w1.lock().unwrap() = Some(Watch {
                    seq,
                    id,
                    field,
                    base: after,
                    start,
                    end,
                    pasted,
                });
            }),
        );
        Self::ax_later(
            ax,
            PASTE_SETTLE + EDIT_WINDOW,
            Box::new(move || {
                if w2.lock().unwrap().as_ref().is_some_and(|w| w.seq == seq) {
                    check(&w2, &tx2);
                }
            }),
        );
    }

    fn check_edits(&mut self) {
        let Mode::Live { watch, ax, .. } = &self.mode else {
            return;
        };
        let (watch, tx) = (watch.clone(), self.tx.clone());
        let _ = ax.send(Box::new(move || check(&watch, &tx)));
    }

    /// Replays run commands for real too (use a scratch actions.json).
    fn run(&mut self, generation: u64, action: &Action, args: &str, env: plaid_core::actions::Env) {
        let (tx, action, args) = (self.tx.clone(), action.clone(), args.to_owned());
        std::thread::spawn(move || {
            let outcome = crate::run::run(&action, &args, &env);
            let _ = tx.try_send(Msg::Ran {
                generation,
                action,
                outcome,
            });
        });
    }

    fn recover(&mut self, id: DateTime<Utc>, audio: std::path::PathBuf, timeout: Duration) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = recognize(&audio, timeout);
            let _ = tx.try_send(Msg::Recovered { id, result });
        });
    }
}

/// One saved WAV through a fresh session without context, sent as fast as the socket takes it. Blocking.
fn recognize(audio: &std::path::Path, timeout: Duration) -> Result<String, String> {
    let bytes = std::fs::read(audio).map_err(|e| format!("无法读取录音：{e}"))?;
    let pcm = plaid_core::wav::decode(&bytes)?;
    if pcm.is_empty() {
        return Err("录音为空".into());
    }
    let mut opts = config::session_options(secret::api_key()?);
    opts.final_timeout = timeout;
    let (s, events, fut) = session::start(opts, config::hotwords());
    std::thread::spawn(move || async_io::block_on(fut));
    s.begin(vec![]);
    for c in pcm.chunks(3200) {
        s.append(c.to_vec());
    }
    s.finish();
    while let Ok(e) = events.recv_blocking() {
        if let session::Event::Done(r) = e {
            return r;
        }
    }
    Err("识别会话意外结束".into())
}

/// Real-time WAV feed: 100 ms buffers, then the key release, then silence until the capture is stopped.
fn feed(pcm: &[u8], generation: u64, stop: &AtomicBool, tx: &Sender<Msg>) {
    let t0 = Instant::now();
    let chunks = pcm
        .chunks(3200)
        .map(<[u8]>::to_vec)
        .map(Some)
        .chain(std::iter::once(None));
    let silence = std::iter::repeat_with(|| Some(vec![0u8; 3200]));
    for (i, chunk) in chunks.chain(silence).enumerate() {
        std::thread::sleep(
            (t0 + Duration::from_millis(100 * i as u64)).saturating_duration_since(Instant::now()),
        );
        if stop.load(Relaxed) {
            break;
        }
        let _ = tx.try_send(match chunk {
            Some(pcm) => Msg::Audio { generation, pcm },
            None => Msg::Key {
                action: gesture::Action::Stop,
                trigger_ms: None,
            },
        });
    }
}
