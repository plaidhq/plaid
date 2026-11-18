//! Hotkey: a platform hook (macOS CGEventTap, Windows WH_KEYBOARD_LL) feeds key events in macOS key codes, the
//! settings' namespace. The gesture is recognized synchronously inside the hook so swallow decisions stay ordered;
//! the controller state it needs (toggled, whether ⏎/esc belong to Plaid) is mirrored into atomics by the driver
//! after every message.
use crate::os;
use async_channel::Sender;
use plaid_core::dictation::{Keys, Msg};
use plaid_core::gesture::{Gesture, HOLD_DELAY, KeyEvent, Press};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(target_os = "macos")]
mod cgtap;
#[cfg(target_os = "macos")]
use cgtap as hook;
#[cfg(windows)]
mod llhook;
#[cfg(windows)]
use llhook as hook;
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod vk;

/// Both sides of ⌘ ⇧ ⌥ ⌃, and fn.
const MODIFIERS: [i32; 9] = [54, 55, 56, 60, 58, 61, 59, 62, 63];
const RETURN: i32 = 36;
const ENTER: i32 = 76;
const ESCAPE: i32 = 53;

/// Controller facts the hook reads.
#[derive(Default)]
pub struct State {
    toggled: AtomicBool,
    escape: AtomicBool,
    enter: AtomicBool,
}

impl State {
    pub fn set(&self, k: Keys) {
        self.toggled.store(k.toggled, Relaxed);
        self.escape.store(k.escape, Relaxed);
        self.enter.store(k.enter, Relaxed);
    }
}

struct Tap {
    hold: i32,
    tap: i32,
    gesture: Mutex<Gesture>,
    state: Arc<State>,
    tx: Sender<Msg>,
}

fn trigger_ms(at: f64) -> Option<i64> {
    let ms = ((os::now() - at) * 1000.) as i64;
    (0..=10_000).contains(&ms).then_some(ms)
}

impl Tap {
    fn send(&self, m: Msg) {
        let _ = self.tx.try_send(m);
    }

    /// Recognize synchronously so chord/repeat/swallow decisions stay ordered. `other(c)`: modifiers other than
    /// key `c` are held. Returns whether to swallow.
    fn key(
        self: &Arc<Self>,
        code: i32,
        down: bool,
        at: f64,
        repeat: bool,
        other: &dyn Fn(i32) -> bool,
    ) -> bool {
        let mut g = self.gesture.lock().unwrap();
        let previous = g.press();
        let e = KeyEvent {
            code,
            down,
            at,
            modifier: MODIFIERS.contains(&code),
            repeat,
            other,
        };
        let (swallow, actions) = g.key(&e, self.hold, self.tap, self.state.toggled.load(Relaxed));
        let triggered = g.press().or(previous).map(|p| p.at);
        for action in actions {
            self.send(Msg::Key {
                action,
                trigger_ms: triggered.and_then(trigger_ms),
            });
        }
        if let Some(p) = g
            .press()
            .filter(|p| !p.other_key && p.code == self.hold && !same(Some(*p), previous))
        {
            let me = self.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs_f64(
                    (p.at + HOLD_DELAY - os::now()).max(0.),
                ));
                let mut g = me.gesture.lock().unwrap();
                if !same(g.press(), Some(p)) {
                    return;
                }
                for action in g.advance(os::now(), me.hold) {
                    me.send(Msg::Key {
                        action,
                        trigger_ms: trigger_ms(p.at),
                    });
                }
            });
        }
        swallow
    }

    /// Only unmodified ⏎/esc belong to Plaid, and only while the controller says so. Key-down only.
    fn command_key(&self, code: i32) -> bool {
        let msg = match code {
            ESCAPE if self.state.escape.load(Relaxed) => Msg::Escape,
            RETURN | ENTER if self.state.enter.load(Relaxed) => Msg::Return,
            _ => return false,
        };
        self.gesture.lock().unwrap().clear_press();
        self.send(msg);
        true
    }
}

fn same(a: Option<Press>, b: Option<Press>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.code == b.code && a.at == b.at,
        _ => false,
    }
}

/// Installs the hook on its own thread. macOS callers check `mac::trusted()` first (an active tap needs
/// Accessibility). -1 disables a binding.
pub fn spawn(hold: i32, tap: i32, tx: Sender<Msg>, state: Arc<State>) -> Result<(), String> {
    hook::install(Arc::new(Tap {
        hold,
        tap,
        gesture: Default::default(),
        state,
        tx,
    }))
}
