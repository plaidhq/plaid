//! `plaid --snapshot <outdir> [page…]`: renders settings pages offscreen to PNGs, with fixture history written
//! into PLAID_DATA_DIR (required, so a real data folder is never touched) and fixture OS facts (no keychain, no
//! audio devices, no window on screen). `plaid --ui-check <outdir>` also clicks through the pages (see `check`).
use super::{Facts, Model, Page, Settings, mav};
use crate::config;
use chrono::{Duration, TimeZone, Utc};
use gpui_kit::*;
use plaid_core::store::{Record, Store};
use std::path::Path;
use std::sync::Arc;

pub fn run(args: &[String]) -> Result<(), String> {
    let Some(out) = args.first() else {
        return Err("usage: plaid --snapshot <outdir> [page…]".into());
    };
    match args.get(1).map(String::as_str) {
        Some("hud") => return hud(out),
        Some("onboarding") => return onboarding(out),
        _ => {}
    }
    let pages: Vec<Page> = if args.len() > 1 {
        args[1..]
            .iter()
            .map(|a| {
                Page::ALL
                    .into_iter()
                    .find(|p| p.slug() == a)
                    .ok_or(format!("unknown page {a}"))
            })
            .collect::<Result<_, _>>()?
    } else {
        Page::ALL.to_vec()
    };
    let mut ui = Ui::new(out)?;
    for page in pages {
        ui.open(page, facts())?;
        ui.shot(page.slug())?;
        ui.close()?;
    }
    Ok(())
}

/// `plaid --snapshot <outdir> hud`: each HUD state over a desktop-like backdrop, one PNG per state.
fn hud(out: &str) -> Result<(), String> {
    use plaid_core::dictation::Hud as H;
    struct Backdrop(Entity<crate::hud::Hud>);
    impl Render for Backdrop {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .bg(linear_gradient(
                    165.,
                    linear_color_stop(mav::hex(0x7f9aba), 0.),
                    linear_color_stop(mav::hex(0x1f334d), 1.),
                ))
                .child(self.0.clone())
        }
    }
    let partial =
        "好的，那我们周四下午三点在公司楼下见，记得带上那份合同，还有上次说的报价单也一起带过来";
    let text = |t: &str, command| H::Text {
        text: t.into(),
        command,
    };
    let levels = [400., 1500., 3000., 800., 2000., 300.].map(H::Level);
    let states: Vec<(&str, Vec<H>)> = vec![
        ("hud-listen-empty", vec![H::Listen]),
        (
            "hud-listen",
            [vec![H::Listen], levels.to_vec(), vec![text(partial, false)]].concat(),
        ),
        (
            "hud-continuous",
            [
                vec![H::Listen, H::Continuous],
                levels.to_vec(),
                vec![text("周四下午三点", false)],
            ]
            .concat(),
        ),
        ("hud-command", vec![H::Listen, text("Plaid 打开日历", true)]),
        (
            "hud-quiet",
            vec![
                H::Listen,
                H::Level(20.),
                H::Quiet("几乎没有声音 · MacBook Pro 麦克风".into()),
            ],
        ),
        (
            "hud-finishing",
            vec![H::Listen, text(partial, false), H::Finishing],
        ),
        ("hud-refining", vec![H::Refining(partial.into())]),
        ("hud-done", vec![H::Done(partial.into())]),
        (
            "hud-saved",
            vec![H::Saved(
                "输入位置已变化或无法确认，本次结果没有自动粘贴。".into(),
            )],
        ),
        ("hud-flash", vec![H::Flash("学会：克劳德 → Claude".into())]),
        (
            "hud-failed",
            vec![H::Failed("HTTP 401；可在历史里查看".into())],
        ),
        ("hud-ask", vec![H::Ask("打开日历".into())]),
        ("hud-running", vec![H::Running("打开日历".into())]),
        (
            "hud-ran",
            vec![H::Running("天气".into()), H::Done("天气".into())],
        ),
        ("hud-note", vec![H::Note("识别结果已保存在原记录中".into())]),
    ];
    let mut ui = Ui::new(out)?;
    for (name, msgs) in states {
        let mut opts = crate::hud::options(point(px(0.), px(0.)));
        opts.window_background = WindowBackgroundAppearance::Opaque;
        ui.open_with(opts, |_, cx| {
            let hud = cx.new(|_| {
                let mut h = crate::hud::Hud::new();
                for m in msgs {
                    h.apply(m);
                }
                h
            });
            cx.new(|_| Backdrop(hud))
        })?;
        ui.shot(name)?;
        ui.close()?;
    }
    Ok(())
}

/// `plaid --snapshot <outdir> onboarding`: each first-run step; the key and permission steps also before granting.
fn onboarding(out: &str) -> Result<(), String> {
    use super::onboarding::{Onboarding, window_options};
    let mut ui = Ui::new(out)?;
    let missing = Facts {
        volc_key: false,
        trusted: false,
        ..facts()
    };
    let shots = [
        ("onboarding-0", 0, facts()),
        ("onboarding-1-missing", 1, missing.clone()),
        ("onboarding-1", 1, facts()),
        ("onboarding-2-missing", 2, missing),
        ("onboarding-2", 2, facts()),
        ("onboarding-3", 3, facts()),
        ("onboarding-4", 4, facts()),
    ];
    for (name, step, f) in shots {
        ui.open_with(window_options(None, false), |window, cx| {
            let model = cx.new(|_| Model::new(f));
            cx.new(|cx| Onboarding::new(step, model, window, cx))
        })?;
        ui.shot(name)?;
        ui.close()?;
    }
    Ok(())
}

/// `plaid --ui-check <outdir>`: drives each page offscreen by clicking controls by id, asserts on the files the
/// clicks write, and saves a PNG of each interesting state. Exits non-zero on the first failed assertion.
pub fn check(args: &[String]) -> Result<(), String> {
    let Some(out) = args.first() else {
        return Err("usage: plaid --ui-check <outdir>".into());
    };
    let mut ui = Ui::new(out)?;
    check_general(&mut ui)?;
    super::overview::check(&mut ui)?;
    super::perf::check(&mut ui)?;
    super::history::check(&mut ui)?;
    super::dictionary::check(&mut ui)?;
    super::refine::check(&mut ui)?;
    super::commands::check(&mut ui)?;
    ui.check_close()?;
    println!("ui-check: PASS");
    Ok(())
}

fn check_general(ui: &mut Ui) -> Result<(), String> {
    ui.open(Page::General, facts())?;
    let before = config::raw("asrContext");
    ui.click("asrContext")?;
    expect(
        "asrContext unchecked",
        config::raw("asrContext").as_deref() == Some("0"),
        &before,
    )?;
    ui.click("asrContext")?;
    expect(
        "asrContext checked",
        config::raw("asrContext").as_deref() == Some("1"),
        &None,
    )?;

    ui.click("advanced")?;
    ui.shot("general-advanced")?;
    let silence = config::value("silenceRMS");
    ui.click("silenceRMS-1")?;
    let up = config::value("silenceRMS");
    expect("silence stepper up", up.as_deref() == Some("510"), &silence)?;
    ui.click("silenceRMS--1")?;
    ui.click("silenceRMS--1")?;
    expect(
        "silence stepper down",
        config::value("silenceRMS").as_deref() == Some("490"),
        &up,
    )?;

    ui.click("hold")?;
    ui.shot("general-menu")?;
    let want = super::general::offered()[0];
    ui.click("hold-menu-0")?;
    expect(
        "hold key picked from menu",
        config::value("holdKey") == Some(want.to_string())
            && config::value("holdKeyName") == Some(super::general::key_name(want)),
        &None,
    )?;
    expect("menu closed after pick", !ui.has("hold-menu-0"), &None)?;
    let y = ui.bounds("reveal").map(|b| b.origin.y);
    ui.scroll("asrContext", -400.)?;
    let y2 = ui.bounds("reveal").map(|b| b.origin.y);
    expect(
        "page scrolls",
        matches!((y, y2), (Some(a), Some(b)) if b < a),
        &None,
    )?;
    ui.shot("general-scrolled")?;
    ui.close()?;

    ui.open(
        Page::General,
        Facts {
            volc_key: false,
            ..facts()
        },
    )?;
    expect(
        "key field in edit mode",
        ui.has("key-save") && !ui.has("key-change"),
        &None,
    )?;
    ui.shot("general-nokey")?;
    ui.close()
}

pub(super) fn expect(what: &str, ok: bool, was: &Option<String>) -> Result<(), String> {
    if ok {
        println!("  ok  {what}");
        Ok(())
    } else {
        Err(format!("FAIL {what} (before: {was:?})"))
    }
}

/// One offscreen app with at most one settings window open at a time.
pub(super) struct Ui {
    cx: HeadlessAppContext,
    out: std::path::PathBuf,
    window: Option<AnyWindowHandle>,
}

impl Ui {
    fn new(out: &str) -> Result<Ui, String> {
        if std::env::var_os("PLAID_DATA_DIR").is_none() {
            return Err("set PLAID_DATA_DIR to a scratch folder".into());
        }
        fixture(&config::data_dir()).map_err(|e| format!("fixture: {e}"))?;
        std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
        let platform = gpui_kit::platform::current_platform(true);
        let mut cx = HeadlessAppContext::with_platform(
            platform.text_system(),
            Arc::new(super::assets::Assets),
            gpui_kit::platform::current_headless_renderer,
        );
        cx.update(gpui_kit::init);
        Ok(Ui {
            cx,
            out: out.into(),
            window: None,
        })
    }

    pub(super) fn open(&mut self, page: Page, facts: Facts) -> Result<(), String> {
        self.open_with(super::window_options(None, false), |window, cx| {
            let model = cx.new(|_| Model::new(facts));
            cx.new(|cx| Settings::new(page, model, window, cx))
        })
    }

    pub(super) fn open_with<V: Render>(
        &mut self,
        opts: WindowOptions,
        build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
    ) -> Result<(), String> {
        let (handle, _) = self
            .cx
            .update(|cx| gpui_kit::open_window(opts, cx, build))
            .map_err(|e| e.to_string())?;
        self.window = Some(handle);
        self.frame()
    }

    /// Two frames: the first lays out lazily created views, the second paints them settled. Probes are rebuilt.
    pub(super) fn frame(&mut self) -> Result<(), String> {
        let handle = self.window.ok_or("no window")?;
        for _ in 0..2 {
            mav::PROBES.with(|p| p.borrow_mut().clear());
            self.cx
                .update_window(handle, |_, window, cx| {
                    window.refresh();
                    window.draw(cx).clear(cx);
                })
                .map_err(|e| e.to_string())?;
            self.cx.run_until_parked();
        }
        Ok(())
    }

    /// ⌘W (Ctrl+W off macOS) closes the active window.
    fn check_close(&mut self) -> Result<(), String> {
        self.cx.update(super::bind_keys);
        self.open(Page::General, facts())?;
        let handle = self.window.take().ok_or("no window")?;
        let key = if cfg!(target_os = "macos") {
            "cmd-w"
        } else {
            "ctrl-w"
        };
        let active = self.cx.update(|cx| cx.active_window()) == Some(handle);
        self.cx
            .update_window(handle, |_, window, cx| {
                window.dispatch_keystroke(Keystroke::parse(key).unwrap(), cx)
            })
            .map_err(|e| e.to_string())?;
        self.cx.run_until_parked();
        let open = self.cx.update(|cx| cx.windows().contains(&handle));
        if open {
            return Err(format!(
                "FAIL {key} left the window open (active: {active})"
            ));
        }
        Ok(())
    }

    pub(super) fn has(&self, id: &str) -> bool {
        mav::PROBES.with(|p| p.borrow().contains_key(id))
    }

    /// Clicks the center of the control painted with this id, then renders the result.
    pub(super) fn click(&mut self, id: &str) -> Result<(), String> {
        let handle = self.window.ok_or("no window")?;
        let Some(b) = mav::PROBES.with(|p| p.borrow().get(id).copied()) else {
            return Err(format!("FAIL no control {id:?} on screen"));
        };
        let position = b.center();
        self.cx
            .update_window(handle, |_, window, cx| {
                let modifiers = Modifiers::default();
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        modifiers,
                        pressed_button: None,
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers,
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers,
                        click_count: 1,
                    }),
                    cx,
                );
            })
            .map_err(|e| e.to_string())?;
        self.cx.run_until_parked();
        self.frame()
    }

    /// Scrolls the pane under the control with this id by `dy` pixels (negative scrolls content up).
    pub(super) fn scroll(&mut self, id: &str, dy: f32) -> Result<(), String> {
        let handle = self.window.ok_or("no window")?;
        let Some(b) = mav::PROBES.with(|p| p.borrow().get(id).copied()) else {
            return Err(format!("FAIL no control {id:?} on screen"));
        };
        self.cx
            .update_window(handle, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: b.center(),
                        modifiers: Modifiers::default(),
                        pressed_button: None,
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::Scroll(ScrollEvent {
                        position: b.center(),
                        delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
                        ..Default::default()
                    }),
                    cx,
                );
            })
            .map_err(|e| e.to_string())?;
        self.frame()
    }

    pub(super) fn bounds(&self, id: &str) -> Option<Bounds<Pixels>> {
        mav::PROBES.with(|p| p.borrow().get(id).copied())
    }

    pub(super) fn shot(&mut self, name: &str) -> Result<(), String> {
        let handle = self.window.ok_or("no window")?;
        let img = self
            .cx
            .capture_screenshot(handle)
            .map_err(|e| e.to_string())?;
        let path = self.out.join(format!("{name}.png"));
        img.save(&path).map_err(|e| e.to_string())?;
        println!("{}", path.display());
        Ok(())
    }

    /// Types into the focused field, one keystroke per character (`window.dispatch_keystroke`, which feeds the
    /// focused input handler), then renders.
    pub(super) fn type_text(&mut self, text: &str) -> Result<(), String> {
        for c in text.chars() {
            let key = match c {
                ' ' => "space".to_string(),
                c => c.to_string(),
            };
            self.key_stroke(Keystroke {
                modifiers: Modifiers::default(),
                key,
                key_char: None,
            })?;
        }
        self.frame()
    }

    /// One named keystroke, e.g. "enter", "cmd-a", "tab", then renders.
    pub(super) fn key(&mut self, name: &'static str) -> Result<(), String> {
        self.key_stroke(Keystroke::parse(name).map_err(|e| format!("{name}: {e}"))?)?;
        self.frame()
    }

    fn key_stroke(&mut self, k: Keystroke) -> Result<(), String> {
        let handle = self.window.ok_or("no window")?;
        self.cx
            .update_window(handle, |_, window, cx| {
                window.dispatch_keystroke(k, cx);
            })
            .map_err(|e| e.to_string())?;
        self.cx.run_until_parked();
        Ok(())
    }

    pub(super) fn close(&mut self) -> Result<(), String> {
        if let Some(handle) = self.window.take() {
            self.cx
                .update_window(handle, |_, window, _| window.remove_window())
                .map_err(|e| e.to_string())?;
            self.cx.run_until_parked();
        }
        Ok(())
    }
}

pub(super) fn facts() -> Facts {
    Facts {
        volc_key: true,
        mics: vec![
            (
                "BuiltInMicrophoneDevice".into(),
                "MacBook Pro 麦克风".into(),
            ),
            ("AppleUSBAudioEngine:Shure:MV7".into(), "Shure MV7".into()),
        ],
        default_mic: Some("MacBook Pro 麦克风".into()),
        trusted: true,
        fixture: true,
    }
}

const SENTENCES: [(&str, &str); 12] = [
    ("帮我把这个函数改成异步的", "帮我把这个函数改成异步的。"),
    (
        "明天下午三点开会讨论一下发布计划",
        "明天下午三点开会，讨论一下发布计划。",
    ),
    (
        "这个 bug 是因为缓存没有失效",
        "这个 bug 是因为缓存没有失效。",
    ),
    ("我觉得可以先上线灰度版本", "我觉得可以先上线灰度版本。"),
    (
        "把 docker compose 里的端口改成八零八零",
        "把 Docker Compose 里的端口改成 8080。",
    ),
    ("周五之前把文档发给我", "周五之前把文档发给我。"),
    (
        "这个接口的延迟大概在两百毫秒左右",
        "这个接口的延迟大概在 200 毫秒左右。",
    ),
    ("好的没问题", "好的，没问题。"),
    ("麻烦看一下这个 PR", "麻烦看一下这个 PR。"),
    ("用 go 写一个简单的反向代理", "用 Go 写一个简单的反向代理。"),
    ("今晚的会议改到明天", "今晚的会议改到明天。"),
    (
        "把 kubernetes 的配置也一起更新",
        "把 Kubernetes 的配置也一起更新。",
    ),
];

const APPS: [&str; 5] = ["Slack", "Visual Studio Code", "Safari", "微信", "Terminal"];

/// Deterministic history over the last ~40 days (only when the folder has none yet).
pub(super) fn fixture(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    if !dir.join("records.jsonl").exists() {
        let mut store = Store::load(dir);
        let now = Utc.with_ymd_and_hms(2026, 11, 18, 9, 0, 0).unwrap();
        let mut seed: u64 = 7;
        let mut next = move |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n.max(1)
        };
        let mut records = vec![];
        for day in (0..40).rev() {
            let n = next(9) + if day % 7 < 5 { 3 } else { 0 };
            for i in 0..n {
                let at = now - Duration::days(day) - Duration::minutes((next(600) + i * 7) as i64);
                let (raw, text) = SENTENCES[next(SENTENCES.len() as u64) as usize];
                let audio_ms = 1500 + next(6000) as i64;
                let llm_on = next(3) == 0;
                let mut r = Record {
                    at,
                    app: Some(APPS[next(APPS.len() as u64) as usize].into()),
                    audio_ms,
                    first_text_ms: Some(280 + next(250) as i64),
                    asr_ms: 180 + next(300) as i64,
                    llm_ms: llm_on.then(|| 400 + next(900) as i64),
                    llm: if llm_on { "ok" } else { "off" }.into(),
                    total_ms: 0,
                    raw: raw.into(),
                    text: text.into(),
                    noise_rms: Some(40 + next(60) as i64),
                    speech_rms: Some(600 + next(1800) as i64),
                    engine: Some("volc".into()),
                    mic: Some("MacBook Pro 麦克风".into()),
                    trigger_ms: Some(next(40) as i64),
                    mic_start_ms: Some(20 + next(60) as i64),
                    ..Default::default()
                };
                r.total_ms = r.asr_ms + r.llm_ms.unwrap_or(0) + 30;
                if next(25) == 0 {
                    r.outcome = Some("failed".into());
                    r.error = Some("ASR 超时".into());
                    r.text.clear();
                }
                records.push(r);
            }
        }
        records.sort_by_key(|r| r.at);
        // Hand corrections that the dictionary page offers as fixes.
        for (k, (from, to)) in [
            ("cooper netes", "Kubernetes"),
            ("杰森", "JSON"),
            ("瑞迪斯", "Redis"),
        ]
        .iter()
        .enumerate()
        {
            for j in 0..(3 - k) {
                let i = records.len() - 5 - k * 4 - j;
                let r = &mut records[i];
                r.pasted = Some(format!("把 {from} 的配置也一起更新。"));
                r.text = r.pasted.clone().unwrap();
                r.edited = Some(format!("把 {to} 的配置也一起更新。"));
            }
        }
        if let Some(r) = records.last_mut() {
            r.speech_rms = Some(820);
            r.noise_rms = Some(64);
        }
        for r in records {
            store.add(r);
        }
    }
    let write = |name: &str, body: &str| -> std::io::Result<()> {
        let p = dir.join(name);
        if !p.exists() {
            std::fs::write(p, body)?;
        }
        Ok(())
    };
    write("hotwords.txt", "Plaid\nKubernetes\nGPUI\n火山引擎\n")?;
    write(
        "actions.json",
        r#"[{"name":"打开终端","phrases":["打开终端"],"run":"open -a Terminal"},{"name":"翻译","phrases":["翻译"],"run":"pbpaste | trans","input":"selection"}]"#,
    )?;
    write(
        "settings.json",
        r#"{"llmEnabled":"1","llmModel":"deepseek-chat"}"#,
    )?;

    // --- History page (ui/history.rs) ---------------------------------------------------------------------------
    // One second of silence for the newest failed record, so its retry is offered, and two records saved without
    // paste for the 未粘贴 filter. Only once per fixture folder.
    let mut store = Store::load(dir);
    if !store.all.is_empty() && !store.all.iter().any(|r| r.audio.is_some()) {
        if let Some(at) = store
            .all
            .iter()
            .rev()
            .find(|r| r.is_failure())
            .map(|r| r.at)
            && let Some(name) = store.save_audio(&vec![0u8; 32000], at)
        {
            store.update(at, |r| r.audio = Some(name));
        }
        let n = store.all.len();
        for back in [2, 20] {
            if let Some(at) = store.all[..n.saturating_sub(back) + 1]
                .iter()
                .rev()
                .find(|r| !r.is_failure())
                .map(|r| r.at)
            {
                store.update(at, |r| r.outcome = Some("saved".into()));
            }
        }
    }
    // --- Perf page fixture: an events.log, plus LLM fallbacks, a saved-only result, a quiet mic and two local-engine
    // records among recent plain (LLM off, unedited, not failed) records so every Perf section has something to show.
    if !dir.join("events.log").exists() {
        let mut store = Store::load(dir);
        let n = store.all.len();
        let picks: Vec<chrono::DateTime<Utc>> = store.all
            [n.saturating_sub(70)..n.saturating_sub(5)]
            .iter()
            .filter(|r| r.llm == "off" && r.edited.is_none() && !r.is_failure())
            .step_by(3)
            .map(|r| r.at)
            .collect();
        for (k, at) in picks.into_iter().take(10).enumerate() {
            store.update(at, |r| match k {
                0 | 1 => {
                    r.llm = "timeout".into();
                    r.llm_ms = Some(2000);
                    r.total_ms += 2000;
                }
                2 => r.llm = "skipped".into(),
                3 => {
                    r.llm = "error: HTTP 500".into();
                    r.llm_ms = Some(640);
                    r.total_ms += 640;
                }
                4 => r.outcome = Some("saved".into()),
                5..=7 => {
                    r.mic = Some("Shure MV7".into());
                    r.speech_rms = Some(120);
                }
                _ => r.engine = Some("local".into()),
            });
        }
        let now = chrono::Local::now();
        let lines = [
            (26, 0, "launch 0.11.0"),
            (26, 10, "key down 63"),
            (26, 11, "start"),
            (26, 14, "key up 63"),
            (26, 14, "stop 3.1s"),
            (26, 15, "finished 312 ms"),
            (26, 16, "pasted 18 chars into Slack"),
            (3, 0, "key down 63"),
            (3, 1, "start"),
            (3, 4, "key up 63"),
            (3, 4, "stop 2.4s"),
            (3, 6, "asr failed: timeout"),
            (2, 0, "key down 63"),
            (2, 1, "start"),
            (2, 3, "esc"),
            (1, 0, "key down 63"),
            (1, 1, "start"),
            (1, 4, "stop 3.6s"),
            (1, 5, "empty result"),
            (0, 0, "llm error: HTTP 500"),
        ];
        let body: String = lines
            .iter()
            .map(|(h, s, msg)| {
                let t = now - Duration::hours(*h) - Duration::minutes(10) + Duration::seconds(*s);
                format!("{} {msg}\n", t.format("%Y-%m-%dT%H:%M:%S%.3f%:z"))
            })
            .collect();
        std::fs::write(dir.join("events.log"), body)?;
    }
    // --- 词典 page fixture: a JSON term (so 杰森 -> JSON becomes a learned rule), an alias line with a block
    // list, and a replacement.
    let hotwords = dir.join("hotwords.txt");
    let mut body = std::fs::read_to_string(&hotwords)?;
    for line in ["JSON", "Tauri | 陶瑞 | 套瑞 ~~~ 金牛座", "克劳德 -> Claude"] {
        if !body.split('\n').any(|l| l == line) {
            body.push_str(line);
            body.push('\n');
        }
    }
    std::fs::write(&hotwords, body)?;
    Ok(())
}
