// Active (default) CGEventTap for one standalone key. Active taps need Accessibility, which paste needs anyway;
// a listen-only tap would need Input Monitoring as a second permission. Every event is passed through unchanged.
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use core_graphics::event::{CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType, CallbackResult, EventField};
use std::cell::{Cell, RefCell};
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug)]
pub enum Key {
    Down(Instant),
    Up,
    /// Any other key or modifier: invalidates a press that hasn't started recording.
    Other,
}

/// Device-dependent modifier bits (NX_DEVICE*KEYMASK), same table as Hotkeys.modifiers in Swift.
const MASKS: [(i64, u64); 9] =
    [(54, 0x10), (55, 0x08), (56, 0x02), (60, 0x04), (58, 0x20), (61, 0x40), (59, 0x01), (62, 0x2000), (63, 0x80_0000)];

fn mask(code: i64) -> Option<u64> {
    MASKS.iter().find(|m| m.0 == code).map(|m| m.1)
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: core_foundation::dictionary::CFDictionaryRef) -> bool;
    static kAXTrustedCheckOptionPrompt: core_foundation::string::CFStringRef;
}

/// Accessibility trust; shows the system prompt when missing. Covers both the key tap and posting ⌘V.
pub fn accessibility() -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::{boolean::CFBoolean, dictionary::CFDictionary, string::CFString};
    unsafe {
        let opts = CFDictionary::from_CFType_pairs(&[(CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt), CFBoolean::true_value())]);
        AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef())
    }
}

/// Non-prompting check.
pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

thread_local! {
    static TAP: RefCell<Option<CGEventTap<'static>>> = const { RefCell::new(None) };
}

pub fn spawn(code: i64, tx: UnboundedSender<Key>) -> Result<(), String> {
    // Right ⌥ (61) belongs to the installed Swift Plaid on this machine.
    assert!(code != 61, "key 61 is reserved for the installed Plaid");
    let target = mask(code).ok_or("spike only supports modifier keys")?;
    let (ok_tx, ok_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            let down = Cell::new(false);
            let tap = CGEventTap::new(
                CGEventTapLocation::Session,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                vec![CGEventType::FlagsChanged, CGEventType::KeyDown],
                move |_, ty, e| {
                    match ty {
                        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
                            TAP.with(|t| t.borrow().as_ref().map(|t| t.enable()));
                        }
                        CGEventType::FlagsChanged => {
                            let c = e.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                            let flags = e.get_flags().bits();
                            if c == code {
                                let now = flags & target != 0;
                                if now != down.get() {
                                    down.set(now);
                                    let _ = tx.send(if now { Key::Down(Instant::now()) } else { Key::Up });
                                }
                            } else if mask(c).is_some_and(|m| flags & m != 0) {
                                let _ = tx.send(Key::Other);
                            }
                        }
                        CGEventType::KeyDown => {
                            let _ = tx.send(Key::Other);
                        }
                        _ => {}
                    }
                    CallbackResult::Keep
                },
            );
            let Ok(tap) = tap else {
                let _ = ok_tx.send(Err("CGEventTapCreate failed (Accessibility not granted?)".to_owned()));
                return;
            };
            let src = tap.mach_port().create_runloop_source(0).expect("runloop source");
            CFRunLoop::get_current().add_source(&src, unsafe { kCFRunLoopCommonModes });
            tap.enable();
            TAP.with(|t| *t.borrow_mut() = Some(tap));
            let _ = ok_tx.send(Ok(()));
            CFRunLoop::run_current();
        })
        .map_err(|e| e.to_string())?;
    ok_rx.recv().map_err(|e| e.to_string())?
}
