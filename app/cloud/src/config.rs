//! Server settings, all from the environment. Keys never come from files or the database.
use plaid_core::llm::Endpoint;
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

/// Parses PLAID_LLM_PROXY. The port is required so the startup log can show where traffic goes.
pub fn proxy(v: &str) -> Result<reqwest::Url, String> {
    let bad = |why: &str| format!("PLAID_LLM_PROXY: {why} (expected socks5h://host:port)");
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

impl Config {
    pub fn from_env() -> Result<Config, String> {
        Self::from(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
    }

    pub fn from(env: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        let key = env("PLAID_VOLC_KEY").ok_or("PLAID_VOLC_KEY is not set")?;
        let llm_proxy = env("PLAID_LLM_PROXY").map(|v| proxy(&v)).transpose()?;
        let llm = match (env("PLAID_LLM_BASE"), env("PLAID_LLM_MODEL")) {
            (Some(base), Some(model)) => Some(Llm {
                endpoint: Endpoint::new(
                    &env("PLAID_LLM_FORMAT").unwrap_or_else(|| "openai".into()),
                    &base,
                    &model,
                ),
                key: env("PLAID_LLM_KEY"),
                extra: env("PLAID_LLM_EXTRA"),
                proxy: llm_proxy,
            }),
            (None, None) => None,
            _ => return Err("set both PLAID_LLM_BASE and PLAID_LLM_MODEL, or neither".into()),
        };
        let price = |k: &str| -> Result<f64, String> {
            env(k).map_or(Ok(0.), |v| {
                micros(&v)
                    .map(|m| m as f64)
                    .map_err(|e| format!("{k}: {e}"))
            })
        };
        let timeout = env("PLAID_LLM_TIMEOUT_MS")
            .map(|v| {
                v.parse::<u64>()
                    .map_err(|_| format!("PLAID_LLM_TIMEOUT_MS: {v}"))
            })
            .transpose()?
            .unwrap_or(8000);
        Ok(Config {
            volc: Volc {
                key,
                url: env("PLAID_VOLC_URL"),
                resource: env("PLAID_VOLC_RESOURCE"),
            },
            llm,
            llm_timeout: Duration::from_millis(timeout),
            prices: Prices {
                asr_hour: price("PLAID_PRICE_ASR_HOUR")?,
                llm_in: price("PLAID_PRICE_LLM_IN")?,
                llm_out: price("PLAID_PRICE_LLM_OUT")?,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_prices() {
        let env = |k: &str| match k {
            "PLAID_VOLC_KEY" => Some("k".to_string()),
            "PLAID_PRICE_ASR_HOUR" => Some("2.3".into()),
            "PLAID_PRICE_LLM_IN" => Some("4".into()),
            _ => None,
        };
        let c = Config::from(env).unwrap();
        assert!(c.llm.is_none());
        assert_eq!(c.llm_timeout, Duration::from_millis(8000));
        // 10 s of audio at 2.3/h, 1000 input tokens at 4/M.
        assert_eq!(c.prices.cost(10_000, 1000, 0), 6389 + 4000);
        assert!(Config::from(|k| (k == "PLAID_LLM_BASE").then(|| "x".into())).is_err());
        assert_eq!(micros("0.5"), Ok(500_000));
        assert!(micros("-1").is_err() && micros("abc").is_err());
    }

    #[test]
    fn reads_proxy() {
        let with = |p: &'static str| {
            Config::from(move |k: &str| match k {
                "PLAID_VOLC_KEY" | "PLAID_LLM_BASE" | "PLAID_LLM_MODEL" => Some("x".to_string()),
                "PLAID_LLM_PROXY" => Some(p.into()),
                _ => None,
            })
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
