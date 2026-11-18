//! `plaid --snapshot <outdir> [page…]`: renders settings pages offscreen to PNGs, with fixture history written
//! into PLAID_DATA_DIR (required, so a real data folder is never touched) and fixture OS facts (no keychain, no
//! audio devices, no window on screen).
use super::{Facts, Model, Page, Settings};
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
    if std::env::var_os("PLAID_DATA_DIR").is_none() {
        return Err("set PLAID_DATA_DIR to a scratch folder".into());
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
    fixture(&config::data_dir()).map_err(|e| format!("fixture: {e}"))?;
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;

    let platform = gpui_kit::platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(
        platform.text_system(),
        Arc::new(super::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    for page in pages {
        let (handle, _) = cx
            .update(|cx| {
                let model = cx.new(|_| Model::new(facts()));
                gpui_kit::open_window(super::window_options(None, false), cx, |window, cx| {
                    cx.new(|cx| Settings::new(page, model, window, cx))
                })
            })
            .map_err(|e| e.to_string())?;
        // Two frames: the first lays out lazily created page views, the second paints them settled.
        for _ in 0..2 {
            cx.update_window(handle, |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .map_err(|e| e.to_string())?;
            cx.run_until_parked();
        }
        let img = cx.capture_screenshot(handle).map_err(|e| e.to_string())?;
        let path = Path::new(out).join(format!("{}.png", page.slug()));
        img.save(&path).map_err(|e| e.to_string())?;
        println!("{}", path.display());
        cx.update_window(handle, |_, window, _| window.remove_window())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn facts() -> Facts {
    Facts {
        volc_key: true,
        mics: vec![
            ("BuiltInMicrophoneDevice".into(), "MacBook Pro 麦克风".into()),
            ("AppleUSBAudioEngine:Shure:MV7".into(), "Shure MV7".into()),
        ],
        default_mic: Some("MacBook Pro 麦克风".into()),
        trusted: true,
        fixture: true,
    }
}

const SENTENCES: [(&str, &str); 12] = [
    ("帮我把这个函数改成异步的", "帮我把这个函数改成异步的。"),
    ("明天下午三点开会讨论一下发布计划", "明天下午三点开会，讨论一下发布计划。"),
    ("这个 bug 是因为缓存没有失效", "这个 bug 是因为缓存没有失效。"),
    ("我觉得可以先上线灰度版本", "我觉得可以先上线灰度版本。"),
    ("把 docker compose 里的端口改成八零八零", "把 Docker Compose 里的端口改成 8080。"),
    ("周五之前把文档发给我", "周五之前把文档发给我。"),
    ("这个接口的延迟大概在两百毫秒左右", "这个接口的延迟大概在 200 毫秒左右。"),
    ("好的没问题", "好的，没问题。"),
    ("麻烦看一下这个 PR", "麻烦看一下这个 PR。"),
    ("用 go 写一个简单的反向代理", "用 Go 写一个简单的反向代理。"),
    ("今晚的会议改到明天", "今晚的会议改到明天。"),
    ("把 kubernetes 的配置也一起更新", "把 Kubernetes 的配置也一起更新。"),
];

const APPS: [&str; 5] = ["Slack", "Visual Studio Code", "Safari", "微信", "Terminal"];

/// Deterministic history over the last ~40 days (only when the folder has none yet).
fn fixture(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    if !dir.join("records.jsonl").exists() {
        let mut store = Store::load(dir);
        let now = Utc.with_ymd_and_hms(2026, 11, 18, 9, 0, 0).unwrap();
        let mut seed: u64 = 7;
        let mut next = move |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
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
        for (k, (from, to)) in [("cooper netes", "Kubernetes"), ("杰森", "JSON"), ("瑞迪斯", "Redis")]
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
    write(
        "hotwords.txt",
        "Plaid\nKubernetes\nGPUI\n火山引擎\ncooper netes => Kubernetes\n",
    )?;
    write(
        "actions.json",
        r#"[{"name":"打开终端","phrases":["打开终端"],"run":"open -a Terminal"},{"name":"翻译","phrases":["翻译"],"run":"pbpaste | trans","input":"selection"}]"#,
    )?;
    write("settings.json", r#"{"llmEnabled":"1","llmModel":"deepseek-chat"}"#)?;
    Ok(())
}
