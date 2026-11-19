//! Windows counterparts of mac.rs: foreground app, paste-target checks, the event clock and synthetic keys.
//! Not yet run on a real machine. No focused-field text yet (UI Automation), so PLAID_BEFORE, the ASR's field
//! context and edit learning are empty on Windows.
use std::collections::HashMap;
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{CloseHandle, HWND};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows_sys::Win32::System::SystemInformation::{GetTickCount, GetTickCount64};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetWindowTextW, GetWindowThreadProcessId,
};

/// Internet Options proxy (WinHTTP's view of the user's settings). PAC/auto-detect are not followed.
pub fn proxy() -> Option<plaid_core::llm::SystemProxy> {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::Networking::WinHttp::{
        WINHTTP_CURRENT_USER_IE_PROXY_CONFIG, WinHttpGetIEProxyConfigForCurrentUser,
    };
    let mut c = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
    if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut c) } == 0 {
        return None;
    }
    let take = |p: *mut u16| -> String {
        if p.is_null() {
            return String::new();
        }
        let s = unsafe { wide(p) };
        unsafe { GlobalFree(p.cast()) };
        s
    };
    let (_, server, bypass) = (
        take(c.lpszAutoConfigUrl),
        take(c.lpszProxy),
        take(c.lpszProxyBypass),
    );
    (!server.is_empty()).then(|| plaid_core::llm::SystemProxy::windows(&server, &bypass))
}

/// NUL-terminated UTF-16.
unsafe fn wide(p: *const u16) -> String {
    let n = (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count();
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Seconds on the tick clock; the gesture compares hook event times against it.
pub fn now() -> f64 {
    (unsafe { GetTickCount64() }) as f64 / 1000.0
}

/// KBDLLHOOKSTRUCT.time (GetTickCount milliseconds, wraps every 49.7 days) in `now()` seconds.
pub fn event_time(ms: u32) -> f64 {
    now() - (unsafe { GetTickCount() }).wrapping_sub(ms) as f64 / 1000.0
}

/// Synthetic key presses (virtual key, down). They reach only windows at or below our integrity level.
pub fn send_keys(keys: &[(u16, bool)]) {
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(vk, down)| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if down { 0 } else { KEYEVENTF_KEYUP },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        })
        .collect();
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
}

/// Name (file description, else exe stem), exe file name (PLAID_BUNDLE) and pid of the foreground window's app.
pub fn front() -> Option<(String, Option<String>, i32)> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return None;
    }
    let Some(exe) = exe_path(pid) else {
        return Some((String::new(), None, pid as i32));
    };
    let path = std::path::Path::new(&exe);
    let file = path.file_name().map(|f| f.to_string_lossy().into_owned());
    let name = describe(&exe).unwrap_or_else(|| {
        path.file_stem()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    Some((name, file, pid as i32))
}

fn exe_path(pid: u32) -> Option<String> {
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let mut buf = [0u16; 1024];
    let mut n = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut n) };
    unsafe { CloseHandle(h) };
    (ok != 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

/// FileDescription from the exe's version resource ("Microsoft Edge" for msedge.exe), cached per path.
fn describe(exe: &str) -> Option<String> {
    static CACHE: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);
    if let Some(d) = CACHE.lock().unwrap().get_or_insert_default().get(exe) {
        return d.clone();
    }
    let d = version_description(exe).filter(|d| !d.trim().is_empty());
    CACHE
        .lock()
        .unwrap()
        .get_or_insert_default()
        .insert(exe.to_owned(), d.clone());
    d
}

fn version_description(exe: &str) -> Option<String> {
    let path = to_wide(exe);
    let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), std::ptr::null_mut()) };
    if size == 0 {
        return None;
    }
    let mut info = vec![0u8; size as usize];
    if unsafe { GetFileVersionInfoW(path.as_ptr(), 0, size, info.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    // Returns a pointer into `info` and its length (bytes for binary values, UTF-16 units for strings).
    let query = |sub: &str| {
        let (mut p, mut n) = (std::ptr::null_mut(), 0u32);
        let ok =
            unsafe { VerQueryValueW(info.as_ptr().cast(), to_wide(sub).as_ptr(), &mut p, &mut n) };
        (ok != 0 && !p.is_null() && n > 0).then_some((p, n))
    };
    let (tr, n) = query("\\VarFileInfo\\Translation")?;
    if n < 4 {
        return None;
    }
    let [lang, cp] = unsafe { *(tr as *const [u16; 2]) };
    let (s, _) = query(&format!(
        "\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription"
    ))?;
    Some(unsafe { wide(s as *const u16) })
}

fn window_text(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

/// Keyboard-focus window of a GUI thread (for browsers and other custom-drawn apps, the whole content area).
fn focused_in(thread: u32) -> Option<usize> {
    let mut g = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    (unsafe { GetGUIThreadInfo(thread, &mut g) } != 0 && !g.hwndFocus.is_null())
        .then_some(g.hwndFocus as usize)
}

/// Where the utterance started: the foreground window and its keyboard-focus window (handles kept as integers so
/// the target can cross to the worker thread).
pub struct Target {
    window: usize,
    focus: Option<usize>,
    /// Foreground window's title (refine's app context).
    pub title: Option<String>,
    pub field: Option<Field>,
}

impl Target {
    /// Still focused where the utterance started.
    pub fn matches(&self) -> bool {
        let hwnd = unsafe { GetForegroundWindow() };
        if self.window == 0 || hwnd as usize != self.window {
            return false;
        }
        let thread = unsafe { GetWindowThreadProcessId(hwnd, std::ptr::null_mut()) };
        self.focus.is_none_or(|f| focused_in(thread) == Some(f))
    }
}

/// Focused text field; not read on Windows yet (needs UI Automation).
#[derive(Clone)]
pub struct Field {
    pub value: String,
}

impl Field {
    pub fn reread(&self) -> Option<String> {
        None
    }
}

/// Paste target of `pid`'s foreground window. Field text is not read yet, so `before` is always None.
pub fn focus(pid: i32, _n: usize) -> (Target, Option<String>) {
    let mut t = Target {
        window: 0,
        focus: None,
        title: None,
        field: None,
    };
    if pid == std::process::id() as i32 {
        return (t, None);
    }
    let hwnd = unsafe { GetForegroundWindow() };
    let mut owner = 0u32;
    let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
    if hwnd.is_null() || owner as i32 != pid {
        return (t, None);
    }
    t.window = hwnd as usize;
    t.title = window_text(hwnd);
    t.focus = focused_in(thread);
    (t, None)
}

/// Blocking error message box (the release exe has no console).
pub fn alert(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW,
    };
    let (t, m) = (to_wide(title), to_wide(text));
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            m.as_ptr(),
            t.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        )
    };
}

/// Whether the taskbar uses the light theme (Personalize\SystemUsesLightTheme); dark when unreadable.
pub fn light_taskbar() -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let key = to_wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = to_wide("SystemUsesLightTheme");
    let (mut v, mut n) = (0u32, 4u32);
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut v as *mut u32).cast(),
            &mut n,
        )
    };
    r == 0 && v != 0
}
