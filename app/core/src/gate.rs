//! Holds long silence back from the ASR stream. Volcengine bills the audio sent, and a held key or a tap-started
//! session can sit silent before, between and after speech; the session's keepalive holds the connection open
//! meanwhile. Only long stretches are held: a buffer counts as speech when either check says so, and a pause
//! keeps streaming for `KEEP` before anything is held.
//! - RMS against the quietest buffer so far. Alone it cut whole phrases in a noisy room (speech under 2x the noise).
//! - earshot's voice detector. Alone it missed quiet speech.
//!
//! Scored on 478 recordings with ASR word timestamps, 20 s of each file's own steady room noise spliced in at the
//! start and in the widest word gap: no word lost, 62% of the spliced noise held (85% is the most a 3 s keep
//! allows); at a quarter and four times the level, 1 and 0 words lost. Noise that is not steady (breathing,
//! typing, voices) keeps streaming: the few real gaps over 4 s in those recordings were almost all sent.
use earshot::{DefaultPredictor, Detector};
use std::collections::VecDeque;

/// Buffers (100 ms) of a pause still sent.
const KEEP: usize = 30;
/// Buffers always sent at the start: the floor is not known yet, and soft first words were lost without it. It also
/// gives a silent press some audio, so the server answers with an empty result rather than "no audio received".
const LEAD: usize = 10;
/// Held buffers sent ahead of resumed speech, for soft onsets: 3 and 5 lost first words in the scoring.
const PREROLL: usize = 8;
/// Buffers at the start left out of the floor: the mic ramps up from near zero.
const RAMP: usize = 3;
/// Detector frame, samples.
const FRAME: usize = 256;
/// The detector misses speech on a quiet mic, so its input is scaled toward this level by the loudest buffer the
/// RMS check confirmed (never by noise, which scaled up reads as voice), at most `MAX_GAIN`.
const TARGET_RMS: f64 = 1000.;
const MAX_GAIN: f64 = 4.;

pub struct Gate {
    vad: Box<Detector<DefaultPredictor>>,
    /// Samples not yet making up a whole detector frame.
    carry: Vec<i16>,
    buffers: usize,
    /// Quietest buffer RMS after the ramp: only falls, so a rising background never makes speech read as silence.
    floor: f64,
    /// Loudest buffer the RMS check called speech.
    peak: f64,
    held: VecDeque<Vec<u8>>,
    /// Buffers since the last speech; the start counts as `LEAD` short of `KEEP`.
    quiet: usize,
    pub sent_bytes: usize,
}

impl Default for Gate {
    fn default() -> Self {
        Gate {
            vad: Detector::default_boxed(),
            carry: vec![],
            buffers: 0,
            floor: f64::INFINITY,
            peak: 0.,
            held: VecDeque::new(),
            quiet: KEEP - LEAD,
            sent_bytes: 0,
        }
    }
}

impl Gate {
    /// One capture buffer (16 kHz mono s16le) and its RMS; returns the PCM to send now (possibly empty).
    pub fn push(&mut self, pcm: Vec<u8>, rms: f64) -> Vec<u8> {
        if self.buffers >= RAMP && rms >= 1. {
            self.floor = self.floor.min(rms);
        }
        self.buffers += 1;
        let loud = self.floor.is_finite() && rms >= (self.floor * 2.).max(self.floor + 15.);
        if loud {
            self.peak = self.peak.max(rms);
        }
        let gain = if self.peak > 0. {
            (TARGET_RMS / self.peak).clamp(1., MAX_GAIN)
        } else {
            1.
        } as f32;
        self.carry.extend(
            pcm.as_chunks::<2>()
                .0
                .iter()
                .map(|b| (i16::from_le_bytes(*b) as f32 * gain).clamp(-32768., 32767.) as i16),
        );
        let whole = self.carry.len() / FRAME * FRAME;
        let mut voice = false;
        for f in self.carry[..whole].as_chunks::<FRAME>().0 {
            // Every frame goes through: the detector keeps state across frames.
            voice |= self.vad.predict_i16(f) >= 0.5;
        }
        self.carry.drain(..whole);

        let mut out = Vec::new();
        if loud || voice {
            for b in self.held.drain(..) {
                out.extend_from_slice(&b);
            }
            out.extend_from_slice(&pcm);
            self.quiet = 0;
        } else if self.quiet < KEEP {
            self.quiet += 1;
            out = pcm;
        } else {
            self.held.push_back(pcm);
            if self.held.len() > PREROLL {
                self.held.pop_front();
            }
        }
        self.sent_bytes += out.len();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = 1600;

    /// 100 ms of a 200 Hz tone at `amp` over steady low noise (`amp` 0: noise only).
    fn buf(amp: f64, seed: usize) -> Vec<u8> {
        (0..N)
            .flat_map(|i| {
                let t = (seed * N + i) as f64;
                let noise = ((t * 12.9898).sin() * 43758.5453).fract() * 40.;
                let s = (t * 200. * std::f64::consts::TAU / 16000.).sin() * amp + noise;
                (s as i16).to_le_bytes()
            })
            .collect()
    }

    /// Sends the levels through the gate; returns which buffers went out, by index.
    fn run(levels: &[f64]) -> Vec<usize> {
        let mut g = Gate::default();
        let bufs: Vec<Vec<u8>> = levels.iter().enumerate().map(|(i, &a)| buf(a, i)).collect();
        let mut out = vec![];
        for b in &bufs {
            out.extend(g.push(b.clone(), crate::dictation::rms(b)));
        }
        assert_eq!(g.sent_bytes, out.len());
        out.chunks(N * 2)
            .map(|c| bufs.iter().position(|b| b == c).unwrap())
            .collect()
    }

    fn speech_after(silence: usize, gap: usize) -> Vec<f64> {
        let mut l = vec![0.; silence];
        l.extend([3000.; 5]);
        l.extend(vec![0.; gap]);
        l.extend([3000.; 5]);
        l
    }

    #[test]
    fn long_leading_silence_is_held_after_the_lead_except_the_preroll() {
        let sent = run(&speech_after(50, 0));
        let mut want: Vec<usize> = (0..LEAD).collect();
        want.extend(50 - PREROLL..60);
        assert_eq!(sent, want);
    }

    #[test]
    fn a_long_pause_keeps_three_seconds() {
        let sent = run(&speech_after(4, 100));
        // The detector trails speech by a buffer or so, which pushes the keep back as much.
        let gap: Vec<usize> = sent
            .iter()
            .copied()
            .filter(|i| (9..109 - PREROLL).contains(i))
            .collect();
        assert!(gap.len() >= KEEP && gap.len() <= KEEP + 2, "{gap:?}");
        assert!(sent.ends_with(&(109 - PREROLL..114).collect::<Vec<_>>())); // preroll + speech
    }

    #[test]
    fn a_pause_under_three_seconds_is_sent_whole() {
        assert_eq!(run(&speech_after(4, 25)), (0..39).collect::<Vec<_>>());
    }

    #[test]
    fn silence_alone_sends_only_the_lead() {
        assert_eq!(run(&[0.; 40]), (0..LEAD).collect::<Vec<_>>());
    }
}
