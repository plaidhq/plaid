//! The built binary's `--file` replay against a local stand-in for the Volcengine endpoint: WAV → session →
//! controller → paste and record, with no key and no network. Runs on every platform CI builds.
use async_tungstenite::tungstenite::Message;
use futures_util::StreamExt;
use plaid_core::volc::{AUDIO_ONLY, FULL_REQUEST, frame};
use std::net::TcpListener;
use std::process::Command;

const SERVER_FULL: u8 = 0b1001;

/// One second of a 440 Hz tone pulsing every 100 ms like syllables (a steady tone reads as noise to the gate),
/// 16 kHz mono 16-bit.
fn wav() -> Vec<u8> {
    let pcm: Vec<u8> = (0..16000)
        .flat_map(|i| {
            let amp = if i / 1600 % 2 == 0 { 8000. } else { 100. };
            let s = (i as f64 * 440. * std::f64::consts::TAU / 16000.).sin() * amp;
            (s as i16).to_le_bytes()
        })
        .collect();
    let mut w = b"RIFF".to_vec();
    w.extend((36 + pcm.len() as u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes()); // PCM
    w.extend(1u16.to_le_bytes()); // mono
    w.extend(16000u32.to_le_bytes());
    w.extend(32000u32.to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend((pcm.len() as u32).to_le_bytes());
    w.extend(pcm);
    w
}

fn result(text: &str, last: bool) -> Message {
    let json = format!(r#"{{"result":{{"text":"{text}"}}}}"#);
    Message::Binary(frame(SERVER_FULL, last, true, json.as_bytes()).into())
}

/// Accepts one utterance: answers a partial after the first audio frame and the final after the last-flagged one.
/// Returns (first frame type, audio bytes received).
fn serve(l: TcpListener) -> (u8, usize) {
    serve_text(l, "你好世界。")
}

fn serve_text(l: TcpListener, text: &str) -> (u8, usize) {
    let (tcp, _) = l.accept().unwrap();
    async_io::block_on(async {
        let tcp = async_io::Async::new(tcp).unwrap();
        let mut ws = async_tungstenite::accept_async(tcp).await.unwrap();
        let (mut first, mut audio) = (None, 0);
        while let Some(Ok(m)) = ws.next().await {
            let Message::Binary(d) = m else { continue };
            let (ty, last) = (d[1] >> 4, d[1] & 0b0010 != 0);
            first.get_or_insert(ty);
            if ty != AUDIO_ONLY {
                continue;
            }
            audio += u32::from_be_bytes(d[4..8].try_into().unwrap()) as usize;
            if last {
                ws.send(result(text, true)).await.unwrap();
                break;
            }
            if audio == d.len() - 8 {
                ws.send(result("你好", false)).await.unwrap();
            }
        }
        let _ = ws.close(None).await;
        (first.unwrap_or(0), audio)
    })
}

#[test]
fn replay_against_mock_endpoint() {
    let dir = std::env::temp_dir().join(format!("plaid-replay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wav_path = dir.join("tone.wav");
    std::fs::write(&wav_path, wav()).unwrap();

    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let server = std::thread::spawn(move || serve(l));

    let out = Command::new(env!("CARGO_BIN_EXE_plaid"))
        .arg("--file")
        .arg(&wav_path)
        .env("PLAID_DATA_DIR", dir.join("data"))
        .env("PLAID_URL", url)
        .env("PLAID_API_KEY", "mock")
        .env("PLAID_AUTO_LEARN", "0")
        .env("PLAID_LLM_ENABLED", "0")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");

    let (first, audio) = server.join().unwrap();
    assert_eq!(first, FULL_REQUEST, "full request goes first");
    assert!(
        audio >= 32000,
        "one second of audio at least, got {audio} bytes"
    );
    assert!(
        stdout.lines().any(|l| l == "paste: 你好世界。"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("record: outcome=pasted"),
        "stdout:\n{stdout}"
    );
    let records = std::fs::read_to_string(dir.join("data/records.jsonl")).unwrap();
    assert!(records.contains("你好世界"), "{records}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An OpenAI-style endpoint that answers every request with the same polished text.
async fn mock_llm() -> String {
    let app = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(|| async {
            axum::Json(serde_json::json!({
                "choices": [{"message": {"content": "你好世界，今天我们讨论一下下周的发布计划和测试安排。"}}],
                "usage": {"prompt_tokens": 100, "completion_tokens": 20},
            }))
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    base
}

/// `--file` in Cloud mode: the binary signs nothing itself; it streams to the real server (in-process here), which
/// recognizes with the Volcengine stand-in, refines with the LLM stand-in, and bills the device.
#[test]
fn cloud_replay_against_the_real_server() {
    use plaid_cloud::config::{Config, Llm, Prices, Volc};
    use plaid_cloud::db::Db;
    use plaid_cloud::server::{self, App};
    use plaid_core::llm::Endpoint;
    use std::sync::Arc;

    let dir = std::env::temp_dir().join(format!("plaid-cloud-replay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wav_path = dir.join("tone.wav");
    std::fs::write(&wav_path, wav()).unwrap();

    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let volc = format!("ws://{}", l.local_addr().unwrap());
    let volc_thread = std::thread::spawn(move || {
        serve_text(l, "你好世界今天我们讨论一下下周的发布计划和测试安排。")
    });

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (app, base) = rt.block_on(async {
        let llm = mock_llm().await;
        let app = Arc::new(App {
            db: Db::open(dir.join("cloud.db").to_str().unwrap()).unwrap(),
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
                llm_timeout: std::time::Duration::from_secs(8),
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
        (app, base)
    });
    let code = app
        .db
        .invite("amy", Some(1_000_000), chrono::Utc::now())
        .unwrap();
    let token = plaid_core::cloud::login(&base, &code, "CI").unwrap().token;

    let out = Command::new(env!("CARGO_BIN_EXE_plaid"))
        .arg("--file")
        .arg(&wav_path)
        .env("PLAID_DATA_DIR", dir.join("data"))
        .env("PLAID_SERVICE_MODE", "cloud")
        .env("PLAID_CLOUD_URL", &base)
        .env("PLAID_CLOUD_TOKEN", &token)
        .env("PLAID_LLM_ENABLED", "1")
        .env("PLAID_AUTO_LEARN", "0")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    let (_, audio) = volc_thread.join().unwrap();
    assert!(audio > 0, "audio reached Volcengine");
    assert!(
        stdout
            .lines()
            .any(|l| l == "paste: 你好世界，今天我们讨论一下下周的发布计划和测试安排。"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let report = plaid_cloud::admin::run(&app.db, &["usage".into()]).unwrap();
    assert!(report.contains("amy\trequests=1\terrors=0"), "{report}");
    assert!(report.contains("llm_in=100\tllm_out=20"), "{report}");
    drop(rt);
    let _ = std::fs::remove_dir_all(&dir);
}
