//! The LLM call. One shared client keeps a warm HTTP/2 connection per provider: PINGs hold it open while idle and
//! there is no idle timeout, so a refine is one round trip without a TLS handshake.
use crate::config::Llm;
use crate::log;
use plaid_core::llm::{self, Failure};
use std::time::{Duration, Instant};

/// `proxy` (PLAID_LLM_PROXY) carries every LLM request; None keeps reqwest's default (direct, or the standard
/// proxy environment variables).
pub fn client(proxy: Option<&reqwest::Url>) -> reqwest::Client {
    let mut b = reqwest::Client::builder();
    if let Some(p) = proxy {
        b = b.proxy(reqwest::Proxy::all(p.clone()).expect("proxy url checked in config"));
    }
    b.pool_idle_timeout(None)
        .http2_keep_alive_interval(Duration::from_secs(20))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .tcp_keepalive(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .expect("http client")
}

/// Opens the connection so the next refine finds it warm. The response (usually 401 or 404) is dropped. Runs in the
/// background, so it may wait out a slow TLS handshake (a proxy can take seconds).
pub async fn warm(http: &reqwest::Client, cfg: &Llm) {
    let t = Instant::now();
    match http
        .get(&cfg.endpoint.base)
        .timeout(Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => log!(
            "llm warm {:?} status={} {}ms",
            r.version(),
            r.status().as_u16(),
            t.elapsed().as_millis()
        ),
        Err(e) => log!("llm warm failed: {e}"),
    }
}

pub struct Reply {
    pub result: Result<String, Failure>,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub ms: i64,
}

pub async fn send(
    http: &reqwest::Client,
    cfg: &Llm,
    system: &str,
    user: &str,
    timeout: Duration,
) -> Reply {
    let t = Instant::now();
    let reply = |result, tokens_in, tokens_out| Reply {
        result,
        tokens_in,
        tokens_out,
        ms: t.elapsed().as_millis() as i64,
    };
    let fail = |message: String, timed_out| Failure { message, timed_out };
    let req = match llm::request(
        &cfg.endpoint,
        cfg.key.as_deref(),
        system,
        user,
        cfg.extra.as_deref(),
        timeout,
    ) {
        Ok(r) => r,
        Err(e) => return reply(Err(fail(e, false)), 0, 0),
    };
    let mut b = http.post(&req.url).timeout(req.timeout).body(req.body);
    for (k, v) in &req.headers {
        b = b.header(*k, v);
    }
    let resp = match b.send().await {
        Ok(r) => r,
        Err(e) => return reply(Err(fail(e.to_string(), e.is_timeout())), 0, 0),
    };
    let (status, version) = (resp.status().as_u16(), resp.version());
    let body = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return reply(Err(fail(e.to_string(), e.is_timeout())), 0, 0),
    };
    let (i, o) = if status == 200 {
        llm::usage(req.anthropic, &body)
    } else {
        (0, 0)
    };
    let r = reply(
        llm::parse(req.anthropic, status, &body).map_err(|e| fail(e, false)),
        i as i64,
        o as i64,
    );
    log!("llm {version:?} status={status} in={i} out={o} {}ms", r.ms);
    r
}
