//! macOS: one active CGEventTap (flagsChanged + keyDown/keyUp) on its own run-loop thread. Active taps need
//! Accessibility, which paste needs anyway.
use super::Tap;
use crate::mac;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use core_graphics::event::{
    CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult, EventField,
};
use std::cell::RefCell;
use std::sync::Arc;

/// Device-dependent bits distinguish the two sides of each modifier (Hotkeys.modifiers).
const MASKS: [(i32, u64); 9] = [
    (54, 0x10),
    (55, 0x08),
    (56, 0x02),
    (60, 0x04),
    (58, 0x20),
    (61, 0x40),
    (59, 0x01),
    (62, 0x2000),
    (63, 0x80_0000),
];
const COMMAND: u64 = 0x10_0000;
const SHIFT: u64 = 0x2_0000;
const OPTION: u64 = 0x8_0000;
const CONTROL: u64 = 0x4_0000;
const FUNCTION: u64 = 0x80_0000;
const F_KEYS: [i32; 20] = [
    122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107, 113, 106, 64, 79, 80, 90,
];

fn mask(code: i32) -> Option<u64> {
    MASKS.iter().find(|m| m.0 == code).map(|m| m.1)
}

/// Modifiers other than `code`'s own family are held (Hotkeys.hasOtherModifiers).
fn other_modifiers(code: i32, flags: u64) -> bool {
    let mut relevant = COMMAND | SHIFT | OPTION | CONTROL | FUNCTION;
    relevant &= !match code {
        54 | 55 => COMMAND,
        56 | 60 => SHIFT,
        58 | 61 => OPTION,
        59 | 62 => CONTROL,
        63 => FUNCTION,
        // macOS marks function/navigation keys with Fn even without a physical Fn press.
        _ if F_KEYS.contains(&code) || (115..=126).contains(&code) => FUNCTION,
        _ => 0,
    };
    if flags & relevant != 0 {
        return true;
    }
    // The aggregate bit cannot distinguish the opposite key of the same family.
    MASKS
        .iter()
        .any(|&(k, m)| k != code && k != 63 && flags & m != 0)
}

fn event(t: &Arc<Tap>, ty: CGEventType, e: &CGEvent) -> bool {
    let code = e.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as i32;
    let flags = e.get_flags().bits();
    let other = |c: i32| other_modifiers(c, flags);
    let at = mac::event_time(unsafe {
        CGEventGetTimestamp(foreign_types::ForeignType::as_ptr(e).cast())
    });
    match ty {
        CGEventType::FlagsChanged => {
            let Some(m) = mask(code) else { return false };
            // A modifier reports flagsChanged only on press and release, so while it is held the next event for
            // it is the release whatever its flags say (synthetic releases may keep the bit set).
            let held = t
                .gesture
                .lock()
                .unwrap()
                .press()
                .is_some_and(|p| p.code == code);
            t.key(code, flags & m != 0 && !held, at, false, &other);
            false
        }
        CGEventType::KeyDown | CGEventType::KeyUp => {
            let down = matches!(ty, CGEventType::KeyDown);
            if down && !other(code) && t.command_key(code) {
                return true;
            }
            let repeat = e.get_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT) != 0;
            t.key(code, down, at, repeat, &other)
        }
        _ => false,
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventGetTimestamp(e: *const std::ffi::c_void) -> u64;
}

thread_local! {
    static TAP: RefCell<Option<CGEventTap<'static>>> = const { RefCell::new(None) };
}

pub(super) fn install(t: Arc<Tap>) -> Result<(), String> {
    let (ok_tx, ok_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            let created = CGEventTap::new(
                CGEventTapLocation::Session,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                vec![
                    CGEventType::FlagsChanged,
                    CGEventType::KeyDown,
                    CGEventType::KeyUp,
                ],
                move |_, ty, e| match ty {
                    CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
                        TAP.with(|t| t.borrow().as_ref().map(|t| t.enable()));
                        CallbackResult::Keep
                    }
                    _ if event(&t, ty, e) => CallbackResult::Drop,
                    _ => CallbackResult::Keep,
                },
            );
            let Ok(tap) = created else {
                let _ = ok_tx.send(Err("CGEventTapCreate failed".to_owned()));
                return;
            };
            let Ok(src) = tap.mach_port().create_runloop_source(0) else {
                let _ = ok_tx.send(Err("tap run loop source".to_owned()));
                return;
            };
            CFRunLoop::get_current().add_source(&src, unsafe { kCFRunLoopCommonModes });
            tap.enable();
            TAP.with(|t| *t.borrow_mut() = Some(tap));
            let _ = ok_tx.send(Ok(()));
            CFRunLoop::run_current();
        })
        .map_err(|e| e.to_string())?;
    ok_rx.recv().map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_modifier_rules() {
        assert!(!other_modifiers(54, COMMAND | 0x10), "own family");
        assert!(
            other_modifiers(54, COMMAND | 0x10 | 0x08),
            "left ⌘ while right ⌘ is the key"
        );
        assert!(other_modifiers(54, SHIFT | 0x02));
        assert!(!other_modifiers(105, FUNCTION), "F13 carries Fn");
        assert!(other_modifiers(0, FUNCTION));
    }
}
