// Throwaway spike: Rust/Tauri building blocks for the cross-platform Plaid. macOS only for now.
//   plaid-spike --file a.wav ...       stream WAVs (16 kHz mono s16) at real-time pace, print partials + release→final latency
//   plaid-spike --paste-test           clipboard snapshot/write/restore on a private pasteboard
//   plaid-spike --mic-test out.wav 5   record the default mic to a 16 kHz WAV (needs Microphone)
//   <app binary> --store-key < keyfile   put the Volc key into the spike's own Keychain item (service dev.neo.plaid.spike)
//   Plaid Spike.app --hud-test [secs]  show the HUD, log activation/key-window facts, quit
//   Plaid Spike.app                    hold-to-talk on PLAID_SPIKE_KEY (default 54, right ⌘); needs Microphone and
//                                      Accessibility
#[cfg(not(target_os = "macos"))]
compile_error!("the spike is macOS-only");

mod audio;
mod hotkey;
mod hud;
mod paste;
mod volc;

use std::io::{Read, Write};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Listener};
use tokio::sync::mpsc;

const KEY_SERVICES: [&str; 2] = ["dev.neo.plaid.spike", "dev.neo.plaid.volc"];

/// PLAID_API_KEY, else the spike's own Keychain item, else the Swift app's item (readable without a prompt only when
/// this binary is signed with identifier dev.neo.plaid.volc). Prompts are disabled so a mismatch fails instead of
/// putting a dialog on screen.
fn api_key() -> Result<String, String> {
    if let Ok(k) = std::env::var("PLAID_API_KEY") {
        return Ok(k);
    }
    use security_framework::os::macos::keychain::SecKeychain;
    let _lock = SecKeychain::disable_user_interaction().map_err(|e| e.to_string())?;
    let mut errs = vec![];
    for s in KEY_SERVICES {
        match security_framework::passwords::get_generic_password(s, "apiKey") {
            Ok(k) => return String::from_utf8(k).map_err(|e| e.to_string()),
            Err(e) => errs.push(format!("{s}: {e}")),
        }
    }
    Err(format!("keychain: {}", errs.join("; ")))
}

/// Same rule as Hotwords.Snapshot.context in Swift: hotword targets from hotwords.txt + hotwords.auto.txt.
fn hotwords_context() -> Option<String> {
    let dir = std::env::var("HOME").ok()? + "/Library/Application Support/Plaid/";
    let text = ["hotwords.txt", "hotwords.auto.txt"].iter().filter_map(|f| std::fs::read_to_string(dir.clone() + f).ok()).collect::<Vec<_>>().join("\n");
    let mut words: Vec<serde_json::Value> = vec![];
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.contains("->")) {
        let head = line.split("~~~").next().unwrap_or("");
        if let Some(w) = head.split('|').map(str::trim).find(|w| !w.is_empty()) {
            words.push(serde_json::json!({ "word": w }));
        }
    }
    (!words.is_empty()).then(|| serde_json::json!({ "hotwords": words }).to_string())
}

fn options(key: String) -> volc::Options {
    volc::Options { key, context: hotwords_context(), nonstream: std::env::var("PLAID_NONSTREAM").as_deref() != Ok("0") }
}

fn log(msg: impl AsRef<str>) {
    let line = format!("{} {}\n", chrono_ms(), msg.as_ref());
    eprint!("{line}");
    let path = std::env::var("PLAID_SPIKE_LOG").unwrap_or_else(|_| std::env::var("HOME").unwrap_or_default() + "/Library/Logs/plaid-spike.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn chrono_ms() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{}.{:03}", d.as_secs(), d.subsec_millis())
}

fn read_pcm(path: &str) -> Vec<u8> {
    let mut r = hound::WavReader::open(path).expect("open wav");
    let s = r.spec();
    assert!(s.sample_rate == 16000 && s.channels == 1 && s.bits_per_sample == 16, "need 16 kHz mono s16, got {s:?}");
    r.samples::<i16>().flat_map(|x| x.unwrap().to_le_bytes()).collect()
}

async fn stream_file(path: &str) {
    let pcm = read_pcm(path);
    let opts = options(api_key().unwrap_or_else(|e| panic!("{e}")));
    let (atx, arx) = mpsc::channel(64);
    let (etx, mut erx) = mpsc::unbounded_channel();
    let t0 = Instant::now();
    tokio::spawn(volc::session(opts, arx, etx));
    // 100 ms feeds starting at t0, like mic buffers; the connection opens concurrently, as URLSession does in Swift.
    let feeder = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        for chunk in pcm.chunks(3200) {
            tick.tick().await;
            if atx.send(chunk.to_vec()).await.is_err() {
                break;
            }
        }
        Instant::now() // release
    });
    let mut released = None;
    let mut feeder = Some(feeder);
    loop {
        tokio::select! {
            r = async { feeder.as_mut().unwrap().await }, if feeder.is_some() => { released = Some(r.unwrap()); feeder = None; }
            ev = erx.recv() => match ev {
                Some(volc::Event::Connected(d)) => println!("connect {} ms", d.as_millis()),
                Some(volc::Event::Text(t)) => println!("{:6.2}s  {t}", t0.elapsed().as_secs_f64()),
                Some(volc::Event::Final(r)) => {
                    let ms = released.map_or(-1, |r: Instant| r.elapsed().as_millis() as i64);
                    match r {
                        Ok(t) => println!("final ({ms} ms after release): {t}"),
                        Err(e) => { println!("error: {e}"); std::process::exit(1) }
                    }
                    return;
                }
                None => return,
            }
        }
    }
}

async fn mic_test(out: &str, secs: u64) -> Result<(), String> {
    let (tx, mut rx) = mpsc::channel(1024);
    let t = Instant::now();
    let cap = audio::start(tx)?;
    println!("device: {} (start {} ms)", cap.device, t.elapsed().as_millis());
    let mut pcm = vec![];
    let _ = tokio::time::timeout(Duration::from_secs(secs), async {
        while let Some(c) = rx.recv().await {
            pcm.extend(c);
        }
    })
    .await;
    drop(cap);
    // The channel must close right after Capture drops; otherwise the Volc session never sends its last frame.
    tokio::time::timeout(Duration::from_millis(500), async {
        while let Some(c) = rx.recv().await {
            pcm.extend(c);
        }
    })
    .await
    .map_err(|_| "audio channel still open 500 ms after stop".to_string())?;
    let samples: Vec<i16> = pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    write_wav(out, &samples)?;
    println!("{} -> {out}", level(&samples));
    Ok(())
}

fn write_wav(path: &str, samples: &[i16]) -> Result<(), String> {
    let spec = hound::WavSpec { channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for s in samples {
        w.write_sample(*s).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())
}

fn level(samples: &[i16]) -> String {
    let rms = (samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64).sqrt();
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    format!("{:.2}s, RMS {rms:.0}, peak {peak}", samples.len() as f64 / 16000.0)
}

fn store_key() -> Result<(), String> {
    let mut k = String::new();
    std::io::stdin().read_to_string(&mut k).map_err(|e| e.to_string())?;
    let k = k.trim();
    if k.is_empty() {
        return Err("empty key on stdin".into());
    }
    use security_framework::os::macos::keychain::SecKeychain;
    let _lock = SecKeychain::disable_user_interaction().map_err(|e| e.to_string())?;
    security_framework::passwords::set_generic_password(KEY_SERVICES[0], "apiKey", k.as_bytes()).map_err(|e| e.to_string())?;
    println!("stored {} chars in {}", k.chars().count(), KEY_SERVICES[0]);
    Ok(())
}

// ---------- app ----------

fn hud_test(app: AppHandle, t0: Instant, ready: std::sync::mpsc::Receiver<()>, hold: u64) {
    let wait = Instant::now();
    if ready.recv_timeout(Duration::from_secs(10)).is_err() {
        log("hud-test: webview never reported ready");
        app.exit(2);
        return;
    }
    log(format!("hud-test: webview ready {} ms after main (waited {} ms)", t0.elapsed().as_millis(), wait.elapsed().as_millis()));
    let p = |tag: &str| {
        let r = hud::on_main(&app, |h| hud::probe(h));
        log(format!("hud-test: {tag}: {r:?}"));
        r
    };
    let before = p("before show");
    let shown = Instant::now();
    hud::show(&app, "listening", "测试 HUD：不应抢走焦点 — focus must stay where it was");
    std::thread::sleep(Duration::from_millis(300));
    let after = p("300 ms after show");
    std::thread::sleep(Duration::from_millis(700));
    let late = p("1 s after show");
    hud::show(&app, "finishing", "收尾状态 finishing");
    std::thread::sleep(Duration::from_millis(hold * 1000));
    let end = p("end of hold");
    let same = |a: &Option<hud::Probe>, b: &Option<hud::Probe>| a.as_ref().map(|x| x.frontmost.0) == b.as_ref().map(|x| x.frontmost.0);
    let ok = same(&before, &after)
        && same(&before, &late)
        && same(&before, &end)
        && [&after, &late, &end].iter().all(|x| x.as_ref().is_some_and(|x| !x.app_active && !x.panel_key && x.panel_visible));
    log(format!("hud-test: {} (shown for {} ms)", if ok { "PASS focus unchanged, panel visible, not key" } else { "FAIL" }, shown.elapsed().as_millis()));
    hud::hide(&app);
    std::thread::sleep(Duration::from_millis(100));
    app.exit(if ok { 0 } else { 1 });
}

const HOLD: Duration = Duration::from_millis(300);
const TAIL: Duration = Duration::from_millis(200);

async fn controller(app: AppHandle) {
    let code: i64 = std::env::var("PLAID_SPIKE_KEY").ok().and_then(|s| s.parse().ok()).unwrap_or(54);
    let ax = hotkey::accessibility();
    log(format!("permissions: accessibility {ax}"));
    let (ktx, mut keys) = mpsc::unbounded_channel();
    // The app has no window or Dock icon, so the HUD is the only feedback. Gate on trust rather than on tap
    // creation: a tap can be created without the permission and then never receive events.
    if !ax {
        hud::show(&app, "error", "需要「辅助功能」权限：系统设置 → 隐私与安全性 → 辅助功能 → 打开 Plaid Spike");
        while !hotkey::trusted() {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        log("accessibility granted");
    }
    if let Err(e) = hotkey::spawn(code, ktx.clone()) {
        log(format!("hotkey: {e}"));
        hud::show(&app, "error", &format!("热键启动失败：{e}"));
        return;
    }
    drop(ktx);
    log(format!("ready: hold key {code}"));
    hud::show(&app, "info", if code == 54 { "Plaid Spike 已就绪，按住右 ⌘ 说话" } else { "Plaid Spike 已就绪" });
    tokio::time::sleep(Duration::from_secs(2)).await;
    hud::hide(&app);
    let mut armed: Option<Instant> = None;
    let mut cap: Option<audio::Capture> = None;
    let mut events: Option<mpsc::UnboundedReceiver<volc::Event>> = None;
    let mut latest = String::new();
    let mut t = (Instant::now(), None::<Instant>); // (recording start, release)
    loop {
        let deadline = armed.map(|a| tokio::time::Instant::from_std(a + HOLD));
        tokio::select! {
            k = keys.recv() => match k {
                Some(hotkey::Key::Down(at)) => if cap.is_none() && events.is_none() { armed = Some(at) },
                Some(hotkey::Key::Other) => armed = None,
                Some(hotkey::Key::Up) => {
                    armed = None;
                    if cap.is_some() {
                        hud::show(&app, "finishing", &latest);
                        tokio::time::sleep(TAIL).await;
                        // Flushes the last chunk and closes the audio channel → last frame.
                        let pcm = cap.take().unwrap().stop();
                        t.1 = Some(Instant::now());
                        let wav = std::env::var("HOME").unwrap_or_default() + "/Library/Logs/plaid-spike-last.wav";
                        let saved = write_wav(&wav, &pcm).map_or_else(|e| format!("wav: {e}"), |_| format!("-> {wav}"));
                        log(format!("audio: {} {saved}", level(&pcm)));
                        // A denied mic delivers exact zeros; say so instead of returning an empty result.
                        if pcm.iter().all(|&s| s == 0) {
                            events = None;
                            hud::show(&app, "error", "麦克风没有声音：系统设置 → 隐私与安全性 → 麦克风 → 打开 Plaid Spike");
                            tokio::time::sleep(Duration::from_secs(4)).await;
                            hud::hide(&app);
                        }
                    }
                }
                None => return,
            },
            _ = async { tokio::time::sleep_until(deadline.unwrap()).await }, if deadline.is_some() => {
                armed = None;
                let key = match api_key() {
                    Ok(k) => k,
                    Err(e) => { log(&e); hud::show(&app, "error", "no API key"); continue }
                };
                let (atx, arx) = mpsc::channel(256);
                let (etx, erx) = mpsc::unbounded_channel();
                let s = Instant::now();
                match audio::start(atx) {
                    Ok(c) => { log(format!("mic: {} (start {} ms)", c.device, s.elapsed().as_millis())); cap = Some(c) }
                    Err(e) => { log(format!("mic: {e}")); hud::show(&app, "error", &e); continue }
                }
                t = (s, None);
                latest.clear();
                tauri::async_runtime::spawn(volc::session(options(key), arx, etx));
                events = Some(erx);
                hud::show(&app, "listening", "");
            },
            ev = async { events.as_mut().unwrap().recv().await }, if events.is_some() => match ev {
                Some(volc::Event::Connected(d)) => log(format!("connect {} ms", d.as_millis())),
                Some(volc::Event::Text(s)) => {
                    latest = s;
                    hud::show(&app, if cap.is_some() { "listening" } else { "finishing" }, &latest);
                }
                Some(volc::Event::Final(r)) => {
                    events = None;
                    cap = None;
                    let after = t.1.map_or(-1, |r| r.elapsed().as_millis() as i64);
                    log(format!("final {after} ms after release, {} ms total: {}", t.0.elapsed().as_millis(), match &r { Ok(t) | Err(t) => t }));
                    match r {
                        Ok(text) if !text.is_empty() => paste(&app, text).await,
                        Ok(_) => hud::hide(&app),
                        Err(e) => { hud::show(&app, "error", &e); tokio::time::sleep(Duration::from_secs(2)).await; hud::hide(&app) }
                    }
                }
                None => { events = None; cap = None; hud::hide(&app) }
            },
        }
    }
}

async fn paste(app: &AppHandle, text: String) {
    let (snap, owned) = hud::on_main(app, move |_| {
        let b = paste::general();
        let snap = paste::snapshot(&b);
        let owned = paste::write_text(&b, &text);
        paste::post_cmd_v();
        (snap, owned)
    });
    hud::hide(app);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let restored = hud::on_main(app, move |_| paste::restore(&paste::general(), &snap, owned));
    log(format!("paste: clipboard {}", if restored { "restored" } else { "left alone (changed since paste)" }));
}

fn run_app(hud_test_secs: Option<u64>) {
    let t0 = Instant::now();
    tauri::Builder::default()
        // tao calls activateIgnoringOtherApps(true) at launch by default, which takes focus from the user's app.
        .activate_ignoring_other_apps(false)
        .plugin(tauri_nspanel::init())
        .setup(move |app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let (rtx, rrx) = std::sync::mpsc::channel();
            app.listen("hud-ready", move |_| {
                let _ = rtx.send(());
            });
            app.listen("hud-rendered", |e| log(format!("hud rendered: {}", e.payload())));
            hud::create(app.handle())?;
            log(format!("panel created {} ms after main", t0.elapsed().as_millis()));
            let h = app.handle().clone();
            match hud_test_secs {
                Some(secs) => drop(std::thread::spawn(move || hud_test(h, t0, rrx, secs))),
                None => drop(tauri::async_runtime::spawn(controller(h))),
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("tauri");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|a| a == name).map(|i| args[i + 1..].iter().take_while(|a| !a.starts_with("--")).cloned().collect::<Vec<_>>());
    let rt = || tokio::runtime::Runtime::new().expect("runtime");
    if let Some(files) = arg("--file") {
        rt().block_on(async {
            for f in &files {
                stream_file(f).await;
            }
        });
    } else if arg("--paste-test").is_some() {
        match paste::self_test() {
            Ok(()) => println!("paste-test: PASS"),
            Err(e) => {
                println!("paste-test: FAIL {e}");
                std::process::exit(1)
            }
        }
    } else if let Some(a) = arg("--mic-test") {
        let out = a.first().map_or("mic.wav", String::as_str).to_owned();
        let secs = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(5);
        if let Err(e) = rt().block_on(mic_test(&out, secs)) {
            println!("mic-test: {e}");
            std::process::exit(1)
        }
    } else if arg("--store-key").is_some() {
        if let Err(e) = store_key() {
            eprintln!("store-key: {e}");
            std::process::exit(1)
        }
    } else if let Some(a) = arg("--hud-test") {
        run_app(Some(a.first().and_then(|s| s.parse().ok()).unwrap_or(2)));
    } else {
        run_app(None);
    }
}
