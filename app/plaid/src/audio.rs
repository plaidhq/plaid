//! Input device (`micDevice`, else the system default) → mono → 16 kHz s16le, posted to the controller as exact 100 ms `Msg::Audio` buffers.
use async_channel::Sender;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use plaid_core::dictation::Msg;

const CHUNK: usize = 3200; // 100 ms of 16 kHz s16

/// Windowed-sinc resampler to 16 kHz: low-pass at 0.92 × 8 kHz (Blackman window, 16 zero crossings per side), so
/// speech energy above 8 kHz is removed instead of folding back into the band the ASR hears. The kernel is
/// tabulated at `PHASES` points per input sample and linearly interpolated. Delay: `half` input samples (~1 ms).
pub struct Resampler {
    rate: u64,
    /// Kernel half-width in input samples.
    half: usize,
    /// Kernel at |d| = i / PHASES input samples.
    table: Vec<f32>,
    /// Input still in reach of the kernel; buf[0] is stream sample `base`. The stream starts with `half` zeros.
    buf: Vec<f32>,
    base: u64,
    /// Next output sample; it sits at stream position half + n × rate / 16000 (exact, so chunking can't drift).
    n: u64,
}

const PHASES: usize = 256;

impl Resampler {
    pub fn new(in_rate: u32) -> Self {
        let step = in_rate as f64 / 16000.0;
        // Cutoff in cycles per input sample; upsampling keeps the input's own band.
        let fc = 0.92 * 0.5 / step.max(1.0);
        let half = (16.0 / (2.0 * fc)).ceil() as usize;
        let table = (0..=half * PHASES + 1)
            .map(|i| {
                let d = i as f64 / PHASES as f64;
                if d >= half as f64 {
                    return 0.0;
                }
                let x = 2.0 * fc * d * std::f64::consts::PI;
                let sinc = if x == 0.0 { 1.0 } else { x.sin() / x };
                let w = std::f64::consts::PI * d / half as f64;
                (2.0 * fc * sinc * (0.42 + 0.5 * w.cos() + 0.08 * (2.0 * w).cos())) as f32
            })
            .collect();
        Resampler {
            rate: in_rate as u64,
            half,
            table,
            buf: vec![0.0; half],
            base: 0,
            n: 0,
        }
    }

    fn kernel(&self, d: f64) -> f32 {
        let x = d.abs() * PHASES as f64;
        let i = x as usize;
        let t = (x - i as f64) as f32;
        self.table[i] + (self.table[i + 1] - self.table[i]) * t
    }

    /// Stream position of output `n`: integer sample and fraction.
    fn at(&self, n: u64) -> (u64, f64) {
        let x = n * self.rate;
        (self.half as u64 + x / 16000, (x % 16000) as f64 / 16000.0)
    }

    pub fn push(&mut self, input: &[f32], out: &mut Vec<i16>) {
        self.buf.extend_from_slice(input);
        let half = self.half as u64;
        loop {
            let (c, frac) = self.at(self.n);
            if c + half >= self.base + self.buf.len() as u64 {
                break;
            }
            let acc: f32 = (c + 1 - half..=c + half)
                .map(|k| {
                    self.buf[(k - self.base) as usize] * self.kernel(k as f64 - c as f64 - frac)
                })
                .sum();
            out.push((acc.clamp(-1.0, 1.0) * 32767.0) as i16);
            self.n += 1;
        }
        let keep_from = self.at(self.n).0 + 1 - half;
        let done = keep_from
            .saturating_sub(self.base)
            .min(self.buf.len() as u64);
        self.buf.drain(..done as usize);
        self.base += done;
    }

    /// Emits what the lookahead still holds (end of the stream).
    pub fn flush(&mut self, out: &mut Vec<i16>) {
        self.push(&vec![0.0; self.half], out);
    }
}

struct Sink {
    tx: Sender<Msg>,
    generation: u64,
    buf: Vec<u8>,
    rs: Resampler,
    channels: usize,
    out: Vec<i16>,
    mono: Vec<f32>,
}

impl Sink {
    fn feed(&mut self, frames: impl Iterator<Item = f32>) {
        // Channel 0 only, as the Swift Resampler does: averaging dilutes the level on multichannel devices.
        self.mono.clear();
        self.mono.extend(frames.step_by(self.channels));
        self.out.clear();
        self.rs.push(&self.mono, &mut self.out);
        self.buf
            .extend(self.out.iter().flat_map(|s| s.to_le_bytes()));
        while self.buf.len() >= CHUNK {
            let rest = self.buf.split_off(CHUNK);
            let pcm = std::mem::replace(&mut self.buf, rest);
            self.send(pcm);
        }
    }

    /// Unbounded channel: never blocks the audio thread.
    fn send(&self, pcm: Vec<u8>) {
        let _ = self.tx.try_send(Msg::Audio {
            generation: self.generation,
            pcm,
        });
    }
}

impl Drop for Sink {
    // Stream dropped (release): flush the last partial chunk.
    fn drop(&mut self) {
        self.out.clear();
        self.rs.flush(&mut self.out);
        self.buf
            .extend(self.out.iter().flat_map(|s| s.to_le_bytes()));
        if !self.buf.is_empty() {
            let pcm = std::mem::take(&mut self.buf);
            self.send(pcm);
        }
    }
}

type Shared = std::sync::Arc<std::sync::Mutex<Option<Sink>>>;

/// Recording runs until `stop` (or drop). cpal streams aren't Send, so the stream lives on its own thread.
/// The sink is shared rather than moved into the callback, so stopping detaches it even if the backend keeps
/// the callback alive after the stream is dropped.
pub struct Capture {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    sink: Shared,
    pub device: String,
}

impl Capture {
    /// Stops recording; the last partial buffer is posted before this returns.
    pub fn stop(mut self) {
        self.halt();
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
        // Sink::drop flushes the last partial chunk.
        self.halt();
    }
}

fn feed(sink: &Shared, frames: impl Iterator<Item = f32>) {
    // try_lock: never block the audio thread; contention only happens at the moment Capture drops.
    if let Ok(mut g) = sink.try_lock()
        && let Some(s) = g.as_mut()
    {
        s.feed(frames);
    }
}

/// `micDevice` (cpal device id: the Core Audio UID on macOS, as the Swift app stored it) while it is connected,
/// else the system default input.
fn input_device() -> Option<cpal::Device> {
    let host = cpal::default_host();
    if let Some(want) = crate::config::value("micDevice").filter(|v| !v.is_empty()) {
        let found = host
            .input_devices()
            .ok()
            .and_then(|mut ds| ds.find(|d| d.id().is_ok_and(|id| id.id() == want)));
        if found.is_some() {
            return found;
        }
        crate::trace::log("micDevice not connected, using the default input");
    }
    host.default_input_device()
}

pub fn start(tx: Sender<Msg>, generation: u64) -> Result<Capture, String> {
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let (ok_tx, ok_rx) = std::sync::mpsc::channel();
    let sink: Shared = Default::default();
    let shared = sink.clone();
    let thread = std::thread::Builder::new()
        .name("mic".into())
        .spawn(move || {
            let run = || -> Result<(cpal::Stream, String), String> {
                let dev = input_device().ok_or("no input device")?;
                let name = dev
                    .description()
                    .map(|d| d.to_string())
                    .unwrap_or_else(|_| "?".into());
                let cfg = dev.default_input_config().map_err(|e| e.to_string())?;
                *shared.lock().unwrap() = Some(Sink {
                    tx,
                    generation,
                    buf: Vec::with_capacity(CHUNK * 2),
                    rs: Resampler::new(cfg.sample_rate()),
                    channels: cfg.channels() as usize,
                    out: vec![],
                    mono: vec![],
                });
                let (a, b) = (shared.clone(), shared.clone());
                let err = |e| eprintln!("mic: {e}");
                let stream = match cfg.sample_format() {
                    cpal::SampleFormat::F32 => dev.build_input_stream(
                        cfg.config(),
                        move |d: &[f32], _: &_| feed(&a, d.iter().copied()),
                        err,
                        None,
                    ),
                    cpal::SampleFormat::I16 => dev.build_input_stream(
                        cfg.config(),
                        move |d: &[i16], _: &_| feed(&b, d.iter().map(|&s| s as f32 / 32768.0)),
                        err,
                        None,
                    ),
                    f => return Err(format!("unsupported sample format {f:?}")),
                }
                .map_err(|e| e.to_string())?;
                stream.play().map_err(|e| e.to_string())?;
                Ok((stream, name))
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
    Ok(Capture {
        stop: Some(stop_tx),
        thread: Some(thread),
        sink,
        device,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| (i as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 0.5)
            .collect()
    }

    fn resample(rate: u32, input: &[f32]) -> Vec<i16> {
        let mut rs = Resampler::new(rate);
        let mut out = vec![];
        for c in input.chunks(rate as usize / 100) {
            rs.push(c, &mut out);
        }
        out
    }

    fn rms(s: &[i16]) -> f32 {
        (s.iter().map(|&x| (x as f32).powi(2)).sum::<f32>() / s.len() as f32).sqrt()
    }

    #[test]
    fn resample_passes_speech_band_without_delay() {
        for (rate, hz) in [
            (48000, 440.0),
            (48000, 3000.0),
            (44100, 1000.0),
            (96000, 6000.0),
            (8000, 1000.0),
        ] {
            let out = resample(rate, &tone(rate, hz, 1.0));
            // Output i is input time i / 16000 s, delayed only by the `half` lookahead the stream hasn't reached.
            let max = out.iter().enumerate().skip(100).map(|(i, &s)| {
                let ideal = (i as f32 * hz * std::f32::consts::TAU / 16000.0).sin() * 0.5 * 32767.0;
                (s as f32 - ideal).abs()
            });
            let max = max.fold(0f32, f32::max);
            assert!(max < 100.0, "{rate} Hz / {hz} Hz tone: max err {max}");
        }
    }

    #[test]
    fn resample_removes_content_above_8k() {
        for (rate, hz) in [
            (48000, 9000.0),
            (48000, 12000.0),
            (44100, 10000.0),
            (96000, 20000.0),
        ] {
            let out = resample(rate, &tone(rate, hz, 1.0));
            let ratio = rms(&out[200..]) / (0.5 * 32767.0 / 2f32.sqrt());
            assert!(ratio < 0.003, "{rate} Hz / {hz} Hz tone leaks {ratio}");
        }
    }

    #[test]
    fn resample_44k1_length() {
        let mut rs = Resampler::new(44100);
        let mut out = vec![];
        for _ in 0..100 {
            rs.push(&[0.1; 441], &mut out);
        }
        rs.flush(&mut out);
        assert!((15999..=16001).contains(&out.len()), "{}", out.len());
    }

    /// Output positions are exact, so the callback's buffer size can't change a sample.
    #[test]
    fn resample_ignores_chunking() {
        for rate in [44100, 48000, 22050] {
            let input = tone(rate, 1234.0, 0.5);
            let mut one = vec![];
            let mut rs = Resampler::new(rate);
            rs.push(&input, &mut one);
            rs.flush(&mut one);
            let mut many = vec![];
            let mut rs = Resampler::new(rate);
            let mut rest = &input[..];
            for n in [1, 160, 512, 1024, 7, 4410].into_iter().cycle() {
                let (c, r) = rest.split_at(n.min(rest.len()));
                rs.push(c, &mut many);
                rest = r;
                if rest.is_empty() {
                    break;
                }
            }
            rs.flush(&mut many);
            assert!(one == many, "{rate} Hz: chunked output differs");
        }
    }
}
