//! API keys in the OS credential store: macOS Keychain generic passwords under the bundle id (the release id
//! dev.neo.plaid.volc is the Swift app's service, so its key carries over), Windows Credential Manager otherwise.
//! Accounts: "apiKey" is the Volcengine key, "llm:<host>" the LLM keys.

/// PLAID_API_KEY wins, for replays and CI.
pub fn api_key() -> Result<String, String> {
    if let Ok(k) = std::env::var("PLAID_API_KEY") {
        return Ok(k);
    }
    get("apiKey")?.ok_or_else(|| "未设置火山引擎 API Key（设置 › 通用）".into())
}

#[cfg(target_os = "macos")]
fn service() -> String {
    objc2_foundation::NSBundle::mainBundle()
        .bundleIdentifier()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "dev.neo.plaid.dev".into())
}

/// Never shows a Keychain dialog: an ACL mismatch fails instead (the user then re-enters the key in Settings).
#[cfg(target_os = "macos")]
pub fn get(account: &str) -> Result<Option<String>, String> {
    use security_framework::base::Error;
    use security_framework::os::macos::keychain::SecKeychain;
    let _lock = SecKeychain::disable_user_interaction().map_err(|e| e.to_string())?;
    match security_framework::passwords::get_generic_password(&service(), account) {
        Ok(k) => String::from_utf8(k).map(Some).map_err(|e| e.to_string()),
        Err(e) if e.code() == Error::from_code(-25300).code() => Ok(None), // errSecItemNotFound
        Err(e) => Err(format!("keychain: {e}")),
    }
}

/// Empty value deletes the item.
#[cfg(target_os = "macos")]
pub fn set(account: &str, value: &str) -> Result<(), String> {
    let s = service();
    let _ = security_framework::passwords::delete_generic_password(&s, account);
    if value.is_empty() {
        return Ok(());
    }
    security_framework::passwords::set_generic_password(&s, account, value.as_bytes())
        .map_err(|e| format!("keychain: {e}"))
}

#[cfg(windows)]
fn target(account: &str) -> Vec<u16> {
    format!("Plaid/{account}")
        .encode_utf16()
        .chain([0])
        .collect()
}

#[cfg(windows)]
pub fn get(account: &str) -> Result<Option<String>, String> {
    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };
    let t = target(account);
    let mut p: *mut CREDENTIALW = std::ptr::null_mut();
    unsafe {
        if CredReadW(t.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) == 0 {
            let e = GetLastError();
            return if e == ERROR_NOT_FOUND {
                Ok(None)
            } else {
                Err(format!("CredRead: {e}"))
            };
        }
        let c = &*p;
        let blob =
            std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec();
        CredFree(p as _);
        String::from_utf8(blob).map(Some).map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
pub fn set(account: &str, value: &str) -> Result<(), String> {
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredWriteW,
    };
    let t = target(account);
    unsafe {
        CredDeleteW(t.as_ptr(), CRED_TYPE_GENERIC, 0);
        if value.is_empty() {
            return Ok(());
        }
        let mut c: CREDENTIALW = std::mem::zeroed();
        c.Type = CRED_TYPE_GENERIC;
        c.TargetName = t.as_ptr() as _;
        c.CredentialBlobSize = value.len() as u32;
        c.CredentialBlob = value.as_ptr() as _;
        c.Persist = CRED_PERSIST_LOCAL_MACHINE;
        if CredWriteW(&c, 0) == 0 {
            Err(format!("CredWrite: {}", std::io::Error::last_os_error()))
        } else {
            Ok(())
        }
    }
}
