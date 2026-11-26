//! The server end to end with stand-ins for Volcengine and the LLM, driven by the app's own Cloud client.
use async_tungstenite::tungstenite::Message;
use axum::routing::post;
use futures_util::StreamExt;
use plaid_cloud::config::{Config, Llm, Prices, Volc};
use plaid_cloud::db::Db;
use plaid_cloud::server::{self, App};
use plaid_core::cloud::{self, Asr, Kind, Options};
use plaid_core::llm::{Endpoint, Level};
use plaid_core::refine::Plan;
use plaid_core::rules::Rules;
use plaid_core::session::Event;
use plaid_core::volc::{AUDIO_ONLY, frame};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const SERVER_FULL: u8 = 0b1001;
const SPOKEN: &str = "你好世界今天我们讨论一下发布计划";

fn result(text: &str, last: bool) -> Message {
    let json = format!(r#"{{"result":{{"text":"{text}"}}}}"#);
    Message::Binary(frame(SERVER_FULL, last, true, json.as_bytes()).into())
}

/// Volcengine: a partial after the first audio frame, the final after the last; counts audio bytes received.
fn mock_volc(audio: Arc<AtomicUsize>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for tcp in l.incoming() {
            let audio = audio.clone();
            std::thread::spawn(move || {
                async_io::block_on(async {
                    let tcp = async_io::Async::new(tcp.unwrap()).unwrap();
                    let mut ws = async_tungstenite::accept_async(tcp).await.unwrap();
                    let mut first = true;
                    while let Some(Ok(m)) = ws.next().await {
                        let Message::Binary(d) = m else { continue };
                        let (ty, last) = (d[1] >> 4, d[1] & 0b0010 != 0);
                        if ty != AUDIO_ONLY {
                            continue;
                        }
                        audio.fetch_add(
                            u32::from_be_bytes(d[4..8].try_into().unwrap()) as usize,
                            Ordering::SeqCst,
                        );
                        if last {
                            let _ = ws.send(result(&format!("{SPOKEN}。"), true)).await;
                            break;
                        }
                        if std::mem::take(&mut first) {
                            let _ = ws.send(result("你好", false)).await;
                        }
                    }
                    let _ = ws.close(None).await;
                })
            });
        }
    });
    url
}

/// An OpenAI-style endpoint: "润色：<user>" with token usage; a user message containing 慢 stalls.
async fn mock_llm(calls: Arc<AtomicUsize>) -> String {
    let app = axum::Router::new().route(
        "/chat/completions",
        post(move |axum::Json(v): axum::Json<serde_json::Value>| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(v["messages"][0]["content"], "S");
                let user = v["messages"][1]["content"].as_str().unwrap().to_string();
                if user.contains('慢') {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                axum::Json(serde_json::json!({
                    "choices": [{"message": {"content": format!("润色：{user}")}}],
                    "usage": {"prompt_tokens": 100, "completion_tokens": 20},
                }))
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    base
}

struct Fixture {
    base: String,
    db_path: std::path::PathBuf,
    app: Arc<App>,
    volc_audio: Arc<AtomicUsize>,
    llm_calls: Arc<AtomicUsize>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let p = self.db_path.to_str().unwrap().to_string();
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{p}{ext}"));
        }
    }
}

async fn fixture() -> Fixture {
    let volc_audio = Arc::new(AtomicUsize::new(0));
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let volc = mock_volc(volc_audio.clone());
    let llm = mock_llm(llm_calls.clone()).await;
    let db_path =
        std::env::temp_dir().join(format!("plaid-cloud-test-{}.db", uuid::Uuid::new_v4()));
    let app = Arc::new(App {
        db: Db::open(db_path.to_str().unwrap()).unwrap(),
        cfg: Config {
            volc: Volc {
                key: "mock".into(),
                url: Some(volc),
                resource: None,
            },
            llm: Some(Llm {
                endpoint: Endpoint::new("openai", &llm, "mock-model"),
                key: Some("k".into()),
                extra: None,
                proxy: None,
            }),
            llm_timeout: Duration::from_secs(8),
            prices: Prices {
                asr_hour: 2_300_000.,
                llm_in: 4_000_000.,
                llm_out: 8_000_000.,
            },
        },
        http: plaid_cloud::llm::client(None),
    });
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(server::serve(app.clone(), l));
    Fixture {
        base,
        db_path,
        app,
        volc_audio,
        llm_calls,
    }
}

impl Fixture {
    /// Signs a new device in through the HTTP endpoint, as the app does.
    /// `credit` in millionths.
    async fn token(&self, credit: i64) -> String {
        let code = self
            .app
            .db
            .invite("amy", Some(credit), chrono::Utc::now())
            .unwrap();
        let base = self.base.clone();
        tokio::task::spawn_blocking(move || cloud::login(&base, &code, "Test Mac").unwrap().token)
            .await
            .unwrap()
    }

    fn opts(&self, token: &str) -> Options {
        Options::new(&self.base, token.into(), "test")
    }

    /// Every text value stored, for the privacy check.
    fn stored_text(&self) -> String {
        let c = rusqlite::Connection::open(&self.db_path).unwrap();
        let mut out = String::new();
        for table in ["users", "devices", "invites", "usage"] {
            let mut q = c.prepare(&format!("SELECT * FROM {table}")).unwrap();
            let n = q.column_count();
            let mut rows = q.query([]).unwrap();
            while let Some(r) = rows.next().unwrap() {
                for i in 0..n {
                    if let Ok(Some(s)) = r.get::<_, Option<String>>(i) {
                        out.push_str(&s);
                        out.push('\n');
                    }
                }
            }
        }
        out
    }

    fn usage_rows(&self) -> Vec<(String, String, Option<String>, i64, i64)> {
        let c = rusqlite::Connection::open(&self.db_path).unwrap();
        let mut q = c
            .prepare("SELECT kind, refine, error, sent_ms, cost_micros FROM usage ORDER BY id")
            .unwrap();
        q.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }
}

fn plan(timeout_ms: u64) -> Plan {
    Plan {
        level: Level::Fluent,
        rules: Rules::default(),
        min_chars: 0,
        system: "S".into(),
        user: "U: {{text}}".into(),
        commands: vec![],
        wake: String::new(),
        timeout_ms,
    }
}

/// One second of a tone pulsing every 100 ms like syllables, in 100 ms buffers.
fn speech() -> Vec<Vec<u8>> {
    (0..10)
        .map(|b| {
            let amp = if b % 2 == 0 { 8000. } else { 100. };
            (0..1600)
                .flat_map(|i| {
                    let t = (b * 1600 + i) as f64;
                    (((t * 440. * std::f64::consts::TAU / 16000.).sin() * amp) as i16).to_le_bytes()
                })
                .collect()
        })
        .collect()
}

/// A whole dictation through the Cloud client: the partials, then the other events in order.
async fn dictate(opts: Options, plan: Option<Plan>) -> (Vec<String>, Vec<Event>) {
    let asr = Asr {
        dialog: vec![],
        hotwords: vec!["Plaid".into()],
        boost: vec![],
        threshold: 0.85,
        nonstream: false,
    };
    let (s, events, fut) = cloud::start(opts, asr);
    tokio::spawn(fut);
    s.begin(vec!["上一句".into()], plan);
    for b in speech() {
        s.append(b);
    }
    s.finish();
    let mut out = vec![];
    while let Ok(Ok(e)) = tokio::time::timeout(Duration::from_secs(10), events.recv()).await {
        let last = matches!(e, Event::Usage(_));
        out.push(e);
        if last {
            break;
        }
    }
    drop(s);
    let (texts, rest): (Vec<_>, Vec<_>) =
        out.into_iter().partition(|e| matches!(e, Event::Text(_)));
    let texts = texts
        .into_iter()
        .map(|e| match e {
            Event::Text(t) => t,
            _ => unreachable!(),
        })
        .collect();
    (texts, rest)
}

#[tokio::test(flavor = "multi_thread")]
async fn dictation_is_recognized_refined_and_billed_without_storing_text() {
    let f = fixture().await;
    let token = f.token(5_000_000).await;
    let (texts, events) = dictate(f.opts(&token), Some(plan(3000))).await;
    let final_text = format!("{SPOKEN}。");
    assert_eq!(texts[0], "你好");
    assert_eq!(events[0], Event::Done(Ok(final_text.clone())));
    assert_eq!(
        events[1],
        Event::Refined(Ok(format!("润色：U: {final_text}")))
    );
    let Event::Usage(u) = &events[2] else {
        panic!("{events:?}")
    };
    assert_eq!((u.llm_in, u.llm_out), (100, 20));
    assert_eq!(u.received_ms, 1000);
    assert!(u.sent_ms > 0 && u.sent_ms <= 1000, "{u:?}");
    assert!(
        u.cost_micros > 0 && u.balance_micros == 5_000_000 - u.cost_micros,
        "{u:?}"
    );
    assert!(f.volc_audio.load(Ordering::SeqCst) > 0);

    let rows = f.usage_rows();
    assert_eq!(
        rows,
        vec![(
            "dictate".into(),
            "ok".into(),
            None,
            u.sent_ms,
            u.cost_micros
        )]
    );
    let stored = f.stored_text();
    for t in ["你好", "润色", "上一句", "Plaid", "U: ", &token] {
        assert!(!stored.contains(t), "{t} stored:\n{stored}");
    }

    let (base, t) = (f.base.clone(), token.clone());
    let account = tokio::task::spawn_blocking(move || cloud::account(&base, &t).unwrap())
        .await
        .unwrap();
    assert_eq!(
        (
            account.name.as_str(),
            account.month_micros,
            account.balance_micros
        ),
        ("amy", u.cost_micros, u.balance_micros)
    );

    // 鲸喷榜 counts the refined text's characters; leaving takes the row off.
    let (base, t) = (f.base.clone(), token.clone());
    let (board, after) = tokio::task::spawn_blocking(move || {
        let b = cloud::board(&base, &t, 7).unwrap();
        cloud::board_join(&base, &t, false).unwrap();
        (b, cloud::board(&base, &t, 7).unwrap())
    })
    .await
    .unwrap();
    let refined = format!("润色：U: {final_text}");
    assert_eq!(
        (board.me.as_str(), board.joined, board.days),
        ("amy", true, 7)
    );
    assert_eq!(board.rows.len(), 1);
    assert_eq!(
        (
            board.rows[0].chars,
            board.rows[0].audio_ms,
            board.rows[0].dictations
        ),
        (plaid_core::records::chars(&refined) as i64, 1000, 1)
    );
    assert!(!after.joined && after.rows.is_empty(), "{after:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_plan_the_server_only_recognizes() {
    let f = fixture().await;
    let token = f.token(5_000_000).await;
    let (_, events) = dictate(f.opts(&token), None).await;
    assert_eq!(events[0], Event::Done(Ok(format!("{SPOKEN}。"))));
    // The client releases a device that might have expected a refine.
    assert!(matches!(&events[1], Event::Refined(Err(_))), "{events:?}");
    assert!(matches!(&events[2], Event::Usage(u) if u.llm_in == 0));
    assert_eq!(f.llm_calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.usage_rows()[0].1, "none");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_llm_fails_open_with_a_timeout() {
    let f = fixture().await;
    let token = f.token(5_000_000).await;
    let mut p = plan(300);
    p.user = "慢 {{text}}".into();
    let (_, events) = dictate(f.opts(&token), Some(p)).await;
    assert!(matches!(&events[0], Event::Done(Ok(_))));
    assert!(
        matches!(&events[1], Event::Refined(Err(e)) if e.timed_out),
        "{events:?}"
    );
    assert_eq!(f.usage_rows()[0].1, "timeout");
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_tokens_and_spent_credit_are_refused_before_upstream() {
    let f = fixture().await;
    let (_, events) = dictate(f.opts("plaid_nope"), Some(plan(3000))).await;
    assert_eq!(
        events,
        vec![Event::Done(Err(cloud::describe_error(
            cloud::ErrorCode::Auth,
            ""
        )))]
    );

    let token = f.token(0).await;
    let (_, events) = dictate(f.opts(&token), Some(plan(3000))).await;
    assert_eq!(
        events,
        vec![Event::Done(Err(cloud::describe_error(
            cloud::ErrorCode::Quota,
            ""
        )))]
    );
    assert_eq!(f.volc_audio.load(Ordering::SeqCst), 0);
    assert!(f.usage_rows().is_empty());

    // Any credit left lets a request start; it finishes below zero and the next one is refused.
    f.app.db.credit("amy", 1, chrono::Utc::now()).unwrap();
    let (_, events) = dictate(f.opts(&token), Some(plan(3000))).await;
    assert!(
        matches!(&events[2], Event::Usage(u) if u.balance_micros < 0),
        "{events:?}"
    );
    let (_, events) = dictate(f.opts(&token), Some(plan(3000))).await;
    assert_eq!(
        events,
        vec![Event::Done(Err(cloud::describe_error(
            cloud::ErrorCode::Quota,
            ""
        )))]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn text_refine_and_learn_use_the_plan_directly() {
    let f = fixture().await;
    let token = f.token(5_000_000).await;
    let r = cloud::refine(&f.opts(&token), Kind::Refine, "原文".into(), plan(3000)).await;
    assert_eq!(r, Ok("润色：U: 原文".into()));
    let r = cloud::refine(&f.opts(&token), Kind::Learn, "原文".into(), plan(3000)).await;
    assert_eq!(r, Ok("润色：U: 原文".into()));
    let rows = f.usage_rows();
    assert_eq!(
        (rows[0].0.as_str(), rows[1].0.as_str()),
        ("refine", "learn")
    );
    assert!(rows.iter().all(|r| r.1 == "ok" && r.3 == 0 && r.4 > 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_leaves_mid_refine_is_still_billed() {
    let f = fixture().await;
    let token = f.token(5_000_000).await;
    let mut p = plan(3000);
    p.user = "慢 {{text}}".into();
    let asr = Asr {
        threshold: 0.85,
        ..Asr::default()
    };
    let (s, events, fut) = cloud::start(f.opts(&token), asr);
    tokio::spawn(fut);
    s.begin(vec![], Some(p));
    for b in speech() {
        s.append(b);
    }
    s.finish();
    loop {
        if let Event::Done(r) = events.recv().await.unwrap() {
            assert!(r.is_ok());
            break;
        }
    }
    drop(s);
    for _ in 0..50 {
        if !f.usage_rows().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let rows = f.usage_rows();
    assert_eq!(
        (rows[0].1.as_str(), rows[0].2.as_deref()),
        ("error", Some("disconnected"))
    );
    assert!(rows[0].3 > 0 && rows[0].4 > 0, "{rows:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_login_says_why() {
    let f = fixture().await;
    let code = f.app.db.invite("amy", None, chrono::Utc::now()).unwrap();
    let base = f.base.clone();
    let (first, again, wrong) = tokio::task::spawn_blocking(move || {
        (
            cloud::login(&base, &code, "Mac").is_ok(),
            cloud::login(&base, &code, "Mac").unwrap_err(),
            cloud::login(&base, "ZZZZZ22222", "Mac").unwrap_err(),
        )
    })
    .await
    .unwrap();
    assert!(first);
    assert_eq!(again, "这个邀请码已经用过了");
    assert_eq!(wrong, "邀请码不对");
}

/// A minimal SOCKS5 server (no auth, CONNECT only) that counts the connections it relays.
async fn mock_socks5(conns: Arc<AtomicUsize>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("socks5h://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut c, _) = l.accept().await.unwrap();
            let conns = conns.clone();
            tokio::spawn(async move {
                let mut b = [0u8; 2];
                c.read_exact(&mut b).await.unwrap();
                let mut methods = vec![0u8; b[1] as usize];
                c.read_exact(&mut methods).await.unwrap();
                c.write_all(&[5, 0]).await.unwrap();
                let mut h = [0u8; 4];
                c.read_exact(&mut h).await.unwrap();
                assert_eq!(&h[..2], &[5, 1], "CONNECT");
                let host = match h[3] {
                    1 => {
                        let mut ip = [0u8; 4];
                        c.read_exact(&mut ip).await.unwrap();
                        std::net::Ipv4Addr::from(ip).to_string()
                    }
                    3 => {
                        let n = c.read_u8().await.unwrap();
                        let mut d = vec![0u8; n as usize];
                        c.read_exact(&mut d).await.unwrap();
                        String::from_utf8(d).unwrap()
                    }
                    t => panic!("address type {t}"),
                };
                let port = c.read_u16().await.unwrap();
                let mut up = tokio::net::TcpStream::connect((host.as_str(), port))
                    .await
                    .unwrap();
                c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
                conns.fetch_add(1, Ordering::SeqCst);
                let _ = tokio::io::copy_bidirectional(&mut c, &mut up).await;
            });
        }
    });
    url
}

#[tokio::test(flavor = "multi_thread")]
async fn llm_requests_go_through_the_configured_proxy() {
    let (calls, conns) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let base = mock_llm(calls.clone()).await;
    let proxy = plaid_cloud::config::proxy(&mock_socks5(conns.clone()).await).unwrap();
    let cfg = Llm {
        endpoint: Endpoint::new("openai", &base, "mock-model"),
        key: Some("k".into()),
        extra: None,
        proxy: Some(proxy.clone()),
    };
    let http = plaid_cloud::llm::client(Some(&proxy));
    let r = plaid_cloud::llm::send(&http, &cfg, "S", "你好", Duration::from_secs(5)).await;
    assert_eq!(r.result.unwrap(), "润色：你好");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(conns.load(Ordering::SeqCst), 1);
}
