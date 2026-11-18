// Volcengine bigmodel streaming ASR over WebSocket, ported from Sources/Plaid/Volc.swift.
// Frames: 4-byte header | [seq/event if flagged] | u32 size | payload, all big-endian.

use futures_util::{SinkExt, StreamExt};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http::HeaderValue};

pub const FULL_REQUEST: u8 = 0b0001;
pub const AUDIO_ONLY: u8 = 0b0010;
const SERVER_FULL: u8 = 0b1001;
const SERVER_ERROR: u8 = 0b1111;
/// 200 ms of 16 kHz mono s16le, the packet size the docs recommend.
pub const PACKET: usize = 6400;

pub fn frame(kind: u8, last: bool, json: bool, payload: &[u8]) -> Vec<u8> {
    let mut d = vec![0x11, kind << 4 | if last { 0b0010 } else { 0 }, if json { 0x10 } else { 0 }, 0];
    d.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    d.extend_from_slice(payload);
    d
}

#[derive(Debug, Default, PartialEq)]
pub struct Response {
    pub text: Option<String>,
    pub last: bool,
    pub error: Option<String>,
}

pub fn parse(b: &[u8]) -> Option<Response> {
    if b.len() < 4 {
        return None;
    }
    let (kind, flags) = (b[1] >> 4, b[1] & 0x0f);
    let mut i = (b[0] & 0x0f) as usize * 4;
    let u32_at = |i: &mut usize| -> Option<u32> {
        let v = u32::from_be_bytes(b.get(*i..*i + 4)?.try_into().ok()?);
        *i += 4;
        Some(v)
    };
    if flags & 0b0001 != 0 {
        u32_at(&mut i)?; // sequence
    }
    if flags & 0b0100 != 0 {
        u32_at(&mut i)?; // event
    }
    let code = if kind == SERVER_ERROR { Some(u32_at(&mut i)?) } else { None };
    let size = u32_at(&mut i)? as usize;
    let payload = b.get(i..i + size)?;
    let last = flags & 0b0010 != 0;
    if let Some(code) = code {
        return Some(Response { last: true, error: Some(format!("{code} {}", String::from_utf8_lossy(payload))), ..Default::default() });
    }
    if kind != SERVER_FULL {
        return None;
    }
    if payload.starts_with(&[0x1f, 0x8b]) {
        return Some(Response { last: true, error: Some("unexpected gzip payload".into()), ..Default::default() });
    }
    let text = serde_json::from_slice::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v["result"]["text"].as_str().map(str::to_owned));
    Some(Response { text, last, error: None })
}

fn cjk(c: char) -> bool {
    matches!(c as u32, 0x3000..=0x303F | 0xFF00..=0xFFEF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F)
}

/// The second pass joins VAD segments with a space ("另外一方面是 有一些"); drop spaces between CJK characters.
pub fn tidy(t: &str) -> String {
    let chars: Vec<char> = t.chars().collect();
    let mut out = String::with_capacity(t.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ' ' {
            let end = chars[i..].iter().position(|&c| c != ' ').map_or(chars.len(), |p| i + p);
            let between = i > 0 && cjk(chars[i - 1]) && end < chars.len() && cjk(chars[end]);
            if !between {
                out.extend(&chars[i..end]);
            }
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

pub struct Options {
    pub key: String,
    pub context: Option<String>,
    pub nonstream: bool,
}

pub enum Event {
    Connected(Duration),
    Text(String),
    Final(Result<String, String>),
}

/// One utterance. Audio fed before the socket opens is held and sent after the full request, as in Swift.
/// Dropping `audio`'s sender finishes the utterance. Events end with exactly one `Final`.
pub async fn session(opts: Options, mut audio: mpsc::Receiver<Vec<u8>>, events: mpsc::UnboundedSender<Event>) {
    let r = run(opts, &mut audio, &events).await;
    let _ = events.send(Event::Final(r));
}

async fn run(opts: Options, audio: &mut mpsc::Receiver<Vec<u8>>, events: &mpsc::UnboundedSender<Event>) -> Result<String, String> {
    let url = std::env::var("PLAID_URL").unwrap_or("wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async".into());
    let resource = std::env::var("PLAID_RESOURCE_ID").unwrap_or("volc.seedasr.sauc.duration".into());
    let mut req = url.into_client_request().map_err(|e| e.to_string())?;
    let h = req.headers_mut();
    let hv = |s: &str| HeaderValue::from_str(s).map_err(|e| e.to_string());
    h.insert("X-Api-Key", hv(&opts.key)?);
    h.insert("X-Api-Resource-Id", hv(&resource)?);
    h.insert("X-Api-Connect-Id", hv(&uuid::Uuid::new_v4().to_string())?);

    let t = Instant::now();
    let (ws, resp) = tokio_tungstenite::connect_async(req).await.map_err(|e| e.to_string())?;
    let _ = events.send(Event::Connected(t.elapsed()));
    let logid = resp.headers().get("X-Tt-Logid").and_then(|v| v.to_str().ok()).unwrap_or("-").to_owned();
    let (mut tx, mut rx) = ws.split();

    let mut request = serde_json::json!({
        "model_name": "bigmodel", "enable_itn": true, "enable_punc": true, "enable_ddc": true,
        "enable_nonstream": opts.nonstream,
    });
    if let Some(ctx) = opts.context {
        request["corpus"] = serde_json::json!({ "context": ctx });
    }
    let params = serde_json::json!({
        "user": { "uid": "plaid" },
        "audio": { "format": "pcm", "codec": "raw", "rate": 16000, "bits": 16, "channel": 1 },
        "request": request,
    });
    let full = frame(FULL_REQUEST, false, true, params.to_string().as_bytes());
    tx.send(Message::binary(full)).await.map_err(|e| e.to_string())?;

    let sender = async move {
        let mut buf = Vec::new();
        while let Some(pcm) = audio.recv().await {
            buf.extend_from_slice(&pcm);
            while buf.len() >= PACKET {
                let rest = buf.split_off(PACKET);
                tx.send(Message::binary(frame(AUDIO_ONLY, false, false, &buf))).await?;
                buf = rest;
            }
        }
        tx.send(Message::binary(frame(AUDIO_ONLY, true, false, &buf))).await?;
        Ok::<_, tokio_tungstenite::tungstenite::Error>(())
    };
    let latest = std::sync::Mutex::new(String::new());
    let receiver = async {
        let mut last_raw = None;
        while let Some(msg) = rx.next().await {
            let Message::Binary(d) = msg.map_err(|e| format!("{e} (logid {logid})"))? else { continue };
            let Some(r) = parse(&d) else { continue };
            if let Some(e) = r.error {
                return Err(e);
            }
            if let Some(raw) = r.text.filter(|raw| last_raw.as_ref() != Some(raw)) {
                let t = tidy(&raw);
                last_raw = Some(raw);
                if *latest.lock().unwrap() != t {
                    *latest.lock().unwrap() = t.clone();
                    let _ = events.send(Event::Text(t));
                }
            }
            if r.last {
                return Ok(());
            }
        }
        Err("socket closed before the final result".to_owned())
    };
    tokio::pin!(sender, receiver);
    // The sender finishes first in the normal case; then wait for the final response, at most 5 s.
    let mut sent = false;
    loop {
        tokio::select! {
            r = &mut sender, if !sent => { r.map_err(|e| e.to_string())?; sent = true; }
            r = &mut receiver => { r?; return Ok(latest.lock().unwrap().clone()); }
            _ = tokio::time::sleep(Duration::from_secs(5)), if sent => {
                // Server occasionally never sends the last packet; fall back to the latest partial.
                let l = latest.lock().unwrap().clone();
                return if l.is_empty() { Err("timed out waiting for final result".into()) } else { Ok(l) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_layout() {
        assert_eq!(frame(FULL_REQUEST, false, true, b"{}"), [0x11, 0x10, 0x10, 0, 0, 0, 0, 2, b'{', b'}']);
        assert_eq!(frame(AUDIO_ONLY, true, false, &[7]), [0x11, 0x22, 0, 0, 0, 0, 0, 1, 7]);
    }

    #[test]
    fn parse_full_and_error() {
        let body = br#"{"result":{"text":"hi"}}"#;
        let mut m = vec![0x11, 0x93, 0x10, 0, 0, 0, 0, 5];
        m.extend_from_slice(&(body.len() as u32).to_be_bytes());
        m.extend_from_slice(body);
        assert_eq!(parse(&m), Some(Response { text: Some("hi".into()), last: true, error: None }));
        let mut e = vec![0x11, 0xF0, 0x10, 0, 0, 0, 0, 45, 0, 0, 0, 3];
        e.extend_from_slice(b"bad");
        assert_eq!(parse(&e).unwrap().error.as_deref(), Some("45 bad"));
        assert_eq!(parse(&[0x11, 0x90, 0, 0, 0, 0, 0, 9]), None); // truncated
    }

    #[test]
    fn tidy_spaces() {
        assert_eq!(tidy("另外一方面是 有一些"), "另外一方面是有一些");
        assert_eq!(tidy("用 Rust 写，  对。 好"), "用 Rust 写，对。好");
        assert_eq!(tidy("hello  world"), "hello  world");
    }
}
