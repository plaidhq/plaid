//! API keys in the OS credential store: on macOS one Keychain item under the bundle id holding all accounts
//! (migrate.rs copies the Swift app's keys once), on Windows one Credential Manager entry per account.
//! Accounts: "apiKey" is the Volcengine key, "llm:<host>" the LLM keys.

/// An LLM endpoint's key: PLAID_LLM_KEY, else the credential stored for its host.
pub fn llm_key(base: &str) -> Option<String> {
    std::env::var("PLAID_LLM_KEY")
        .ok()
        .or_else(|| get(&plaid_core::llm::key_account(base)).ok().flatten())
        .filter(|k| !k.is_empty())
}

/// PLAID_API_KEY wins, for replays and CI.
pub fn api_key() -> Result<String, String> {
    if let Ok(k) = std::env::var("PLAID_API_KEY") {
        return Ok(k);
    }
    get("apiKey")?.ok_or_else(|| "未设置火山引擎 API Key（设置 › 通用）".into())
}

#[cfg(target_os = "macos")]
fn bundle_id() -> Option<String> {
    objc2_foundation::NSBundle::mainBundle()
        .bundleIdentifier()
        .map(|s| s.to_string())
}

#[cfg(target_os = "macos")]
fn service() -> String {
    bundle_id().unwrap_or_else(|| "com.neospaceindustries.plaid.dev".into())
}

/// macOS keeps every account in one generic password (account "secrets", a JSON object), so a rebuild whose
/// signature no longer matches the item's ACL costs one Keychain dialog, not one per key. Loaded once per launch,
/// failure included, so the dialog shows at most once.
#[cfg(target_os = "macos")]
static SECRETS: std::sync::Mutex<Option<Result<Secrets, String>>> = std::sync::Mutex::new(None);
#[cfg(target_os = "macos")]
type Secrets = std::collections::BTreeMap<String, String>;
#[cfg(target_os = "macos")]
const ACCOUNT: &str = "secrets";

/// Only a bundled app may show the dialog; a bare binary (tests, snapshots, replays) fails instead.
#[cfg(target_os = "macos")]
fn load() -> Result<Secrets, String> {
    use security_framework::base::Error;
    use security_framework::os::macos::keychain::SecKeychain;
    let _lock = match bundle_id() {
        Some(_) => None,
        None => Some(SecKeychain::disable_user_interaction().map_err(|e| e.to_string())?),
    };
    match security_framework::passwords::get_generic_password(&service(), ACCOUNT) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("keychain: {e}")),
        Err(e) if e.code() == Error::from_code(-25300).code() => Ok(Secrets::new()), // errSecItemNotFound
        Err(e) => Err(format!("keychain: {e}")),
    }
}

#[cfg(target_os = "macos")]
pub fn get(account: &str) -> Result<Option<String>, String> {
    let mut cache = SECRETS.lock().unwrap();
    match cache.get_or_insert_with(load) {
        Ok(m) => Ok(m.get(account).cloned()),
        Err(e) => Err(e.clone()),
    }
}

/// Empty value removes the account. When the item could not be read, the rewrite starts from an empty map and
/// replaces it.
#[cfg(target_os = "macos")]
pub fn set(account: &str, value: &str) -> Result<(), String> {
    let mut cache = SECRETS.lock().unwrap();
    let mut m = cache.get_or_insert_with(load).clone().unwrap_or_default();
    if value.is_empty() {
        m.remove(account);
    } else {
        m.insert(account.into(), value.into());
    }
    let s = service();
    let _ = security_framework::passwords::delete_generic_password(&s, ACCOUNT);
    if !m.is_empty() {
        let json = serde_json::to_vec(&m).map_err(|e| e.to_string())?;
        security_framework::passwords::set_generic_password(&s, ACCOUNT, &json)
            .map_err(|e| format!("keychain: {e}"))?;
    }
    *cache = Some(Ok(m));
    Ok(())
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
