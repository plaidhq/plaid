//! HTTP endpoints and the dictation websocket.
use crate::config::Config;
use crate::db::{self, Db, Device};
use crate::{llm, log};
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Json, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::Utc;
use plaid_core::cloud::{
    Account, Asr, Board, BoardJoin, ClientMsg, ErrorCode, Kind, Login, LoginReply, ServerMsg, Usage,
};
use plaid_core::dictation::rms;
use plaid_core::gate::Gate;
use plaid_core::hotwords::Snapshot;
use plaid_core::llm::Failure;
use plaid_core::records::chars;
use plaid_core::refine::Plan;
use plaid_core::session::{self, Event};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 100 ms of 16 kHz mono s16le: the gate's buffer.
const CHUNK: usize = 3200;
/// For `start` after the upgrade.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a closing connection waits for the client's Close.
const CLOSE_WAIT: Duration = Duration::from_secs(2);
/// A dictation connection is closed after this long, whatever state it is in.
const MAX_DICTATION: Duration = Duration::from_secs(600);

pub struct App {
    pub db: Db,
    pub cfg: Config,
    pub http: reqwest::Client,
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/v1/login", post(login))
        .route("/v1/account", get(account))
        .route("/v1/board", get(board).post(board_join))
        .route("/v1/ws", get(ws))
        .with_state(app)
}

fn bearer(h: &HeaderMap) -> Option<&str> {
    h.get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

fn authorize(app: &App, h: &HeaderMap) -> Result<Device, StatusCode> {
    let token = bearer(h).ok_or(StatusCode::UNAUTHORIZED)?;
    match app.db.auth(token, Utc::now()) {
        Ok(Some(d)) => Ok(d),
        Ok(None) => Err(StatusCode::UNAUTHORIZED),
        Err(e) => {
            log!("db error: {e}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn login(State(app): State<Arc<App>>, Json(req): Json<Login>) -> Response {
    match app.db.login(&req.code, &req.device, Utc::now()) {
        Ok(Ok((token, name))) => {
            log!("login user={name}");
            Json(LoginReply { token, name }).into_response()
        }
        Ok(Err(why)) => {
            log!("login refused: {why:?}");
            (StatusCode::UNAUTHORIZED, why.message()).into_response()
        }
        Err(e) => {
            log!("db error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn account(State(app): State<Arc<App>>, h: HeaderMap) -> Response {
    let d = match authorize(&app, &h) {
        Ok(d) => d,
        Err(s) => return s.into_response(),
    };
    match app
        .db
        .month(d.user_id, Utc::now())
        .and_then(|m| Ok((m, app.db.balance(d.user_id)?)))
    {
        Ok(((month_micros, month_ms), balance_micros)) => Json(Account {
            name: d.user,
            balance_micros,
            month_micros,
            month_ms,
        })
        .into_response(),
        Err(e) => {
            log!("db error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn board(State(app): State<Arc<App>>, h: HeaderMap, RawQuery(q): RawQuery) -> Response {
    let d = match authorize(&app, &h) {
        Ok(d) => d,
        Err(s) => return s.into_response(),
    };
    // `days=N`; anything else is the default week.
    let days = q
        .as_deref()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("days=")))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(7)
        .clamp(1, 90);
    match app.db.board(d.user_id, days, Utc::now()) {
        Ok((joined, rows)) => Json(Board {
            days,
            me: d.user,
            joined,
            rows,
        })
        .into_response(),
        Err(e) => {
            log!("db error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn board_join(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(req): Json<BoardJoin>,
) -> Response {
    let d = match authorize(&app, &h) {
        Ok(d) => d,
        Err(s) => return s.into_response(),
    };
    match app.db.set_board(d.user_id, req.join) {
        Ok(()) => {
            log!("board user={} join={}", d.user, req.join);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            log!("db error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn ws(State(app): State<Arc<App>>, h: HeaderMap, up: WebSocketUpgrade) -> Response {
    match authorize(&app, &h) {
        Ok(d) => up.on_upgrade(move |socket| request(app, d, socket)),
        Err(s) => s.into_response(),
    }
}

async fn say(ws: &mut WebSocket, m: &ServerMsg) -> bool {
    let j = serde_json::to_string(m).expect("json");
    ws.send(Message::Text(j.into())).await.is_ok()
}

async fn refuse(mut ws: WebSocket, code: ErrorCode, message: &str) {
    say(
        &mut ws,
        &ServerMsg::Error {
            code,
            message: message.into(),
        },
    )
    .await;
    close(&mut ws).await;
}

/// Sends Close and reads until the client's Close (or `CLOSE_WAIT`). Dropping the socket while audio frames sit
/// unread makes the OS answer with a TCP reset, and the client can lose the error or `done` just sent.
async fn close(ws: &mut WebSocket) {
    if ws.send(Message::Close(None)).await.is_err() {
        return;
    }
    let _ = tokio::time::timeout(CLOSE_WAIT, async {
        while let Some(Ok(m)) = ws.recv().await {
            if matches!(m, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

/// The first text frame, which must be `start`.
async fn read_start(ws: &mut WebSocket) -> Result<ClientMsg, &'static str> {
    loop {
        match tokio::time::timeout(START_TIMEOUT, ws.recv()).await {
            Err(_) => return Err("no start"),
            Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => return Err("closed"),
            Ok(Some(Ok(Message::Text(t)))) => {
                return serde_json::from_str(&t).map_err(|_| "bad start");
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// One request on one connection.
async fn request(app: Arc<App>, d: Device, mut ws: WebSocket) {
    let start = match read_start(&mut ws).await {
        Ok(ClientMsg::Start {
            kind,
            client,
            asr,
            text,
            refine,
        }) => (kind, client, asr, text, refine),
        Ok(ClientMsg::Finish) | Err(_) => {
            return refuse(ws, ErrorCode::BadRequest, "expected start").await;
        }
    };
    let (kind, client, asr, text, plan) = start;
    // Checked once, up front: the request that crosses zero finishes and leaves the balance a little below it.
    match app.db.balance(d.user_id) {
        Ok(b) if b <= 0 => {
            log!("quota user={} balance={b}", d.user);
            return refuse(ws, ErrorCode::Quota, "no credit left").await;
        }
        Ok(_) => {}
        Err(e) => {
            log!("db error: {e}");
            return refuse(ws, ErrorCode::Upstream, "database").await;
        }
    }
    let row = db::Usage {
        user_id: d.user_id,
        device_id: d.id,
        kind: kind.key().into(),
        client: client.chars().take(32).collect(),
        refine: "none".into(),
        ..db::Usage::default()
    };
    let r = Run { app, ws, row };
    match (kind, asr, text, plan) {
        (Kind::Dictate, Some(asr), _, plan) => r.dictate(asr, plan).await,
        (Kind::Refine | Kind::Learn, None, Some(text), Some(plan)) => r.text(text, plan).await,
        _ => refuse(r.ws, ErrorCode::BadRequest, "start does not fit its kind").await,
    }
}

struct Run {
    app: Arc<App>,
    ws: WebSocket,
    row: db::Usage,
}

/// How a request ended, for the usage row.
enum End {
    Done,
    Disconnected,
    Upstream(String),
}

impl Run {
    async fn dictate(mut self, asr: Asr, plan: Option<Plan>) {
        let started = Instant::now();
        let mut opts = session::Options::new(self.app.cfg.volc.key.clone());
        if let Some(u) = &self.app.cfg.volc.url {
            opts.url = u.clone();
        }
        if let Some(r) = &self.app.cfg.volc.resource {
            opts.resource = r.clone();
        }
        opts.nonstream = asr.nonstream;
        let (s, events, fut) = session::start(
            opts,
            Snapshot::new(&asr.hotwords, asr.threshold).boost(asr.boost.clone()),
        );
        tokio::spawn(fut);
        s.begin(asr.dialog, None);
        // Like the device: make sure the provider connection is open while the user speaks. On a warm pooled
        // connection this is one cheap round trip.
        if let (Some(_), Some(l)) = (&plan, &self.app.cfg.llm) {
            let (http, l) = (self.app.http.clone(), l.clone());
            tokio::spawn(async move { llm::warm(&http, &l).await });
        }

        let mut gate = Gate::default();
        let (mut pending, mut received) = (Vec::<u8>::new(), 0usize);
        let mut finished_at = None::<Instant>;
        let deadline = tokio::time::sleep(MAX_DICTATION);
        tokio::pin!(deadline);
        let outcome = loop {
            tokio::select! {
                m = self.ws.recv(), if finished_at.is_none() => match m {
                    Some(Ok(Message::Binary(b))) => {
                        received += b.len();
                        pending.extend_from_slice(&b);
                        while pending.len() >= CHUNK {
                            let chunk: Vec<u8> = pending.drain(..CHUNK).collect();
                            let r = rms(&chunk);
                            let out = gate.push(chunk, r);
                            if !out.is_empty() {
                                s.append(out);
                            }
                        }
                    }
                    Some(Ok(Message::Text(t))) => {
                        if matches!(serde_json::from_str(&t), Ok(ClientMsg::Finish)) {
                            if !pending.is_empty() {
                                let chunk = std::mem::take(&mut pending);
                                let r = rms(&chunk);
                                let out = gate.push(chunk, r);
                                if !out.is_empty() {
                                    s.append(out);
                                }
                            }
                            s.finish();
                            finished_at = Some(Instant::now());
                        }
                    }
                    Some(Ok(_)) => {}
                    None | Some(Err(_)) => break Err(End::Disconnected),
                },
                e = events.recv() => match e {
                    Ok(Event::Text(text)) => {
                        if !say(&mut self.ws, &ServerMsg::Partial { text }).await {
                            break Err(End::Disconnected);
                        }
                    }
                    Ok(Event::Done(Ok(text))) => break Ok(text),
                    Ok(Event::Done(Err(e))) => break Err(End::Upstream(e)),
                    Ok(_) => {}
                    Err(_) => break Err(End::Upstream("session ended".into())),
                },
                _ = &mut deadline => break Err(End::Disconnected),
            }
        };
        // Volc bills what it received, so the row is written on every exit from here.
        drop(s);
        self.row.received_ms = received as i64 / 32;
        self.row.sent_ms = gate.sent_bytes as i64 / 32;
        self.row.asr_wait_ms = finished_at.map(|t| t.elapsed().as_millis() as i64);
        let text = match outcome {
            Ok(t) => t,
            Err(end) => return self.end(end).await,
        };
        let llm = match (&plan, &self.app.cfg.llm) {
            (Some(p), Some(_)) => p.decide(&text).llm.ok().map(|user| (p, user)),
            _ => None,
        };
        if plan.is_some() {
            self.row.refine = "skipped".into();
        }
        self.row.chars = chars(&text) as i64;
        let msg = ServerMsg::Final {
            text,
            refining: llm.is_some(),
        };
        if !say(&mut self.ws, &msg).await {
            return self.end(End::Disconnected).await;
        }
        log!(
            "dictate device={} received={}ms sent={}ms asr_wait={:?}ms total={}ms",
            self.row.device_id,
            self.row.received_ms,
            self.row.sent_ms,
            self.row.asr_wait_ms,
            started.elapsed().as_millis()
        );
        let end = match llm {
            Some((p, user)) => self.refine(p, &user).await,
            None => End::Done,
        };
        self.end(end).await
    }

    async fn text(mut self, text: String, plan: Plan) {
        if self.app.cfg.llm.is_none() {
            return refuse(self.ws, ErrorCode::Upstream, "no LLM configured").await;
        }
        let user = plan.user_with(&text);
        let end = self.refine(&plan, &user).await;
        self.end(end).await
    }

    /// The LLM call, abandoned if the device goes away; sends `refined` or `refine_error`.
    async fn refine(&mut self, plan: &Plan, user: &str) -> End {
        let cfg = self.app.cfg.llm.as_ref().expect("checked");
        let timeout = Duration::from_millis(plan.timeout_ms).min(self.app.cfg.llm_timeout);
        self.row.llm_model = Some(cfg.endpoint.model.clone());
        let call = llm::send(&self.app.http, cfg, &plan.system, user, timeout);
        tokio::pin!(call);
        let reply = loop {
            tokio::select! {
                r = &mut call => break r,
                m = self.ws.recv() => match m {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => {
                        self.row.refine = "error".into();
                        return End::Disconnected;
                    }
                    Some(Ok(_)) => {}
                },
            }
        };
        self.row.llm_in = reply.tokens_in;
        self.row.llm_out = reply.tokens_out;
        self.row.llm_ms = Some(reply.ms);
        let msg = match reply.result {
            Ok(text) => {
                self.row.refine = "ok".into();
                if self.row.kind == Kind::Dictate.key() {
                    self.row.chars = chars(&text) as i64;
                }
                ServerMsg::Refined { text }
            }
            Err(Failure { message, timed_out }) => {
                self.row.refine = if timed_out { "timeout" } else { "error" }.into();
                // Provider bodies can echo request fragments; the device gets a short reason only.
                let message = if timed_out {
                    "LLM 超时".into()
                } else {
                    match message.split_once(':') {
                        Some((status, _)) if status.starts_with("HTTP ") => {
                            format!("LLM 服务出错（{status}）")
                        }
                        _ => "LLM 服务出错".into(),
                    }
                };
                ServerMsg::RefineError { message, timed_out }
            }
        };
        if say(&mut self.ws, &msg).await {
            End::Done
        } else {
            End::Disconnected
        }
    }

    /// Writes the usage row and, if the device is still there, `done` or the error.
    async fn end(mut self, end: End) {
        let now = Utc::now();
        let r = &mut self.row;
        r.cost_micros = self.app.cfg.prices.cost(r.sent_ms, r.llm_in, r.llm_out);
        r.error = match &end {
            End::Done => None,
            End::Disconnected => Some("disconnected".into()),
            End::Upstream(_) => Some("upstream".into()),
        };
        if let Err(e) = self.app.db.add_usage(r, now) {
            log!("db error: {e}");
        }
        log!(
            "end device={} kind={} refine={} error={:?} llm_in={} llm_out={} llm={:?}ms cost={}",
            r.device_id,
            r.kind,
            r.refine,
            r.error,
            r.llm_in,
            r.llm_out,
            r.llm_ms,
            r.cost_micros
        );
        match end {
            End::Done => {
                let balance_micros = self.app.db.balance(r.user_id).unwrap_or_default();
                let usage = Usage {
                    received_ms: r.received_ms,
                    sent_ms: r.sent_ms,
                    asr_wait_ms: r.asr_wait_ms.unwrap_or(0),
                    llm_ms: r.llm_ms.unwrap_or(0),
                    llm_in: r.llm_in,
                    llm_out: r.llm_out,
                    cost_micros: r.cost_micros,
                    balance_micros,
                };
                say(&mut self.ws, &ServerMsg::Done { usage }).await;
                close(&mut self.ws).await;
            }
            End::Upstream(e) => {
                // Volc's message carries a log id and a reason, not text.
                refuse(self.ws, ErrorCode::Upstream, &e).await;
            }
            End::Disconnected => {}
        }
    }
}

/// Serves until ctrl-c.
pub async fn serve(app: Arc<App>, listener: tokio::net::TcpListener) -> std::io::Result<()> {
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}
