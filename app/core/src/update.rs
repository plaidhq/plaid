//! Update check against the GitHub releases API: the latest release's version, page and, for this platform,
//! the zip's URL and sha256 (GitHub's asset digest). Downloads are verified against that digest. Requests
//! follow the same proxy rules as the LLM calls.
use crate::llm::{self, SystemProxy};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

/// `PLAID_UPDATE_URL` overrides it (tests, a local mirror).
pub const LATEST: &str = "https://api.github.com/repos/plaidhq/plaid/releases/latest";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Without the leading "v".
    pub version: String,
    /// The release's web page.
    pub page: String,
    pub notes: String,
    /// The zip for this platform, when the release has one with a digest.
    pub asset: Option<Asset>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub url: String,
    /// Lowercase hex.
    pub sha256: String,
    pub size: u64,
}

/// The release asset name ends with this, e.g. "Plaid-v0.11.42-macos-arm64.zip".
pub fn asset_suffix() -> &'static str {
    if cfg!(windows) {
        "-windows-x64.zip"
    } else if cfg!(target_arch = "x86_64") {
        "-macos-x64.zip"
    } else {
        "-macos-arm64.zip"
    }
}

/// Reads a releases-API "latest release" object.
pub fn parse(json: &str, suffix: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().ok_or("no tag_name")?;
    let asset = v["assets"].as_array().and_then(|a| {
        a.iter().find_map(|a| {
            let name = a["name"].as_str()?;
            let sha256 = a["digest"].as_str()?.strip_prefix("sha256:")?;
            name.ends_with(suffix).then(|| Asset {
                url: a["browser_download_url"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                sha256: sha256.to_ascii_lowercase(),
                size: a["size"].as_u64().unwrap_or(0),
            })
        })
    });
    Ok(Release {
        version: tag.trim_start_matches('v').to_owned(),
        page: v["html_url"].as_str().unwrap_or_default().to_owned(),
        notes: v["body"].as_str().unwrap_or_default().to_owned(),
        asset: asset.filter(|a| !a.url.is_empty()),
    })
}

/// True when `candidate` is a strictly higher dotted version than `current` (numeric parts; "0.11.10" >
/// "0.11.9"). Anything unparsable is not newer.
pub fn newer(candidate: &str, current: &str) -> bool {
    let parts = |s: &str| -> Option<Vec<u64>> { s.split('.').map(|p| p.parse().ok()).collect() };
    match (parts(candidate), parts(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Blocking: fetches the latest release.
pub fn check(proxy: Option<&SystemProxy>) -> Result<Release, String> {
    let url = std::env::var("PLAID_UPDATE_URL").unwrap_or_else(|_| LATEST.into());
    let req = llm::AGENT
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", concat!("Plaid/", env!("CARGO_PKG_VERSION")));
    let mut resp = llm::route(req, &url, proxy, Duration::from_secs(15))
        .call()
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    parse(&body, asset_suffix())
}

/// Blocking: downloads the asset to `to` and checks its sha256; a mismatched or partial file is removed.
pub fn download(asset: &Asset, to: &Path, proxy: Option<&SystemProxy>) -> Result<(), String> {
    let result = (|| {
        let req = llm::AGENT
            .get(&asset.url)
            .header("User-Agent", concat!("Plaid/", env!("CARGO_PKG_VERSION")));
        let mut resp = llm::route(req, &asset.url, proxy, Duration::from_secs(600))
            .call()
            .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        let mut reader = resp.body_mut().with_config().limit(512 << 20).reader();
        let mut file = std::fs::File::create(to).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut buf = vec![0; 64 << 10];
        loop {
            let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        }
        let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != asset.sha256 {
            return Err(format!("sha256 mismatch: {got}"));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(to);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.11.10", "0.11.9"));
        assert!(newer("0.12.0", "0.11.42"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("0.11.42", "0.11.42"));
        assert!(!newer("0.11.41", "0.11.42"));
        assert!(!newer("0.11.x", "0.11.1"));
        assert!(!newer("", "0.11.1"));
    }

    #[test]
    fn parses_release() {
        let json = r#"{"tag_name":"v0.11.43","html_url":"https://github.com/plaidhq/plaid/releases/tag/v0.11.43",
            "body":"notes","assets":[
            {"name":"Plaid-v0.11.43-windows-x64.zip","digest":"sha256:AA","size":2,"browser_download_url":"https://x/w.zip"},
            {"name":"Plaid-v0.11.43-macos-arm64.zip","digest":"sha256:BB","size":3,"browser_download_url":"https://x/m.zip"}]}"#;
        let r = parse(json, "-macos-arm64.zip").unwrap();
        assert_eq!(r.version, "0.11.43");
        assert_eq!(r.notes, "notes");
        assert_eq!(
            r.asset,
            Some(Asset {
                url: "https://x/m.zip".into(),
                sha256: "bb".into(),
                size: 3
            })
        );
        // No digest (assets uploaded before GitHub computed them): no asset, so no in-app install.
        let old = r#"{"tag_name":"v1.0.0","assets":[{"name":"P-macos-arm64.zip","size":1,"browser_download_url":"u"}]}"#;
        assert_eq!(parse(old, "-macos-arm64.zip").unwrap().asset, None);
        assert!(parse(r#"{"message":"Not Found"}"#, "x").is_err());
    }

    /// Serves `body` once on a loopback port; returns the URL.
    fn serve(body: Vec<u8>) -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/a.zip", l.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut req = [0; 4096];
            let _ = s.read(&mut req);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            s.write_all(head.as_bytes()).unwrap();
            s.write_all(&body).unwrap();
        });
        url
    }

    #[test]
    fn download_checks_digest() {
        let dir = std::env::temp_dir().join(format!("plaid-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let to = dir.join("a.zip");
        let body = b"hello".to_vec();
        // sha256("hello")
        let sha = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let ok = Asset {
            url: serve(body.clone()),
            sha256: sha.into(),
            size: 5,
        };
        download(&ok, &to, None).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), body);
        let bad = Asset {
            url: serve(body),
            sha256: "00".into(),
            size: 5,
        };
        assert!(download(&bad, &to, None).unwrap_err().contains("mismatch"));
        assert!(!to.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
