//! Plaid Cloud: the protocol between the app and `plaid-cloud` (docs/cloud.md), and the app's side of it. A Cloud
//! dictation is a `Session` like a Volc one; its events add the server's refine (`Event::Refined`) and the cost
//! (`Event::Usage`).
use crate::llm::Failure;
use crate::refine::Plan;
use crate::session::{self, Cmd, Event, Session};
use async_channel::{Receiver, Sender};
use async_tungstenite::tungstenite::{self, Message, client::IntoClientRequest, http::HeaderValue};
use futures_util::{FutureExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Audio in, recognized (and maybe refined) text out.
    Dictate,
    /// Text through the LLM (重新润色).
    Refine,
    /// Text through the LLM for learning.
    Learn,
}

impl Kind {
    pub fn key(self) -> &'static str {
        match self {
            Kind::Dictate => "dictate",
            Kind::Refine => "refine",
            Kind::Learn => "learn",
        }
    }
}

/// What the server's Volc session needs from the device.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Asr {
    /// Context texts, newest first.
    #[serde(default)]
    pub dialog: Vec<String>,
    /// Hotword file lines (terms, `heard -> wanted` rewrites, sound-alike entries).
    #[serde(default)]
    pub hotwords: Vec<String>,
    pub threshold: f64,
    pub nonstream: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // one Start per request
pub enum ClientMsg {
    Start {
        kind: Kind,
        client: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asr: Option<Asr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default)]
        refine: Option<Plan>,
    },
    Finish,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    /// Audio received from the device and sent on to Volc after the server's gate.
    pub received_ms: i64,
    pub sent_ms: i64,
    /// `finish` → the final result.
    pub asr_wait_ms: i64,
    pub llm_ms: i64,
    pub llm_in: i64,
    pub llm_out: i64,
    /// In credit (see `CREDIT`).
    pub cost_micros: i64,
    /// After this request.
    pub balance_micros: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Auth,
    Quota,
    Upstream,
    BadRequest,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Partial { text: String },
    Final { text: String, refining: bool },
    Refined { text: String },
    RefineError { message: String, timed_out: bool },
    Done { usage: Usage },
    Error { code: ErrorCode, message: String },
}

/// What the user sees for a server error.
pub fn describe_error(code: ErrorCode, message: &str) -> String {
    match code {
        ErrorCode::Auth => "Cloud 登录已失效，请在 设置 › 服务 重新登录".into(),
        ErrorCode::Quota => format!("Cloud {CREDIT} 已用完，请联系管理员充值"),
        ErrorCode::Upstream => format!("Cloud 上游出错：{message}"),
        ErrorCode::BadRequest => format!("Cloud 拒绝了请求：{message}"),
    }
}

/// What a Cloud account's balance is called, in the app and in admin output. Amounts are stored as millionths of
/// one; the server's PLAID_PRICE_* are set in it.
pub const CREDIT: &str = "credit";

/// An amount with two decimals, three under 0.01, and the unit.
pub fn credits(micros: i64) -> String {
    let y = micros as f64 / 1e6;
    if micros != 0 && micros.abs() < 10_000 {
        format!("{y:.3} {CREDIT}")
    } else {
        format!("{y:.2} {CREDIT}")
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// http(s)://host[:port], no trailing path.
    pub base: String,
    pub token: String,
    /// App version, for the usage rows.
    pub client: String,
    /// Wait for the final after `finish`. Longer than the server's own wait for Volc, which answers first.
    pub final_timeout: Duration,
}

impl Options {
    pub fn new(base: &str, token: String, client: &str) -> Options {
        Options {
            base: base.trim().trim_end_matches('/').to_owned(),
            token,
            client: client.into(),
            final_timeout: Duration::from_secs(8),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn ws_request(&self) -> Result<tungstenite::handshake::client::Request, String> {
        let url = self.url("/v1/ws");
        let url = if let Some(rest) = url.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = url.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            return Err(format!(
                "Cloud 地址需以 http:// 或 https:// 开头：{}",
                self.base
            ));
        };
        let mut req = url.into_client_request().map_err(|e| e.to_string())?;
        let auth =
            HeaderValue::from_str(&format!("Bearer {}", self.token)).map_err(|e| e.to_string())?;
        req.headers_mut().insert("Authorization", auth);
        Ok(req)
    }
}

fn describe(e: tungstenite::Error) -> String {
    match e {
        tungstenite::Error::Http(r) if r.status() == 401 => describe_error(ErrorCode::Auth, ""),
        tungstenite::Error::Http(r) => format!("Cloud HTTP {}", r.status().as_u16()),
        e => format!("Cloud：{e}"),
    }
}

/// Where a Cloud dictation is; an error is reported as the event the controller still waits for.
#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Recognizing,
    Refining,
    /// Final (and refine) delivered; only `done` is left.
    Closing,
}

/// Opens the connection immediately; commands queue until it is up. `asr.dialog` is replaced by `begin`'s.
pub fn start(
    opts: Options,
    asr: Asr,
) -> (
    Session,
    Receiver<Event>,
    impl Future<Output = ()> + Send + 'static,
) {
    let (session, rx) = Session::channel();
    let (etx, erx) = async_channel::unbounded();
    let fut = async move {
        let mut stage = Stage::Recognizing;
        if let Err(e) = dictate(&opts, asr, &rx, &etx, &mut stage).await {
            let ev = match stage {
                Stage::Recognizing => Some(Event::Done(Err(e))),
                Stage::Refining => Some(Event::Refined(Err(Failure {
                    message: e,
                    timed_out: false,
                }))),
                Stage::Closing => None,
            };
            if let Some(ev) = ev {
                let _ = etx.send(ev).await;
            }
        }
    };
    (session, erx, fut)
}

type Ws = async_tungstenite::WebSocketStream<async_tungstenite::smol::ConnectStream>;

async fn send_json(ws: &mut Ws, m: &ClientMsg) -> Result<(), String> {
    let j = serde_json::to_string(m).map_err(|e| e.to_string())?;
    ws.send(Message::text(j)).await.map_err(describe)
}

/// The next server message; text frames only.
fn parse(m: Option<Result<Message, tungstenite::Error>>) -> Result<Option<ServerMsg>, String> {
    match m {
        None => Err("Cloud 连接已断开".into()),
        Some(Err(e)) => Err(describe(e)),
        Some(Ok(Message::Text(t))) => Ok(serde_json::from_str(&t).ok()),
        Some(Ok(Message::Close(_))) => Err("Cloud 连接已断开".into()),
        Some(Ok(_)) => Ok(None),
    }
}

async fn dictate(
    opts: &Options,
    mut asr: Asr,
    cmds: &Receiver<Cmd>,
    events: &Sender<Event>,
    stage: &mut Stage,
) -> Result<(), String> {
    let req = opts.ws_request()?;
    let (mut queued, mut finished) = (std::collections::VecDeque::new(), false);
    let mut deadline = None::<async_io::Timer>;
    let connect = async_tungstenite::smol::connect_async(req).fuse();
    futures_util::pin_mut!(connect);
    let (mut ws, _) = loop {
        futures_util::select! {
            r = connect => break r.map_err(describe)?,
            c = session::next_cmd(cmds, finished).fuse() => {
                if c.is_err() {
                    return Err("cancelled".into());
                }
                if matches!(c, Ok(Cmd::Finish)) {
                    finished = true;
                    deadline = Some(async_io::Timer::after(opts.final_timeout));
                }
                queued.push_back(c);
            }
            _ = session::until(&mut deadline).fuse() => return Err("timed out connecting".into()),
        }
    };

    let (mut begun, mut sent_finish) = (false, false);
    let mut held = Vec::<u8>::new();
    let mut latest = String::new();
    loop {
        // After the final only a dropped `Session` matters: it cancels the server's refine.
        let cmd = async {
            match queued.pop_front() {
                Some(c) => c,
                None if *stage == Stage::Closing => std::future::pending().await,
                None => cmds.recv().await,
            }
        };
        futures_util::select! {
            c = cmd.fuse() => match c {
                Err(_) => {
                    let _ = ws.close(None).await;
                    return if *stage == Stage::Recognizing { Err("cancelled".into()) } else { Ok(()) };
                }
                Ok(_) if sent_finish => {}
                Ok(Cmd::Begin(dialog, plan)) if !begun => {
                    begun = true;
                    asr.dialog = dialog;
                    begin(&mut ws, opts, &asr, plan.map(|p| *p), &mut held).await?;
                }
                Ok(Cmd::Begin(..)) => {}
                Ok(Cmd::Audio(pcm)) if begun => ws.send(Message::binary(pcm)).await.map_err(describe)?,
                Ok(Cmd::Audio(pcm)) => held.extend_from_slice(&pcm),
                Ok(Cmd::Finish) => {
                    if !begun {
                        begun = true;
                        asr.dialog.clear();
                        begin(&mut ws, opts, &asr, None, &mut held).await?;
                    }
                    sent_finish = true;
                    send_json(&mut ws, &ClientMsg::Finish).await?;
                    if deadline.is_none() {
                        deadline = Some(async_io::Timer::after(opts.final_timeout));
                    }
                }
            },
            m = ws.next().fuse() => {
                let Some(m) = parse(m)? else { continue };
                match m {
                    ServerMsg::Partial { text } => {
                        if *stage == Stage::Recognizing && text != latest {
                            latest = text.clone();
                            let _ = events.send(Event::Text(text)).await;
                        }
                    }
                    ServerMsg::Final { text, refining } => {
                        let _ = events.send(Event::Done(Ok(text))).await;
                        if refining {
                            // The controller keeps its own deadline from the plan.
                            *stage = Stage::Refining;
                            deadline = None;
                        } else {
                            // A plan the device expected refined but the server skipped must not leave it waiting.
                            let _ = events.send(Event::Refined(Err(Failure {
                                message: "Cloud did not refine".into(),
                                timed_out: false,
                            }))).await;
                            *stage = Stage::Closing;
                            deadline = Some(async_io::Timer::after(Duration::from_secs(5)));
                        }
                    }
                    ServerMsg::Refined { text } if *stage == Stage::Refining => {
                        let _ = events.send(Event::Refined(Ok(text))).await;
                        *stage = Stage::Closing;
                        deadline = Some(async_io::Timer::after(Duration::from_secs(5)));
                    }
                    ServerMsg::RefineError { message, timed_out } if *stage == Stage::Refining => {
                        let _ = events.send(Event::Refined(Err(Failure { message, timed_out }))).await;
                        *stage = Stage::Closing;
                        deadline = Some(async_io::Timer::after(Duration::from_secs(5)));
                    }
                    ServerMsg::Refined { .. } | ServerMsg::RefineError { .. } => {}
                    ServerMsg::Done { usage } => {
                        let _ = events.send(Event::Usage(usage)).await;
                        let _ = ws.close(None).await;
                        return Ok(());
                    }
                    ServerMsg::Error { code, message } => {
                        let _ = ws.close(None).await;
                        return Err(describe_error(code, &message));
                    }
                }
            },
            _ = session::until(&mut deadline).fuse() => {
                let _ = ws.close(None).await;
                return match *stage {
                    Stage::Recognizing => Err("timed out waiting for final result".into()),
                    _ => Ok(()),
                };
            },
        }
    }
}

async fn begin(
    ws: &mut Ws,
    opts: &Options,
    asr: &Asr,
    plan: Option<Plan>,
    held: &mut Vec<u8>,
) -> Result<(), String> {
    let start = ClientMsg::Start {
        kind: Kind::Dictate,
        client: opts.client.clone(),
        asr: Some(asr.clone()),
        text: None,
        refine: plan,
    };
    send_json(ws, &start).await?;
    if !held.is_empty() {
        ws.send(Message::binary(std::mem::take(held)))
            .await
            .map_err(describe)?;
    }
    Ok(())
}

/// `text` through the server's LLM (`Kind::Refine` or `Kind::Learn`). Gives up at the plan's timeout + 3 s.
pub async fn refine(
    opts: &Options,
    kind: Kind,
    text: String,
    plan: Plan,
) -> Result<String, Failure> {
    let fail = |message: String| Failure {
        message,
        timed_out: false,
    };
    let limit = Duration::from_millis(plan.timeout_ms) + Duration::from_secs(3);
    let work = async {
        let req = opts.ws_request().map_err(fail)?;
        let (mut ws, _) = async_tungstenite::smol::connect_async(req)
            .await
            .map_err(|e| fail(describe(e)))?;
        let start = ClientMsg::Start {
            kind,
            client: opts.client.clone(),
            asr: None,
            text: Some(text),
            refine: Some(plan),
        };
        send_json(&mut ws, &start).await.map_err(fail)?;
        let mut result = None;
        loop {
            let m = match parse(ws.next().await) {
                Ok(m) => m,
                // Done may never come after the result; the result counts.
                Err(e) => return result.unwrap_or(Err(fail(e))),
            };
            match m {
                Some(ServerMsg::Refined { text }) => result = Some(Ok(text)),
                Some(ServerMsg::RefineError { message, timed_out }) => {
                    result = Some(Err(Failure { message, timed_out }))
                }
                Some(ServerMsg::Error { code, message }) => {
                    return Err(fail(describe_error(code, &message)));
                }
                Some(ServerMsg::Done { .. }) => {
                    let _ = ws.close(None).await;
                    return result.unwrap_or_else(|| Err(fail("Cloud 没有返回结果".into())));
                }
                _ => {}
            }
        }
    };
    futures_util::select! {
        r = work.fuse() => r,
        _ = FutureExt::fuse(async_io::Timer::after(limit)) => Err(Failure { message: "Cloud timed out".into(), timed_out: true }),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    pub balance_micros: i64,
    /// Spent this month.
    pub month_micros: i64,
    /// Audio sent to Volc this month.
    pub month_ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Login {
    pub code: String,
    pub device: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginReply {
    pub token: String,
    pub name: String,
}

fn http_err(e: ureq::Error) -> String {
    format!("连不上 Cloud：{e}")
}

/// Exchanges an invite code for a device token. Blocking.
pub fn login(base: &str, code: &str, device: &str) -> Result<LoginReply, String> {
    let opts = Options::new(base, String::new(), "");
    if !opts.base.starts_with("http://") && !opts.base.starts_with("https://") {
        return Err("Cloud 地址需以 http:// 或 https:// 开头".into());
    }
    let body = serde_json::to_vec(&Login {
        code: code.trim().into(),
        device: device.into(),
    })
    .map_err(|e| e.to_string())?;
    let mut resp = crate::llm::AGENT
        .post(&opts.url("/v1/login"))
        .header("Content-Type", "application/json")
        .config()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .send(&body[..])
        .map_err(http_err)?;
    let status = resp.status().as_u16();
    let body = resp.body_mut().read_to_vec().map_err(http_err)?;
    match status {
        200 => serde_json::from_slice(&body).map_err(|e| format!("Cloud 回复无法解析：{e}")),
        // The server says why (unknown, used, expired, disabled).
        401 => Err(Some(
            String::from_utf8_lossy(&body)
                .trim()
                .chars()
                .take(80)
                .collect::<String>(),
        )
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "邀请码不可用".into())),
        404 => Err("这个地址不是 Plaid Cloud".into()),
        s => Err(format!("Cloud HTTP {s}")),
    }
}

/// The signed-in user's balance and this month's usage. Blocking.
pub fn account(base: &str, token: &str) -> Result<Account, String> {
    let opts = Options::new(base, token.into(), "");
    let mut resp = crate::llm::AGENT
        .get(&opts.url("/v1/account"))
        .header("Authorization", &format!("Bearer {token}"))
        .config()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .call()
        .map_err(http_err)?;
    match resp.status().as_u16() {
        200 => {}
        401 => return Err(describe_error(ErrorCode::Auth, "")),
        s => return Err(format!("Cloud HTTP {s}")),
    }
    let body = resp.body_mut().read_to_vec().map_err(http_err)?;
    serde_json::from_slice(&body).map_err(|e| format!("Cloud 回复无法解析：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Level;
    use crate::rules::Rules;

    fn plan() -> Plan {
        Plan {
            level: Level::Fluent,
            rules: Rules::default(),
            min_chars: 0,
            system: "S".into(),
            user: "{{text}}".into(),
            commands: vec![],
            wake: String::new(),
            timeout_ms: 1000,
        }
    }

    #[test]
    fn messages_have_the_documented_shape() {
        let j = serde_json::to_value(ClientMsg::Start {
            kind: Kind::Dictate,
            client: "0.11.61".into(),
            asr: Some(Asr::default()),
            text: None,
            refine: None,
        })
        .unwrap();
        assert_eq!(j["type"], "start");
        assert_eq!(j["kind"], "dictate");
        assert!(j.get("text").is_none() && j["refine"].is_null());
        assert_eq!(
            serde_json::to_value(ClientMsg::Finish).unwrap(),
            serde_json::json!({"type": "finish"})
        );
        let m: ServerMsg =
            serde_json::from_str(r#"{"type":"refine_error","message":"x","timed_out":true}"#)
                .unwrap();
        assert_eq!(
            m,
            ServerMsg::RefineError {
                message: "x".into(),
                timed_out: true
            }
        );
        let m: ServerMsg =
            serde_json::from_str(r#"{"type":"error","code":"bad_request","message":""}"#).unwrap();
        assert!(matches!(
            m,
            ServerMsg::Error {
                code: ErrorCode::BadRequest,
                ..
            }
        ));
    }

    #[test]
    fn credits_keep_small_amounts_visible() {
        assert_eq!(credits(0), format!("0.00 {CREDIT}"));
        assert_eq!(credits(1_234), format!("0.001 {CREDIT}"));
        assert_eq!(credits(-12_345_678), format!("-12.35 {CREDIT}"));
    }

    /// A scripted server: the test gets its socket.
    fn server() -> (String, std::net::TcpListener) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        (format!("http://{}", l.local_addr().unwrap()), l)
    }

    fn accept(
        l: &std::net::TcpListener,
    ) -> async_tungstenite::WebSocketStream<async_io::Async<std::net::TcpStream>> {
        let (tcp, _) = l.accept().unwrap();
        async_io::block_on(async_tungstenite::accept_async(
            async_io::Async::new(tcp).unwrap(),
        ))
        .unwrap()
    }

    fn say(
        ws: &mut async_tungstenite::WebSocketStream<async_io::Async<std::net::TcpStream>>,
        m: ServerMsg,
    ) {
        async_io::block_on(ws.send(Message::text(serde_json::to_string(&m).unwrap()))).unwrap();
    }

    fn next_json(
        ws: &mut async_tungstenite::WebSocketStream<async_io::Async<std::net::TcpStream>>,
    ) -> ClientMsg {
        loop {
            match async_io::block_on(ws.next()).unwrap().unwrap() {
                Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                _ => continue,
            }
        }
    }

    fn recv(events: &Receiver<Event>) -> Event {
        async_io::block_on(async {
            futures_util::select! {
                e = events.recv().fuse() => e.unwrap(),
                _ = FutureExt::fuse(async_io::Timer::after(Duration::from_secs(5))) => panic!("no event"),
            }
        })
    }

    #[test]
    fn dictation_streams_audio_then_reports_final_refined_and_usage() {
        let (base, l) = server();
        let (s, events, fut) = start(Options::new(&base, "t".into(), "test"), Asr::default());
        let t = std::thread::spawn(move || async_io::block_on(fut));
        // Audio before begin is held and goes right after `start`.
        s.append(vec![1; 3200]);
        s.begin(vec!["ctx".into()], Some(plan()));
        let mut ws = accept(&l);
        let ClientMsg::Start {
            kind, asr, refine, ..
        } = next_json(&mut ws)
        else {
            panic!()
        };
        assert_eq!(
            (kind, asr.unwrap().dialog, refine),
            (Kind::Dictate, vec!["ctx".to_string()], Some(plan()))
        );
        let Message::Binary(b) = async_io::block_on(ws.next()).unwrap().unwrap() else {
            panic!()
        };
        assert_eq!(b.len(), 3200);
        say(&mut ws, ServerMsg::Partial { text: "你".into() });
        assert_eq!(recv(&events), Event::Text("你".into()));
        s.finish();
        assert_eq!(next_json(&mut ws), ClientMsg::Finish);
        say(
            &mut ws,
            ServerMsg::Final {
                text: "你好".into(),
                refining: true,
            },
        );
        assert_eq!(recv(&events), Event::Done(Ok("你好".into())));
        say(
            &mut ws,
            ServerMsg::Refined {
                text: "你好。".into(),
            },
        );
        assert_eq!(recv(&events), Event::Refined(Ok("你好。".into())));
        say(
            &mut ws,
            ServerMsg::Done {
                usage: Usage {
                    cost_micros: 7,
                    ..Usage::default()
                },
            },
        );
        assert!(matches!(
            recv(&events),
            Event::Usage(Usage { cost_micros: 7, .. })
        ));
        t.join().unwrap();
        drop(s);
    }

    /// Fail-open: a socket that drops during the refine reports a refine failure, not a lost dictation.
    #[test]
    fn a_drop_during_refine_is_a_refine_failure() {
        let (base, l) = server();
        let (s, events, fut) = start(Options::new(&base, "t".into(), "test"), Asr::default());
        let t = std::thread::spawn(move || async_io::block_on(fut));
        s.begin(vec![], Some(plan()));
        s.finish();
        let mut ws = accept(&l);
        next_json(&mut ws);
        say(
            &mut ws,
            ServerMsg::Final {
                text: "原文".into(),
                refining: true,
            },
        );
        assert_eq!(recv(&events), Event::Done(Ok("原文".into())));
        drop(ws);
        assert!(matches!(recv(&events), Event::Refined(Err(_))));
        t.join().unwrap();
    }

    /// Dropping the session after the final while the server refines closes the socket (aborting the LLM call).
    #[test]
    fn dropping_the_session_during_refine_closes_the_socket() {
        let (base, l) = server();
        let (s, events, fut) = start(Options::new(&base, "t".into(), "test"), Asr::default());
        let t = std::thread::spawn(move || async_io::block_on(fut));
        s.begin(vec![], Some(plan()));
        s.finish();
        let mut ws = accept(&l);
        next_json(&mut ws);
        say(
            &mut ws,
            ServerMsg::Final {
                text: "原文".into(),
                refining: true,
            },
        );
        assert_eq!(recv(&events), Event::Done(Ok("原文".into())));
        drop(s);
        let closed = async_io::block_on(async {
            loop {
                match ws.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break true,
                    _ => continue,
                }
            }
        });
        assert!(closed);
        t.join().unwrap();
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_server_error_before_the_final_ends_the_dictation() {
        let (base, l) = server();
        let (s, events, fut) = start(Options::new(&base, "t".into(), "test"), Asr::default());
        let t = std::thread::spawn(move || async_io::block_on(fut));
        s.begin(vec![], None);
        let mut ws = accept(&l);
        next_json(&mut ws);
        say(
            &mut ws,
            ServerMsg::Error {
                code: ErrorCode::Quota,
                message: String::new(),
            },
        );
        assert_eq!(
            recv(&events),
            Event::Done(Err(describe_error(ErrorCode::Quota, "")))
        );
        t.join().unwrap();
    }

    #[test]
    fn a_skipped_refine_releases_a_waiting_device() {
        let (base, l) = server();
        let (s, events, fut) = start(Options::new(&base, "t".into(), "test"), Asr::default());
        let t = std::thread::spawn(move || async_io::block_on(fut));
        s.begin(vec![], Some(plan()));
        s.finish();
        let mut ws = accept(&l);
        next_json(&mut ws);
        say(
            &mut ws,
            ServerMsg::Final {
                text: "短".into(),
                refining: false,
            },
        );
        assert_eq!(recv(&events), Event::Done(Ok("短".into())));
        assert!(matches!(recv(&events), Event::Refined(Err(_))));
        say(
            &mut ws,
            ServerMsg::Done {
                usage: Usage::default(),
            },
        );
        assert!(matches!(recv(&events), Event::Usage(_)));
        t.join().unwrap();
    }

    #[test]
    fn text_refine_returns_the_result_and_waits_for_done() {
        let (base, l) = server();
        let opts = Options::new(&base, "t".into(), "test");
        let t = std::thread::spawn(move || {
            async_io::block_on(refine(&opts, Kind::Learn, "原文".into(), plan()))
        });
        let mut ws = accept(&l);
        let ClientMsg::Start { kind, text, .. } = next_json(&mut ws) else {
            panic!()
        };
        assert_eq!((kind, text.as_deref()), (Kind::Learn, Some("原文")));
        say(
            &mut ws,
            ServerMsg::Refined {
                text: "改后".into(),
            },
        );
        say(
            &mut ws,
            ServerMsg::Done {
                usage: Usage::default(),
            },
        );
        assert_eq!(t.join().unwrap(), Ok("改后".into()));
    }
}
