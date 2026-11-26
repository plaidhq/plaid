//! One Volcengine streaming utterance: connect on key-down, stream PCM, finish on key-up, wait for the last
//! response. Runtime-agnostic (async-io reactor); the caller spawns the future returned by `start`.
use crate::hotwords::Snapshot;
use crate::refine::Plan;
use crate::volc::{self, AUDIO_ONLY, FULL_REQUEST};
use async_channel::{Receiver, Sender};
use async_tungstenite::tungstenite::{self, Message, client::IntoClientRequest, http::HeaderValue};
use futures_util::{FutureExt, StreamExt};
use std::future::Future;
use std::time::Duration;

/// 200 ms of 16 kHz mono s16le, the packet size the docs recommend.
pub const PACKET: usize = 6400;
/// The server ends the session after ~8 s without an audio packet; while the caller holds back silence, an empty
/// packet keeps it open without adding billed audio.
const KEEPALIVE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct Options {
    pub key: String,
    pub url: String,
    pub resource: String,
    /// Second pass per VAD segment (~+300 ms on release, better text).
    pub nonstream: bool,
    /// Wait for the last response after `finish`; on timeout the latest partial is the result.
    pub final_timeout: Duration,
}

impl Options {
    pub fn new(key: String) -> Self {
        Options {
            key,
            url: "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async".into(),
            resource: "volc.seedasr.sauc.duration".into(),
            nonstream: true,
            final_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// Partial result after tidy + hotword rules; only sent when it changes.
    Text(String),
    /// Exactly once. Last, except that a Cloud session follows it with `Refined` when the server refines, and
    /// with `Usage`.
    Done(Result<String, String>),
    /// Cloud only: the server's LLM pass over the final.
    Refined(Result<String, crate::llm::Failure>),
    /// Cloud only: what the request cost, last.
    Usage(crate::cloud::Usage),
}

pub(crate) enum Cmd {
    /// Context texts and, for Cloud, the refine plan.
    Begin(Vec<String>, Option<Box<Plan>>),
    Audio(Vec<u8>),
    Finish,
}

/// Commands for a running session. Dropping it without `finish` cancels the utterance (Done is still sent); a Cloud
/// session is also cancelled by a drop after `finish`, which aborts the server's refine.
pub struct Session {
    tx: Sender<Cmd>,
}

impl Session {
    pub(crate) fn channel() -> (Session, Receiver<Cmd>) {
        let (tx, rx) = async_channel::unbounded();
        (Session { tx }, rx)
    }

    /// Sends the full request. Audio is held until then, so context gathered after key-down still goes first.
    /// `dialog` = context texts, newest first; `plan` is for a Cloud session (Volc ignores it). Only the first
    /// call counts; `finish` begins without context.
    pub fn begin(&self, dialog: Vec<String>, plan: Option<Plan>) {
        let _ = self.tx.try_send(Cmd::Begin(dialog, plan.map(Box::new)));
    }

    /// 16 kHz mono s16le.
    pub fn append(&self, pcm: Vec<u8>) {
        let _ = self.tx.try_send(Cmd::Audio(pcm));
    }

    pub fn finish(&self) {
        let _ = self.tx.try_send(Cmd::Finish);
    }
}

/// Opens the connection immediately; commands queue until it is up.
pub fn start(
    opts: Options,
    hotwords: Snapshot,
) -> (
    Session,
    Receiver<Event>,
    impl Future<Output = ()> + Send + 'static,
) {
    let (session, rx) = Session::channel();
    let (etx, erx) = async_channel::unbounded();
    let fut = async move {
        let r = run(opts, hotwords, rx, &etx).await;
        let _ = etx.send(Event::Done(r)).await;
    };
    (session, erx, fut)
}

fn params(opts: &Options, hotwords: &Snapshot, dialog: &[String]) -> Vec<u8> {
    let mut request = serde_json::json!({
        "model_name": "bigmodel", "enable_itn": true, "enable_punc": true,
        "enable_ddc": true, // drop filler words
        "enable_nonstream": opts.nonstream,
    });
    if let Some(ctx) = hotwords.context(dialog) {
        request["corpus"] = serde_json::json!({ "context": ctx });
    }
    let p = serde_json::json!({
        "user": { "uid": "plaid" },
        "audio": { "format": "pcm", "codec": "raw", "rate": 16000, "bits": 16, "channel": 1 },
        "request": request,
    });
    volc::frame(FULL_REQUEST, false, true, p.to_string().as_bytes())
}

fn describe(e: tungstenite::Error) -> String {
    match e {
        tungstenite::Error::Http(r) => {
            let logid = r
                .headers()
                .get("X-Tt-Logid")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
                .to_owned();
            let body = r
                .body()
                .as_deref()
                .map(String::from_utf8_lossy)
                .unwrap_or_default()
                .into_owned();
            format!(
                "HTTP {} (logid {logid}) {}",
                r.status().as_u16(),
                body.trim()
            )
        }
        e => e.to_string(),
    }
}

async fn run(
    opts: Options,
    hotwords: Snapshot,
    cmds: Receiver<Cmd>,
    events: &Sender<Event>,
) -> Result<String, String> {
    let mut req = opts.url.as_str().into_client_request().map_err(describe)?;
    let h = req.headers_mut();
    let hv = |s: &str| HeaderValue::from_str(s).map_err(|e| e.to_string());
    h.insert("X-Api-Key", hv(&opts.key)?);
    h.insert("X-Api-Resource-Id", hv(&opts.resource)?);
    h.insert("X-Api-Connect-Id", hv(&uuid::Uuid::new_v4().to_string())?);
    // Commands queue while connecting; `finish` arms the deadline even then, so a stalled connect can't hang.
    let (mut queued, mut finished) = (std::collections::VecDeque::new(), false);
    let mut deadline = None::<async_io::Timer>;
    let connect = async_tungstenite::smol::connect_async(req).fuse();
    futures_util::pin_mut!(connect);
    let (mut ws, resp) = loop {
        futures_util::select! {
            r = connect => break r.map_err(describe)?,
            c = next_cmd(&cmds, finished).fuse() => {
                if c.is_err() {
                    return Err("cancelled".into());
                }
                if matches!(c, Ok(Cmd::Finish)) {
                    finished = true;
                    deadline = Some(async_io::Timer::after(opts.final_timeout));
                }
                queued.push_back(c);
            }
            _ = until(&mut deadline).fuse() => return Err("timed out connecting".into()),
        }
    };
    let logid = resp
        .headers()
        .get("X-Tt-Logid")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_owned();
    let send_err = |e: tungstenite::Error| format!("{} (logid {logid})", describe(e));

    let (mut buf, mut begun, mut sent_last) = (Vec::<u8>::new(), false, false);
    let (mut latest, mut last_raw) = (String::new(), None::<String>);
    let mut keepalive = None::<async_io::Timer>;
    loop {
        let cmd = async {
            match queued.pop_front() {
                Some(c) => c,
                None => next_cmd(&cmds, finished).await,
            }
        };
        futures_util::select! {
            c = cmd.fuse() => match c {
                Ok(Cmd::Begin(dialog, _)) if !begun => {
                    begun = true;
                    ws.send(Message::binary(params(&opts, &hotwords, &dialog))).await.map_err(send_err)?;
                    keepalive = Some(async_io::Timer::after(KEEPALIVE));
                }
                Ok(Cmd::Begin(..)) => {}
                Ok(Cmd::Audio(pcm)) => buf.extend_from_slice(&pcm),
                Ok(Cmd::Finish) => {
                    if !begun {
                        begun = true;
                        ws.send(Message::binary(params(&opts, &hotwords, &[]))).await.map_err(send_err)?;
                    }
                    if !finished {
                        finished = true;
                        deadline = Some(async_io::Timer::after(opts.final_timeout));
                    }
                }
                Err(_) => {
                    let _ = ws.close(None).await;
                    return Err("cancelled".into());
                }
            },
            m = ws.next().fuse() => {
                let Some(m) = m else { return Err(format!("socket closed before the final result (logid {logid})")) };
                let Message::Binary(d) = m.map_err(send_err)? else { continue };
                let Some(r) = volc::parse(&d) else { continue };
                if let Some(e) = r.error {
                    return Err(format!("{e} (logid {logid})"));
                }
                if let Some(raw) = r.text.filter(|raw| last_raw.as_ref() != Some(raw)) {
                    let t = hotwords.apply(&volc::tidy(&raw));
                    last_raw = Some(raw);
                    if t != latest {
                        latest = t.clone();
                        let _ = events.send(Event::Text(t)).await;
                    }
                }
                if r.last {
                    let _ = ws.close(None).await;
                    return Ok(latest);
                }
                continue;
            },
            _ = until(&mut keepalive).fuse() => {
                ws.send(Message::binary(volc::frame(AUDIO_ONLY, false, false, &[]))).await.map_err(send_err)?;
                keepalive = Some(async_io::Timer::after(KEEPALIVE));
            },
            _ = until(&mut deadline).fuse() => {
                // The server occasionally never sends the last packet; fall back to the latest partial.
                let _ = ws.close(None).await;
                return if latest.is_empty() { Err("timed out waiting for final result".into()) } else { Ok(latest) };
            },
        }
        // Flush full packets once begun; on finish, the remainder goes out as the last packet.
        while begun && buf.len() >= PACKET {
            let rest = buf.split_off(PACKET);
            ws.send(Message::binary(volc::frame(AUDIO_ONLY, false, false, &buf)))
                .await
                .map_err(send_err)?;
            buf = rest;
            keepalive = Some(async_io::Timer::after(KEEPALIVE));
        }
        // A queued `finish` (it came while connecting) counts once the audio queued before it has gone out.
        if finished && !sent_last && queued.is_empty() {
            sent_last = true;
            ws.send(Message::binary(volc::frame(
                AUDIO_ONLY,
                true,
                false,
                &std::mem::take(&mut buf),
            )))
            .await
            .map_err(send_err)?;
            keepalive = None;
        }
    }
}

/// The next command; none once finished (later ones are ignored).
pub(crate) async fn next_cmd(
    cmds: &Receiver<Cmd>,
    finished: bool,
) -> Result<Cmd, async_channel::RecvError> {
    if finished {
        std::future::pending().await
    } else {
        cmds.recv().await
    }
}

pub(crate) async fn until(deadline: &mut Option<async_io::Timer>) {
    match deadline {
        Some(t) => {
            t.await;
        }
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// A server that accepts and then says nothing: `finish` must still end the session after `final_timeout`.
    #[test]
    fn stalled_connect_times_out_after_finish() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut opts = Options::new("k".into());
        opts.url = format!("ws://{}", l.local_addr().unwrap());
        opts.final_timeout = Duration::from_millis(300);
        let (s, events, fut) = start(opts, Snapshot::default());
        let t = std::thread::spawn(move || async_io::block_on(fut));
        let _held = l.accept().unwrap();
        s.begin(vec![], None);
        s.append(vec![0; PACKET]);
        let t0 = Instant::now();
        s.finish();
        let done = async_io::block_on(events.recv()).unwrap();
        assert_eq!(done, Event::Done(Err("timed out connecting".into())));
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
        t.join().unwrap();
    }

    /// Everything queued while connecting goes out in order, the last-packet flag on the final frame only.
    #[test]
    fn queued_finish_sends_last_packet_last() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut opts = Options::new("k".into());
        opts.url = format!("ws://{}", l.local_addr().unwrap());
        let (s, events, fut) = start(opts, Snapshot::default());
        let client = std::thread::spawn(move || async_io::block_on(fut));
        let (tcp, _) = l.accept().unwrap();
        s.begin(vec![], None);
        for _ in 0..3 {
            s.append(vec![1; PACKET]);
        }
        s.finish();
        std::thread::sleep(Duration::from_millis(100));
        // (type, last flag, payload bytes) per frame, until the last-flagged one.
        let frames = async_io::block_on(async {
            let tcp = async_io::Async::new(tcp).unwrap();
            let mut ws = async_tungstenite::accept_async(tcp).await.unwrap();
            let mut frames = vec![];
            while let Some(Ok(m)) = ws.next().await {
                let Message::Binary(d) = m else { continue };
                let last = d[1] & 0b0010 != 0;
                frames.push((
                    d[1] >> 4,
                    last,
                    u32::from_be_bytes(d[4..8].try_into().unwrap()),
                ));
                if last {
                    break;
                }
            }
            frames
        });
        let audio = |last| (AUDIO_ONLY, last, PACKET as u32);
        assert_eq!(
            frames[1..],
            [
                audio(false),
                audio(false),
                audio(false),
                (AUDIO_ONLY, true, 0)
            ]
        );
        assert_eq!(frames[0].0, FULL_REQUEST);
        drop(events);
        client.join().unwrap();
    }

    /// While no audio comes (the gate is holding silence), empty packets keep the server from timing out.
    #[test]
    fn idle_session_sends_empty_keepalive_packets() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut opts = Options::new("k".into());
        opts.url = format!("ws://{}", l.local_addr().unwrap());
        let (s, events, fut) = start(opts, Snapshot::default());
        let client = std::thread::spawn(move || async_io::block_on(fut));
        let (tcp, _) = l.accept().unwrap();
        s.begin(vec![], None);
        let (mut s, mut finisher) = (Some(s), None);
        let frames = async_io::block_on(async {
            let tcp = async_io::Async::new(tcp).unwrap();
            let mut ws = async_tungstenite::accept_async(tcp).await.unwrap();
            let mut frames = vec![];
            while let Some(Ok(m)) = ws.next().await {
                let Message::Binary(d) = m else { continue };
                // The keepalive is armed once the full request is out; finish well after one is due.
                if frames.is_empty() {
                    let s = s.take().unwrap();
                    finisher = Some(std::thread::spawn(move || {
                        std::thread::sleep(KEEPALIVE + Duration::from_millis(500));
                        s.finish();
                    }));
                }
                let last = d[1] & 0b0010 != 0;
                frames.push((
                    d[1] >> 4,
                    last,
                    u32::from_be_bytes(d[4..8].try_into().unwrap()),
                ));
                if last {
                    break;
                }
            }
            frames
        });
        assert_eq!(frames[1..], [(AUDIO_ONLY, false, 0), (AUDIO_ONLY, true, 0)]);
        finisher.unwrap().join().unwrap();
        drop(events);
        client.join().unwrap();
    }
}
