// Plaid: hold-to-talk dictation.
//   plaid                 normal run
//   plaid --hud-test [s]  show the HUD, log activation/key-window facts, quit (exit 1 if focus moved)
//   plaid --file a.wav    one utterance through the controller with the WAV as the mic; prints partials and the paste
//   plaid --recover [at]  recognize a record's saved audio again (default: newest with audio), save in place
//   plaid --store-key     read the API key from stdin into this app's credential store
//   plaid --snapshot <dir> [page]  (feature `snapshot`) render settings pages offscreen to PNGs with fixture data
// Windows release builds are GUI-subsystem (no console window); redirected stdin/stdout still work.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod audio;
mod cli;
mod config;
mod driver;
mod hotkey;
mod hud;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
mod paste;
#[cfg(windows)]
#[path = "paste_win.rs"]
mod paste;
mod run;
mod secret;
mod trace;
mod tray;
mod ui;
#[cfg(windows)]
mod win;

#[cfg(target_os = "macos")]
use mac as os;
#[cfg(windows)]
use win as os;

use gpui_kit::*;
use plaid_core::dictation::{Controller, Msg};
use plaid_core::store::Store;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let headless = match args.get(1).map(String::as_str) {
        Some("--file") => Some(cli::replay(args.get(2).map_or("", String::as_str))),
        Some("--recover") => Some(cli::recover(args.get(2).map(String::as_str))),
        Some("--store-key") => Some(cli::store_key()),
        #[cfg(all(feature = "snapshot", any(target_os = "macos", target_os = "linux")))]
        Some("--snapshot") => Some(ui::snapshot::run(&args[2..])),
        _ => None,
    };
    if let Some(r) = headless {
        if let Err(e) = r {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return;
    }
    gpui_kit::application()
        .with_assets(ui::assets::Assets)
        .with_quit_mode(QuitMode::Explicit)
        .with_windowing(WindowingRequest::Headless)
        .run(move |cx| {
            gpui_kit::init(cx);
            let hud = hud::open(cx).expect("hud window");
            if let Err(e) = tray::install(cx, |c, cx| match c {
                tray::Command::Quit => cx.quit(),
                tray::Command::History => ui::open(ui::Page::History, cx),
                tray::Command::Settings => ui::open(ui::Page::General, cx),
            }) {
                eprintln!("tray: {e}");
            }
            if args.get(1).map(String::as_str) == Some("--hud-test") {
                let hold = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2);
                return hud_test(hud, hold, cx);
            }
            run(hud, cx);
        });
}

/// The dictation loop: hotkey, mic and ASR threads post to one channel; the controller handles each message on
/// the main thread, then the HUD and the hotkey's view of the controller are updated.
fn run(hud: WindowHandle<hud::Hud>, cx: &mut App) {
    let (tx, rx) = async_channel::unbounded::<Msg>();
    let state = std::sync::Arc::new(hotkey::State::default());
    install_hotkey(tx.clone(), state.clone());
    cx.set_global(ui::Live {
        tx: tx.clone(),
        keys: state.clone(),
    });
    let mut driver = driver::Driver::live(
        tx,
        cx.foreground_executor().clone(),
        cx.background_executor().clone(),
    );
    driver.restore_clipboard_on_quit(cx);
    let mut ctl = Controller::new(Store::load(&config::data_dir()));
    trace::log(&format!(
        "launch {} data={}",
        env!("CARGO_PKG_VERSION"),
        config::data_dir().display()
    ));
    if !config::actions().0.is_empty() {
        crate::run::prewarm();
    }
    cx.spawn(async move |cx| {
        while let Ok(m) = rx.recv().await {
            ctl.handle(m, &mut driver);
            state.set(ctl.keys());
            let changes = std::mem::take(&mut driver.hud);
            let _ = cx.update(|cx| changes.into_iter().for_each(|h| hud::apply(&hud, h, cx)));
        }
    })
    .detach();
}

/// Starts the key hook. macOS: once Accessibility is granted (asks once at launch, then polls without prompting).
fn install_hotkey(tx: async_channel::Sender<Msg>, state: std::sync::Arc<hotkey::State>) {
    #[cfg(target_os = "macos")]
    let trusted = mac::request_trust();
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        if !trusted {
            trace::log("waiting for Accessibility");
            while !mac::trusted() {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
        let (hold, tap) = config::hotkeys();
        match hotkey::spawn(tx, state) {
            Ok(()) => trace::log(&format!("hotkey hold={hold} tap={tap}")),
            Err(e) => trace::log(&format!("hotkey: {e}")),
        }
    });
}

#[cfg(target_os = "macos")]
fn hud_test(hud: WindowHandle<hud::Hud>, hold: u64, cx: &mut App) {
    cx.spawn(async move |cx| {
        let probe = |tag: &str, cx: &mut AsyncApp| {
            let p = cx.update(|cx| {
                hud.update(cx, |_, w, _| hud::native::probe(w))
                    .ok()
                    .flatten()
            });
            eprintln!("hud-test: {tag}: {p:?}");
            p
        };
        let exec = cx.background_executor().clone();
        let sleep = |ms| exec.timer(std::time::Duration::from_millis(ms));
        let before = probe("before show", cx);
        cx.update(|cx| {
            hud::show(
                &hud,
                hud::Phase::Listening,
                "测试 HUD：不应抢走焦点 — focus must stay where it was",
                cx,
            )
        });
        sleep(300).await;
        let after = probe("300 ms after show", cx);
        sleep(700).await;
        let late = probe("1 s after show", cx);
        cx.update(|cx| hud::show(&hud, hud::Phase::Finishing, "收尾状态 finishing", cx));
        sleep(hold * 1000).await;
        let end = probe("end of hold", cx);
        cx.update(|cx| hud::hide(&hud, cx));
        sleep(100).await;
        let hidden = probe("after hide", cx);
        let pid = |p: &Option<hud::native::Probe>| p.as_ref().map(|x| x.frontmost.0);
        let ok = [&after, &late, &end, &hidden]
            .iter()
            .all(|p| pid(p) == pid(&before))
            && [&after, &late, &end].iter().all(|p| {
                p.as_ref()
                    .is_some_and(|x| !x.app_active && !x.hud_key && x.hud_visible)
            })
            && hidden
                .as_ref()
                .is_some_and(|x| !x.hud_visible && !x.app_active);
        eprintln!("hud-test: {}", if ok { "PASS" } else { "FAIL" });
        std::process::exit(if ok { 0 } else { 1 });
    })
    .detach();
}

#[cfg(not(target_os = "macos"))]
fn hud_test(_: WindowHandle<hud::Hud>, _: u64, _: &mut App) {}
