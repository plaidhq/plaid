//! Server settings, from one TOML file (`--config`, default plaid-cloud.toml; example in
//! cloud/plaid-cloud.example.toml). Keys live only there, never in the database.
use plaid_core::llm::Endpoint;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone)]
pub struct Volc {
    pub key: String,
    pub url: Option<String>,
    pub resource: Option<String>,
}

#[derive(Clone)]
pub struct Llm {
    pub endpoint: Endpoint,
    pub key: Option<String>,
    /// JSON object merged over each request body (e.g. reasoning off).
    pub extra: Option<String>,
    /// Outbound proxy for LLM requests (socks5, socks5h, http, https). None: direct.
    pub proxy: Option<reqwest::Url>,
}

/// Parses llm.proxy. The port is required so the startup log can show where traffic goes.
pub fn proxy(v: &str) -> Result<reqwest::Url, String> {
    let bad = |why: &str| format!("llm.proxy: {why} (expected socks5h://host:port)");
    let url = reqwest::Url::parse(v.trim()).map_err(|_| bad("not a URL"))?;
    if !matches!(url.scheme(), "socks5" | "socks5h" | "http" | "https") {
        return Err(bad("scheme must be socks5, socks5h, http or https"));
    }
    if url.host_str().is_none() || url.port_or_known_default().is_none() {
        return Err(bad("missing host or port"));
    }
    Ok(url)
}

/// scheme://host:port, without credentials, for logs.
pub fn proxy_display(url: &reqwest::Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or_default()
    )
}

/// Millionths of a credit (plaid_core::cloud::CREDIT).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Prices {
    /// Per hour of audio sent to Volc.
    pub asr_hour: f64,
    /// Per million tokens.
    pub llm_in: f64,
    pub llm_out: f64,
}

impl Prices {
    pub fn cost(&self, sent_ms: i64, llm_in: i64, llm_out: i64) -> i64 {
        (sent_ms as f64 * self.asr_hour / 3_600_000.
            + llm_in as f64 * self.llm_in / 1e6
            + llm_out as f64 * self.llm_out / 1e6)
            .round() as i64
    }
}

#[derive(Clone)]
pub struct Config {
    pub volc: Volc,
    /// None: dictations are never refined.
    pub llm: Option<Llm>,
    /// Upper bound on a plan's timeout.
    pub llm_timeout: Duration,
    pub prices: Prices,
}

/// A non-negative amount as millionths.
pub fn micros(amount: &str) -> Result<i64, String> {
    let y: f64 = amount
        .trim()
        .parse()
        .map_err(|_| format!("not an amount: {amount}"))?;
    if !y.is_finite() || y < 0. {
        return Err(format!("not an amount: {amount}"));
    }
    Ok((y * 1e6).round() as i64)
}

/// The config file as written.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub listen: Option<String>,
    /// Relative to the config file's directory.
    pub db: Option<PathBuf>,
    pub volc: Option<FileVolc>,
    /// Absent: dictations are never refined.
    pub llm: Option<FileLlm>,
    #[serde(default)]
    pub prices: FilePrices,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileVolc {
    pub key: String,
    pub url: Option<String>,
    pub resource: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileLlm {
    pub format: Option<String>,
    pub base: String,
    pub model: String,
    pub key: Option<String>,
    /// Merged over each request body.
    pub extra: Option<serde_json::Map<String, serde_json::Value>>,
    pub timeout_ms: Option<u64>,
    pub proxy: Option<String>,
}

/// Credit per hour of audio and per million tokens; absent = 0.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FilePrices {
    #[serde(default)]
    pub asr_hour: f64,
    #[serde(default)]
    pub llm_in: f64,
    #[serde(default)]
    pub llm_out: f64,
}

/// The parsed file plus where it was read from, so relative paths resolve against it.
pub struct Settings {
    pub file: File,
    dir: PathBuf,
}

impl Settings {
    pub fn load(path: &Path) -> Result<Settings, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("config {}: {e}", path.display()))?;
        let file = Self::parse(&text).map_err(|e| format!("config {}: {e}", path.display()))?;
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        Ok(Settings { file, dir })
    }

    pub fn parse(text: &str) -> Result<File, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn listen(&self) -> String {
        self.file
            .listen
            .clone()
            .unwrap_or_else(|| "127.0.0.1:8787".into())
    }

    pub fn db(&self) -> PathBuf {
        let db = self
            .file
            .db
            .clone()
            .unwrap_or_else(|| "plaid-cloud.db".into());
        self.dir.join(db)
    }
}

fn price(name: &str, v: f64) -> Result<f64, String> {
    if !v.is_finite() || v < 0. {
        return Err(format!("prices.{name}: not an amount: {v}"));
    }
    Ok((v * 1e6).round())
}

fn set(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

impl Config {
    pub fn from(file: &File) -> Result<Config, String> {
        let volc = file.volc.as_ref().ok_or("[volc] is missing")?;
        let key = set(Some(volc.key.clone())).ok_or("volc.key is empty")?;
        let (llm, timeout) = match &file.llm {
            Some(l) => {
                if l.base.trim().is_empty() || l.model.trim().is_empty() {
                    return Err("llm.base and llm.model are required in [llm]".into());
                }
                let llm = Llm {
                    endpoint: Endpoint::new(
                        l.format.as_deref().unwrap_or("openai"),
                        &l.base,
                        &l.model,
                    ),
                    key: set(l.key.clone()),
                    extra: l
                        .extra
                        .as_ref()
                        .map(|m| serde_json::Value::Object(m.clone()).to_string()),
                    proxy: set(l.proxy.clone()).map(|v| proxy(&v)).transpose()?,
                };
                (Some(llm), l.timeout_ms.unwrap_or(8000))
            }
            None => (None, 8000),
        };
        let p = &file.prices;
        Ok(Config {
            volc: Volc {
                key,
                url: set(volc.url.clone()),
                resource: set(volc.resource.clone()),
            },
            llm,
            llm_timeout: Duration::from_millis(timeout),
            prices: Prices {
                asr_hour: price("asr_hour", p.asr_hour)?,
                llm_in: price("llm_in", p.llm_in)?,
                llm_out: price("llm_out", p.llm_out)?,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(text: &str) -> Result<Config, String> {
        Config::from(&Settings::parse(text)?)
    }

    #[test]
    fn reads_prices() {
        let c = cfg("[volc]\nkey = \"k\"\n[prices]\nasr_hour = 2.3\nllm_in = 4\n").unwrap();
        assert!(c.llm.is_none());
        assert_eq!(c.llm_timeout, Duration::from_millis(8000));
        // 10 s of audio at 2.3/h, 1000 input tokens at 4/M.
        assert_eq!(c.prices.cost(10_000, 1000, 0), 6389 + 4000);
        assert!(cfg("[volc]\nkey = \"k\"\n[prices]\nllm_in = -1\n").is_err());
        assert!(cfg("[volc]\nkey = \"\"\n").is_err());
        assert!(cfg("").is_err());
        assert!(cfg("[volc]\nkey = \"k\"\n[llm]\nbase = \"x\"\n").is_err());
        // A typo is an error, not a silently ignored setting.
        assert!(cfg("[volc]\nkey = \"k\"\n[prices]\nasr_hours = 1\n").is_err());
        assert_eq!(micros("0.5"), Ok(500_000));
        assert!(micros("-1").is_err() && micros("abc").is_err());
    }

    #[test]
    fn reads_llm() {
        let c = cfg(r#"
[volc]
key = "k"
[llm]
base = "https://api.example.com/v1"
model = "m"
timeout_ms = 5000
extra = { reasoning_effort = "none" }
"#)
        .unwrap();
        let l = c.llm.unwrap();
        assert_eq!(l.extra.as_deref(), Some(r#"{"reasoning_effort":"none"}"#));
        assert_eq!(c.llm_timeout, Duration::from_millis(5000));
        assert!(l.proxy.is_none() && l.key.is_none());
    }

    #[test]
    fn resolves_db_against_the_config_dir() {
        let s = Settings {
            file: Settings::parse("db = \"data/p.db\"").unwrap(),
            dir: "/etc/plaid".into(),
        };
        assert_eq!(s.db(), PathBuf::from("/etc/plaid/data/p.db"));
        let s = Settings {
            file: Settings::parse("db = \"/var/lib/p.db\"").unwrap(),
            dir: "/etc/plaid".into(),
        };
        assert_eq!(s.db(), PathBuf::from("/var/lib/p.db"));
        assert_eq!(s.listen(), "127.0.0.1:8787");
    }

    #[test]
    fn reads_proxy() {
        let with = |p: &str| {
            cfg(&format!(
                "[volc]\nkey = \"x\"\n[llm]\nbase = \"x\"\nmodel = \"x\"\nproxy = \"{p}\"\n"
            ))
        };
        let c = with("socks5h://user:secret@127.0.0.1:1080").unwrap();
        let url = c.llm.unwrap().proxy.unwrap();
        assert_eq!(proxy_display(&url), "socks5h://127.0.0.1:1080");
        assert_eq!(url.password(), Some("secret"));
        assert_eq!(
            proxy_display(&proxy("http://proxy.lan").unwrap()),
            "http://proxy.lan:80"
        );
        assert!(with("ftp://127.0.0.1:21").is_err());
        assert!(with("socks5://127.0.0.1").is_err());
        assert!(with("127.0.0.1:1080").is_err());
        let err = with("socks4://u:secret@h:1").err().unwrap();
        assert!(!err.contains("secret"), "{err}");
    }
}
