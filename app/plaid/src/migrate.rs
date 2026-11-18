//! One-time import from the retired Swift app (bundle id dev.neo.plaid.volc) on the release build's first launch.
//! Its records, hotwords and actions already live in the shared data dir; this copies its UserDefaults into
//! settings.json and its Keychain keys under the new bundle id. Runs only while settings.json does not exist.
use crate::{config, secret, trace};
use objc2_foundation::{NSNumber, NSString, NSUserDefaults};
use security_framework::item::{ItemClass, ItemSearchOptions, Limit};

const OLD: &str = "dev.neo.plaid.volc";

pub fn run() {
    if !config::release() || config::data_dir().join("settings.json").exists() {
        return;
    }
    let settings = settings(OLD);
    let keys = keys();
    trace::log(&format!(
        "migrate from {OLD}: settings {settings}, keys {keys}"
    ));
}

/// Strings and numbers (bools included, as 0/1) from the old defaults domain; writes settings.json even when
/// there is nothing to import, so this runs once.
fn settings(domain: &str) -> String {
    let mut map = serde_json::Map::new();
    if let Some(d) =
        NSUserDefaults::standardUserDefaults().persistentDomainForName(&NSString::from_str(domain))
    {
        let (keys, values) = d.to_vecs();
        for (k, v) in keys.iter().zip(values) {
            let value = match v.downcast::<NSString>() {
                Ok(s) => serde_json::Value::from(s.to_string()),
                Err(v) => match v.downcast::<NSNumber>() {
                    Ok(n) if matches!(unsafe { *n.objCType().as_ptr() } as u8, b'f' | b'd') => {
                        serde_json::Value::from(n.doubleValue())
                    }
                    Ok(n) => serde_json::Value::from(n.longLongValue()),
                    Err(_) => continue,
                },
            };
            map.insert(k.to_string(), value);
        }
    }
    let n = map.len();
    let path = config::data_dir().join("settings.json");
    let written = std::fs::create_dir_all(config::data_dir())
        .and_then(|_| std::fs::write(&path, serde_json::to_vec_pretty(&map).unwrap_or_default()));
    match written {
        Ok(()) => n.to_string(),
        Err(e) => format!("not written: {e}"),
    }
}

/// Copies every account of the old Keychain service that the new one lacks. Listing reads attributes only; reading
/// each secret shows the system's Keychain dialog once, since the item's ACL names the old app.
fn keys() -> String {
    let found = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(OLD)
        .load_attributes(true)
        .limit(Limit::All)
        .search()
        .unwrap_or_default();
    let mut copied = vec![];
    for item in found {
        let Some(account) = item.simplify_dict().and_then(|d| d.get("acct").cloned()) else {
            continue;
        };
        if secret::get(&account).ok().flatten().is_some() {
            continue;
        }
        let Ok(value) = security_framework::passwords::get_generic_password(OLD, &account) else {
            continue;
        };
        if let Ok(v) = String::from_utf8(value)
            && secret::set(&account, &v).is_ok()
        {
            copied.push(account);
        }
    }
    copied.join(",")
}

#[cfg(test)]
mod tests {
    #[test]
    fn imports_defaults() {
        let domain = "com.neospaceindustries.plaid.migrate-test";
        let dir = std::env::temp_dir().join(domain);
        let _ = std::fs::remove_dir_all(&dir);
        let w = |args: &[&str]| {
            let ok = std::process::Command::new("defaults")
                .args(["write", domain])
                .args(args)
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "defaults write {args:?}");
        };
        w(&["holdKey", "-int", "61"]);
        w(&["llmEnabled", "-bool", "true"]);
        w(&["wakeWord", "-string", "Plaid"]);
        w(&["silenceRMS", "-float", "0.5"]);
        w(&["blob", "-data", "00ff"]);
        unsafe { std::env::set_var("PLAID_DATA_DIR", &dir) };
        let n = super::settings(domain);
        let _ = std::process::Command::new("defaults")
            .args(["delete", domain])
            .status();
        assert_eq!(n, "4");
        for (k, v) in [
            ("holdKey", "61"),
            ("llmEnabled", "1"),
            ("wakeWord", "Plaid"),
            ("silenceRMS", "0.5"),
        ] {
            assert_eq!(crate::config::value(k).as_deref(), Some(v), "{k}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
