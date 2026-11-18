//! 16 kHz mono s16le WAV, the format Plaid records and replays.

pub fn encode(pcm: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(44 + pcm.len());
    let n = pcm.len() as u32;
    d.extend_from_slice(b"RIFF");
    d.extend_from_slice(&(36 + n).to_le_bytes());
    d.extend_from_slice(b"WAVEfmt ");
    d.extend_from_slice(&16u32.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes()); // PCM
    d.extend_from_slice(&1u16.to_le_bytes()); // mono
    d.extend_from_slice(&16000u32.to_le_bytes());
    d.extend_from_slice(&32000u32.to_le_bytes());
    d.extend_from_slice(&2u16.to_le_bytes());
    d.extend_from_slice(&16u16.to_le_bytes());
    d.extend_from_slice(b"data");
    d.extend_from_slice(&n.to_le_bytes());
    d.extend_from_slice(pcm);
    d
}

/// The PCM bytes of a 16 kHz mono 16-bit PCM WAV.
pub fn decode(b: &[u8]) -> Result<&[u8], String> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let (mut i, mut fmt_ok) = (12, false);
    while i + 8 <= b.len() {
        let size = u32::from_le_bytes(b[i + 4..i + 8].try_into().unwrap()) as usize;
        let body = i + 8;
        match &b[i..i + 4] {
            b"fmt " if body + 16 <= b.len() => {
                let rate = u32::from_le_bytes(b[body + 4..body + 8].try_into().unwrap());
                let (fmt, ch, bits) = (u16_at(body), u16_at(body + 2), u16_at(body + 14));
                if (fmt, ch, rate, bits) != (1, 1, 16000, 16) {
                    return Err(format!(
                        "need 16 kHz mono s16 PCM, got format {fmt} {rate} Hz ×{ch} {bits}-bit"
                    ));
                }
                fmt_ok = true;
            }
            b"data" if fmt_ok => return Ok(&b[body..(body + size).min(b.len())]),
            _ => {}
        }
        i = body + size + (size & 1);
    }
    Err("no fmt/data chunk".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip() {
        let pcm: Vec<u8> = (0..100u8).collect();
        assert_eq!(super::decode(&super::encode(&pcm)).unwrap(), &pcm[..]);
    }
}
