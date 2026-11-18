//! Windows: a WH_KEYBOARD_LL hook on its own message-loop thread. Needs no permission. Not yet run on a real
//! machine. Limits: it sees nothing while an elevated window has focus (UIPI); Windows silently removes a hook whose
//! callback exceeds LowLevelHooksTimeout, so the callback only recognizes and posts; injected events (ours and
//! other tools') are ignored.
use super::{Tap, vk};
use crate::win;
use std::cell::RefCell;
use std::sync::Arc;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED,
    LLKHF_UP, MSG, SetWindowsHookExW, WH_KEYBOARD_LL,
};

struct Hook {
    tap: Arc<Tap>,
    /// Per virtual key; the hook reports auto-repeat as more key-downs.
    down: [bool; 256],
    /// The fake left Ctrl of an AltGr press is held (it must not make right Alt a chord).
    altgr_ctrl: bool,
}

impl Hook {
    /// (swallow, send the mask key).
    fn event(&mut self, k: &KBDLLHOOKSTRUCT) -> (bool, bool) {
        if k.flags & LLKHF_INJECTED != 0 || k.vkCode > 255 {
            return (false, false);
        }
        let down = k.flags & LLKHF_UP == 0;
        if k.vkCode == vk::LCONTROL && k.scanCode == vk::ALTGR_CTRL_SCAN {
            self.altgr_ctrl = down;
            return (false, false);
        }
        let code = vk::code(k.vkCode, k.flags & LLKHF_EXTENDED != 0);
        let repeat = down && self.down[k.vkCode as usize];
        self.down[k.vkCode as usize] = down;
        let altgr = self.altgr_ctrl;
        let other = |c: i32| {
            vk::other(c, |v| {
                !(altgr && v == vk::LCONTROL) && unsafe { GetAsyncKeyState(v as i32) } < 0
            })
        };
        let t = &self.tap;
        if down && !other(code) && t.command_key(code) {
            return (true, false);
        }
        let swallow = t.key(code, down, win::event_time(k.time), repeat, &other);
        // Whether injected input lands before or after the event being hooked is not documented, so the mask goes
        // out on both edges: one of them always falls between the bound key's down and up.
        let bound = code == t.hold() || code == t.tap();
        (swallow, bound && !repeat && vk::needs_mask(code))
    }
}

thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

unsafe extern "system" fn proc(n: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if n == HC_ACTION as i32 {
        let k = unsafe { &*(l as *const KBDLLHOOKSTRUCT) };
        let (swallow, mask) = HOOK.with(|h| {
            h.borrow_mut()
                .as_mut()
                .map_or((false, false), |h| h.event(k))
        });
        if mask {
            win::send_keys(&[(vk::MASK_KEY, true), (vk::MASK_KEY, false)]);
        }
        if swallow {
            return 1;
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), n, w, l) }
}

pub(super) fn install(t: Arc<Tap>) -> Result<(), String> {
    let (ok_tx, ok_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            HOOK.with(|h| {
                *h.borrow_mut() = Some(Hook {
                    tap: t,
                    down: [false; 256],
                    altgr_ctrl: false,
                })
            });
            let hook = unsafe {
                SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(proc),
                    GetModuleHandleW(std::ptr::null()),
                    0,
                )
            };
            if hook.is_null() {
                let e = std::io::Error::last_os_error();
                let _ = ok_tx.send(Err(format!("SetWindowsHookExW: {e}")));
                return;
            }
            let _ = ok_tx.send(Ok(()));
            // Hook callbacks are delivered inside GetMessageW.
            let mut msg = MSG::default();
            while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {}
        })
        .map_err(|e| e.to_string())?;
    ok_rx.recv().map_err(|e| e.to_string())?
}
