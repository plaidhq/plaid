//! Paths and settings. Settings live in settings.json in the data dir; `PLAID_<KEY>` env vars win
//! (camelCase key → PLAID_SNAKE_CASE, as in the Swift app).
use plaid_core::hotwords::{self, Snapshot};
use std::path::PathBuf;

/// ~/Library/Application Support/Plaid (release; the retired Swift app's data dir too) or "Plaid Dev" (dev bundle and
/// unbundled runs); %APPDATA%\Plaid on Windows. PLAID_DATA_DIR overrides.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("PLAID_DATA_DIR") {
        return d.into();
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    };
    base.unwrap_or_default()
        .join(if release() { "Plaid" } else { "Plaid Dev" })
}

/// Release bundle (com.neospaceindustries.plaid) or the Windows build.
pub fn release() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2_foundation::NSBundle::mainBundle()
            .bundleIdentifier()
            .is_some_and(|s| s.to_string() == "com.neospaceindustries.plaid")
    }
    #[cfg(not(target_os = "macos"))]
    {
        !cfg!(debug_assertions)
    }
}

fn env_name(key: &str) -> String {
    let mut s = String::from("PLAID_");
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            s.push('_');
        }
        s.push(c.to_ascii_uppercase());
    }
    s
}

/// Non-empty setting value: env first, then settings.json (strings, numbers and bools as text).
pub fn value(key: &str) -> Option<String> {
    raw(key).filter(|s| !s.is_empty())
}

/// Like `value`, but an empty string counts as set.
pub fn raw(key: &str) -> Option<String> {
    std::env::var(env_name(key)).ok().or_else(|| {
        let body = std::fs::read(data_dir().join("settings.json")).ok()?;
        match serde_json::from_slice::<serde_json::Value>(&body)
            .ok()?
            .get(key)?
        {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Bool(b) => Some(if *b { "1" } else { "0" }.into()),
            v @ serde_json::Value::Number(_) => Some(v.to_string()),
            _ => None,
        }
    })
}

/// Writes one setting into settings.json (read-modify-write, unknown keys kept); `Null` removes it.
pub fn set(key: &str, v: serde_json::Value) -> std::io::Result<()> {
    let path = data_dir().join("settings.json");
    let mut map = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&b).ok())
        .unwrap_or_default();
    if v.is_null() {
        map.remove(key);
    } else {
        map.insert(key.into(), v);
    }
    std::fs::create_dir_all(data_dir())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&map)?)?;
    std::fs::rename(tmp, path)
}

pub fn session_options(key: String) -> plaid_core::session::Options {
    let mut o = plaid_core::session::Options::new(key);
    if let Some(u) = value("url") {
        o.url = u;
    }
    if let Some(r) = value("resourceId") {
        o.resource = r;
    }
    o.nonstream = value("nonstream").as_deref() != Some("0");
    o
}

/// hotwords.txt + hotwords.auto.txt (+ the file named by hotwordsExtra), re-read per utterance.
pub fn hotwords() -> Snapshot {
    snapshot(true)
}

/// Wanted spellings. Without `auto`, only the user's own files: learned rules must not vote for themselves.
pub fn terms(auto: bool) -> Vec<String> {
    snapshot(auto).terms
}

fn snapshot(auto: bool) -> Snapshot {
    let dir = data_dir();
    let read = |p: PathBuf| std::fs::read_to_string(p).unwrap_or_default();
    let extra = value("hotwordsExtra")
        .map(|p| read(p.into()))
        .unwrap_or_default();
    let learned = if auto {
        read(dir.join(plaid_core::store::AUTO_RULES))
    } else {
        String::new()
    };
    let body = [read(dir.join("hotwords.txt")), learned, extra].join("\n");
    let threshold = value("hotwordThresh")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.85);
    Snapshot::new(&hotwords::lines(&body), threshold)
}

/// Voice commands from actions.json (re-read per utterance) and the wake word; empty = bare mode.
pub fn actions() -> (Vec<plaid_core::actions::Action>, String) {
    let list = std::fs::read(data_dir().join("actions.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let wake = if value("commandMode").as_deref() == Some("bare") {
        String::new()
    } else {
        raw("wakeWord").unwrap_or_else(|| "Plaid".into())
    };
    (list, wake)
}

/// Hold and tap key codes (macOS codes on every platform); -1 disables. Right ⌥ / right Alt (61) is the default,
/// except for macOS dev builds: there it belongs to the installed release Plaid, so they use right ⌘ (54) and refuse
/// 61.
pub fn hotkeys() -> (i32, i32) {
    let dev_mac = cfg!(target_os = "macos") && !release();
    let default = if dev_mac { 54 } else { 61 };
    let code = |k| value(k).and_then(|s| s.parse().ok()).unwrap_or(default);
    let guard = |c: i32| if c == 61 && dev_mac { 54 } else { c };
    (guard(code("holdKey")), guard(code("tapKey")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn env_names() {
        assert_eq!(super::env_name("resourceId"), "PLAID_RESOURCE_ID");
        assert_eq!(super::env_name("hotwordThresh"), "PLAID_HOTWORD_THRESH");
    }
}
