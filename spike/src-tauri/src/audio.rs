// Default input device → mono → 16 kHz s16le, delivered in 100 ms chunks.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc::Sender;

const CHUNK: usize = 3200; // 100 ms of 16 kHz s16

/// Linear-interpolation resampler with a box pre-filter as a crude anti-alias. Good enough for a spike;
/// a real build should use a windowed-sinc resampler.
pub struct Resampler {
    step: f64,
    width: usize,
    t: f64,
    prev: f32,
    hist: Vec<f32>,
}

impl Resampler {
    pub fn new(in_rate: u32) -> Self {
        let step = in_rate as f64 / 16000.0;
        Resampler { step, width: step.round().max(1.0) as usize, t: 0.0, prev: 0.0, hist: vec![] }
    }

    pub fn push(&mut self, input: &[f32], out: &mut Vec<i16>) {
        if input.is_empty() {
            return;
        }
        // Moving average over `width` samples, carrying history across calls.
        let mut x = Vec::with_capacity(input.len());
        for &s in input {
            self.hist.push(s);
            if self.hist.len() > self.width {
                self.hist.remove(0);
            }
            x.push(self.hist.iter().sum::<f32>() / self.hist.len() as f32);
        }
        let n = x.len();
        while self.t < (n - 1) as f64 {
            let i = self.t.floor();
            let f = (self.t - i) as f32;
            let a = if i < 0.0 { self.prev } else { x[i as usize] };
            let b = x[(i + 1.0) as usize];
            out.push(((a + (b - a) * f).clamp(-1.0, 1.0) * 32767.0) as i16);
            self.t += self.step;
        }
        self.t -= n as f64;
        self.prev = x[n - 1];
    }
}

struct Sink {
    tx: Sender<Vec<u8>>,
    buf: Vec<u8>,
    rs: Resampler,
    channels: usize,
    out: Vec<i16>,
    mono: Vec<f32>,
    /// Everything sent, kept for the per-utterance level log and the last-utterance WAV.
    rec: Vec<i16>,
}

impl Sink {
    fn feed(&mut self, frames: impl Iterator<Item = f32>) {
        // Channel 0 only, as the Swift Resampler does: averaging dilutes the level on multichannel devices.
        self.mono.clear();
        self.mono.extend(frames.step_by(self.channels));
        self.out.clear();
        self.rs.push(&self.mono, &mut self.out);
        self.rec.extend_from_slice(&self.out);
        self.buf.extend(self.out.iter().flat_map(|s| s.to_le_bytes()));
        if self.buf.len() >= CHUNK {
            // Never block the audio thread; the channel is sized for several seconds of backlog.
            let _ = self.tx.try_send(std::mem::take(&mut self.buf));
        }
    }
}

impl Drop for Sink {
    // Stream dropped (release): flush the last partial chunk.
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            let _ = self.tx.try_send(std::mem::take(&mut self.buf));
        }
    }
}

type Shared = std::sync::Arc<std::sync::Mutex<Option<Sink>>>;

/// Recording runs until this is dropped. cpal streams aren't Send, so the stream lives on its own thread.
/// The sink (and the channel sender in it) is shared rather than moved into the callback, so dropping Capture
/// closes the channel even if the backend keeps the callback alive after the stream is dropped.
pub struct Capture {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    sink: Shared,
    pub device: String,
}

impl Capture {
    /// Stop recording, close the channel and return every 16 kHz sample that was sent.
    pub fn stop(mut self) -> Vec<i16> {
        self.halt().map(|mut s| std::mem::take(&mut s.rec)).unwrap_or_default()
    }

    fn halt(&mut self) -> Option<Sink> {
        self.stop.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.sink.lock().unwrap().take()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // Sink::drop flushes the last partial chunk, then the sender drops and the channel closes.
        self.halt();
    }
}

fn feed(sink: &Shared, frames: impl Iterator<Item = f32>) {
    // try_lock: never block the audio thread; contention only happens at the moment Capture drops.
    if let Ok(mut g) = sink.try_lock() {
        if let Some(s) = g.as_mut() {
            s.feed(frames);
        }
    }
}

pub fn start(tx: Sender<Vec<u8>>) -> Result<Capture, String> {
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let (ok_tx, ok_rx) = std::sync::mpsc::channel();
    let sink: Shared = Default::default();
    let shared = sink.clone();
    let thread = std::thread::Builder::new()
        .name("mic".into())
        .spawn(move || {
            let run = || -> Result<(cpal::Stream, String), String> {
                let dev = cpal::default_host().default_input_device().ok_or("no input device")?;
                let name = dev.description().map(|d| d.to_string()).unwrap_or_else(|_| "?".into());
                let cfg = dev.default_input_config().map_err(|e| e.to_string())?;
                *shared.lock().unwrap() = Some(Sink {
                    tx,
                    buf: Vec::with_capacity(CHUNK * 2),
                    rs: Resampler::new(cfg.sample_rate()),
                    channels: cfg.channels() as usize,
                    out: vec![],
                    mono: vec![],
                    rec: vec![],
                });
                let (a, b) = (shared.clone(), shared.clone());
                let err = |e| eprintln!("mic: {e}");
                let stream = match cfg.sample_format() {
                    cpal::SampleFormat::F32 => dev.build_input_stream(cfg.config(), move |d: &[f32], _: &_| feed(&a, d.iter().copied()), err, None),
                    cpal::SampleFormat::I16 => {
                        dev.build_input_stream(cfg.config(), move |d: &[i16], _: &_| feed(&b, d.iter().map(|&s| s as f32 / 32768.0)), err, None)
                    }
                    f => return Err(format!("unsupported sample format {f:?}")),
                }
                .map_err(|e| e.to_string())?;
                stream.play().map_err(|e| e.to_string())?;
                Ok((stream, format!("{name} {}Hz×{}", cfg.sample_rate(), cfg.channels())))
            };
            match run() {
                Ok((stream, name)) => {
                    let _ = ok_tx.send(Ok(name));
                    let _ = stop_rx.recv(); // Err when Capture drops the sender
                    drop(stream);
                }
                Err(e) => {
                    let _ = ok_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let device = ok_rx.recv().map_err(|e| e.to_string())??;
    Ok(Capture { stop: Some(stop_tx), thread: Some(thread), sink, device })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_48k_sine() {
        let input: Vec<f32> = (0..48000).map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin() * 0.5).collect();
        let mut rs = Resampler::new(48000);
        let mut out = vec![];
        for c in input.chunks(480) {
            rs.push(c, &mut out);
        }
        assert!((15990..=16000).contains(&out.len()), "{}", out.len());
        // Output i samples input 3i through the 3-tap box filter (1 input sample of delay, < 1% gain loss at 440 Hz).
        let err = out.iter().enumerate().skip(10).map(|(i, &s)| {
            let ideal = ((3 * i) as f32 - 1.0).mul_add(440.0 * std::f32::consts::TAU / 48000.0, 0.0).sin() * 0.5 * 32767.0;
            (s as f32 - ideal).abs()
        });
        let max = err.fold(0f32, f32::max);
        assert!(max < 200.0, "max err {max}");
    }

    #[test]
    fn resample_44k1_length() {
        let mut rs = Resampler::new(44100);
        let mut out = vec![];
        for _ in 0..100 {
            rs.push(&[0.1; 441], &mut out);
        }
        assert!((15990..=16000).contains(&out.len()), "{}", out.len());
    }
}
