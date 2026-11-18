//! Volcengine bigmodel streaming ASR wire format. Docs: https://www.volcengine.com/docs/6561/1354869
//! Frames: 4-byte header | [seq/event if flagged] | u32 size | payload, all big-endian.
//! We send uncompressed payloads without sequence numbers; the server mirrors the compression.

pub const FULL_REQUEST: u8 = 0b0001;
pub const AUDIO_ONLY: u8 = 0b0010;
const SERVER_FULL: u8 = 0b1001;
const SERVER_ERROR: u8 = 0b1111;

pub fn frame(ty: u8, last: bool, json: bool, payload: &[u8]) -> Vec<u8> {
    let mut d = vec![
        0x11,
        ty << 4 | if last { 0b0010 } else { 0 },
        if json { 0x10 } else { 0 },
        0,
    ];
    d.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    d.extend_from_slice(payload);
    d
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Response {
    pub text: Option<String>,
    pub last: bool,
    pub error: Option<String>,
}

pub fn parse(b: &[u8]) -> Option<Response> {
    if b.len() < 4 {
        return None;
    }
    let (ty, flags) = (b[1] >> 4, b[1] & 0x0f);
    let mut i = (b[0] & 0x0f) as usize * 4;
    let mut u32 = || {
        let v = b
            .get(i..i + 4)
            .map(|s| u32::from_be_bytes(s.try_into().unwrap()));
        if v.is_some() {
            i += 4;
        }
        v
    };
    if flags & 0b0001 != 0 {
        u32(); // sequence
    }
    if flags & 0b0100 != 0 {
        u32(); // event
    }
    let code = if ty == SERVER_ERROR { u32() } else { None };
    let size = u32()? as usize;
    let payload = b.get(i..i.checked_add(size)?)?;
    if let Some(code) = code {
        return Some(Response {
            last: true,
            error: Some(format!("{code} {}", String::from_utf8_lossy(payload))),
            ..Default::default()
        });
    }
    if ty != SERVER_FULL {
        return None;
    }
    let last = flags & 0b0010 != 0;
    if payload.starts_with(&[0x1f, 0x8b]) {
        return Some(Response {
            last: true,
            error: Some("unexpected gzip payload".into()),
            ..Default::default()
        });
    }
    if payload.is_empty() {
        return Some(Response {
            last,
            ..Default::default()
        });
    }
    let obj: Option<serde_json::Value> = serde_json::from_slice(payload).ok();
    let text = obj
        .as_ref()
        .and_then(|o| o["result"]["text"].as_str())
        .map(str::to_owned);
    Some(Response {
        text,
        last,
        error: None,
    })
}

/// The second pass joins VAD segments with a space ("另外一方面是 有一些"); drop spaces between CJK characters
/// (Han, CJK Symbols and Punctuation, Halfwidth and Fullwidth Forms).
pub fn tidy(t: &str) -> String {
    let cjk = |c: char| matches!(c as u32, 0x3000..=0x303F | 0xFF00..=0xFFEF) || is_han(c);
    let cs: Vec<char> = t.chars().collect();
    let mut out = String::with_capacity(t.len());
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == ' ' && i > 0 && cjk(cs[i - 1]) {
            let mut j = i;
            while j < cs.len() && cs[j] == ' ' {
                j += 1;
            }
            if j < cs.len() && cjk(cs[j]) {
                i = j;
                continue;
            }
        }
        out.push(cs[i]);
        i += 1;
    }
    out
}

/// ICU `\p{Han}` (Script=Han).
fn is_han(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x2E99 | 0x2E9B..=0x2EF3 | 0x2F00..=0x2FD5 | 0x3005 | 0x3007 | 0x3021..=0x3029 | 0x3038..=0x303B
        | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFA6D | 0xFA70..=0xFAD9 | 0x16FE2 | 0x16FE3 | 0x16FF0 | 0x16FF1
        | 0x20000..=0x2A6DF | 0x2A700..=0x2EE5D | 0x2F800..=0x2FA1D | 0x30000..=0x323AF)
}
