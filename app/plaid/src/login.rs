//! Launch at login. macOS registers the app bundle with SMAppService (a login item, listed in System Settings ›
//! General › Login Items); Windows writes HKCU\...\Run. The system is the source of truth, so turning it off in
//! System Settings shows up as off here. `launchAtLogin` in settings.json only remembers that the user chose, so the
//! first-launch default is applied once.
use crate::config;

/// Release builds turn it on once, on first launch; a later "0" in settings stays off.
pub fn default_on() {
    if !config::release() || cfg!(feature = "snapshot") || config::raw("launchAtLogin").is_some() {
        return;
    }
    match set(true) {
        Ok(()) => crate::trace::log("login item: on (first launch)"),
        Err(e) => crate::trace::log(&format!("login item: {e}")),
    }
}

/// Turns it on or off and records the choice.
pub fn set(on: bool) -> Result<(), String> {
    let _ = config::set("launchAtLogin", (if on { "1" } else { "0" }).into());
    if cfg!(feature = "snapshot") || !config::release() {
        return Ok(());
    }
    imp::set(on)
}

pub fn enabled() -> bool {
    if cfg!(feature = "snapshot") || !config::release() {
        return config::value("launchAtLogin").as_deref() == Some("1");
    }
    imp::enabled()
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool};

    #[link(name = "ServiceManagement", kind = "framework")]
    unsafe extern "C" {}

    fn service() -> Option<Retained<AnyObject>> {
        let cls = AnyClass::get(c"SMAppService")?;
        // SAFETY: +[SMAppService mainAppService] returns the shared service for this bundle.
        unsafe { objc2::msg_send![cls, mainAppService] }
    }

    /// SMAppServiceStatus: 0 not registered, 1 enabled, 2 requires approval, 3 not found.
    pub fn enabled() -> bool {
        service().is_some_and(|s| {
            let status: isize = unsafe { objc2::msg_send![&*s, status] };
            status == 1 || status == 2
        })
    }

    pub fn set(on: bool) -> Result<(), String> {
        let s = service().ok_or("SMAppService unavailable")?;
        let mut err: *mut AnyObject = std::ptr::null_mut();
        // SAFETY: -registerAndReturnError: / -unregisterAndReturnError: take an NSError** and return BOOL.
        let ok: Bool = unsafe {
            if on {
                objc2::msg_send![&*s, registerAndReturnError: &mut err]
            } else {
                objc2::msg_send![&*s, unregisterAndReturnError: &mut err]
            }
        };
        if ok.as_bool() || (!on && !enabled()) {
            return Ok(());
        }
        let desc = if err.is_null() {
            "unknown error".to_string()
        } else {
            // SAFETY: err is an NSError set by the call above.
            let d: Retained<objc2_foundation::NSString> =
                unsafe { objc2::msg_send![&*err, localizedDescription] };
            d.to_string()
        };
        Err(desc)
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "Plaid";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub fn enabled() -> bool {
        let (key, name) = (wide(KEY), wide(NAME));
        let mut n = 0u32;
        // SAFETY: size query only.
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut n,
            )
        };
        r == 0
    }

    pub fn set(on: bool) -> Result<(), String> {
        let (key, name) = (wide(KEY), wide(NAME));
        let r = if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let cmd = wide(&format!("\"{}\"", exe.display()));
            // SAFETY: cmd is a NUL-terminated UTF-16 string; the size includes the NUL.
            unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    name.as_ptr(),
                    REG_SZ,
                    cmd.as_ptr().cast(),
                    (cmd.len() * 2) as u32,
                )
            }
        } else {
            let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
            if r == 2 { 0 } else { r }
        };
        if r == 0 {
            Ok(())
        } else {
            Err(format!("registry error {r}"))
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    pub fn enabled() -> bool {
        false
    }
    pub fn set(_: bool) -> Result<(), String> {
        Ok(())
    }
}
