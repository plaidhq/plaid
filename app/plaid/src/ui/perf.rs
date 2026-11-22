//! 性能: latency, levels, LLM outcomes and the raw records, for chasing slow or dropped dictation (Perf.swift).
use super::Model;
use super::mav::*;
use super::overview::{axis_label, boxed, grid, thousands, ticks, tile};
use crate::config;
use chrono::{DateTime, Datelike, Local, NaiveDate, Utc};
use gpui_kit::base::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::{QUIET_RMS, SILENCE_DEFAULT, pct};
use plaid_core::store::Record;
use std::collections::BTreeMap;
use std::ops::Range;

mod report;

const RANGES: [i64; 3] = [50, 200, 0];
const STAGES: usize = 60;
const EVENTS: usize = 500;
const ROW_H: f32 = 20.;
/// Raw table columns: header, width (0 = flexible).
const COLUMNS: [(&str, f32); 10] = [
    ("时间", 108.),
    ("应用", 72.),
    ("时长", 44.),
    ("首字", 44.),
    ("识别", 44.),
    ("LLM", 56.),
    ("粘贴", 44.),
    ("噪声", 40.),
    ("语音", 44.),
    ("文本", 0.),
];
const KINDS: [&str; 5] = ["错误", "按键", "录音", "识别", "其他"];

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Kind {
    Error,
    Key,
    Record,
    Asr,
    Other,
}

/// events.log line category (EventLog.kind in Perf.swift).
pub(super) fn kind(msg: &str) -> Kind {
    if msg.contains("failed") || msg.contains("error") || msg.contains("unavailable") {
        Kind::Error
    } else if msg.starts_with("key ") {
        Kind::Key
    } else if msg.starts_with("start")
        || msg.starts_with("stop")
        || msg.starts_with("end ")
        || msg == "return"
        || msg == "esc"
    {
        Kind::Record
    } else if msg.starts_with("finished") || msg.starts_with("empty result") {
        Kind::Asr
    } else {
        Kind::Other
    }
}

struct Event {
    day: NaiveDate,
    time: String,
    msg: String,
    kind: Kind,
}

/// The newest `EVENTS` lines of events.log, newest first.
fn events() -> Vec<Event> {
    let body = std::fs::read_to_string(config::data_dir().join("events.log")).unwrap_or_default();
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(EVENTS)..]
        .iter()
        .rev()
        .filter_map(|l| {
            let (ts, msg) = l.split_once(' ')?;
            let t = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Local);
            Some(Event {
                day: t.date_naive(),
                time: t.format("%H:%M:%S%.3f").to_string(),
                msg: msg.to_string(),
                kind: kind(msg),
            })
        })
        .collect()
}

fn local(r: &Record) -> bool {
    r.engine.as_deref() == Some("local")
}

fn engine_name(r: &Record) -> &'static str {
    if local(r) { "本地" } else { "火山引擎" }
}

fn ok(r: &Record) -> bool {
    r.llm == "ok"
}

fn sample(r: &Record) -> bool {
    !r.is_failure() && r.outcome.as_deref() != Some("saved")
}

fn range() -> i64 {
    config::value("perfRange")
        .and_then(|s| s.parse::<f64>().ok())
        .map(|v| v as i64)
        .filter(|v| RANGES.contains(v))
        .unwrap_or(200)
}

/// ByteCountFormatter-ish: memory counts in 1024s, files in 1000s.
pub(super) fn bytes(n: u64, base: f64) -> String {
    let v = n as f64;
    if v < base {
        format!("{n} 字节")
    } else if v < base * base {
        format!("{:.0} KB", v / base)
    } else if v < base * base * base {
        format!("{:.1} MB", v / base / base)
    } else {
        format!("{:.2} GB", v / base / base / base)
    }
}

fn dir_size(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Process phys_footprint, what Activity Monitor calls Memory.
#[cfg(target_os = "macos")]
fn footprint() -> Option<u64> {
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let r = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        )
    };
    (r == 0).then_some(info.ri_phys_footprint)
}

#[cfg(not(target_os = "macos"))]
fn footprint() -> Option<u64> {
    None
}

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn opt(v: Option<i64>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

/// 复制为 CSV.
pub(super) fn csv(rows: &[Record]) -> String {
    let mut out = String::from(
        "at,app,engine,audioMs,firstTextMs,asrMs,llmMs,llm,totalMs,noiseRMS,speechRMS,mic,vp,edited,text\n",
    );
    for r in rows {
        let cols = [
            q(&r.at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            q(r.app.as_deref().unwrap_or("")),
            q(r.engine.as_deref().unwrap_or("")),
            r.audio_ms.to_string(),
            opt(r.first_text_ms),
            r.asr_ms.to_string(),
            opt(r.llm_ms),
            q(&r.llm),
            r.total_ms.to_string(),
            opt(r.noise_rms),
            opt(r.speech_rms),
            q(r.mic.as_deref().unwrap_or("")),
            r.vp.map(|v| v.to_string()).unwrap_or_default(),
            q(r.edited.as_deref().unwrap_or("")),
            q(&r.text),
        ];
        out += &cols.join(",");
        out.push('\n');
    }
    out
}

/// 复制 JSON: pretty, keys sorted.
pub(super) fn json(r: &Record) -> String {
    let sorted: BTreeMap<String, serde_json::Value> = match serde_json::to_value(r) {
        Ok(serde_json::Value::Object(m)) => m.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    serde_json::to_string_pretty(&sorted).unwrap_or_default()
}

/// Character diff: byte ranges of `a` missing from `b` (deleted) and of `b` missing from `a` (inserted), by LCS.
pub(super) fn diff(a: &str, b: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let x: Vec<(usize, char)> = a.char_indices().collect();
    let y: Vec<(usize, char)> = b.char_indices().collect();
    let (n, m) = (x.len(), y.len());
    let mut t = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[at(i, j)] = if x[i].1 == y[j].1 {
                t[at(i + 1, j + 1)] + 1
            } else {
                t[at(i + 1, j)].max(t[at(i, j + 1)])
            };
        }
    }
    let (mut keep_x, mut keep_y) = (vec![false; n], vec![false; m]);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if x[i].1 == y[j].1 {
            keep_x[i] = true;
            keep_y[j] = true;
            i += 1;
            j += 1;
        } else if t[at(i + 1, j)] >= t[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let spans = |s: &str, chars: &[(usize, char)], keep: &[bool]| {
        let mut out: Vec<Range<usize>> = vec![];
        for (k, (off, c)) in chars.iter().enumerate() {
            if keep[k] {
                continue;
            }
            let end = off + c.len_utf8();
            match out.last_mut() {
                Some(r) if r.end == *off => r.end = end,
                _ => out.push(*off..end),
            }
        }
        debug_assert!(out.iter().all(|r| r.end <= s.len()));
        out
    };
    (spans(a, &x, &keep_x), spans(b, &y, &keep_y))
}

fn p50_90(v: &[i64]) -> Option<(i64, i64)> {
    Some((pct(v, 0.5)?, pct(v, 0.9)?))
}

/// One group row: count, ASR p50, paste p50/p90, fallbacks/calls.
fn group_cells(rs: &[&Record]) -> [String; 5] {
    let asr: Vec<i64> = rs.iter().map(|r| r.asr_ms).collect();
    let tot: Vec<i64> = rs.iter().map(|r| r.total_ms).collect();
    let calls = rs.iter().filter(|r| r.llm != "off").count();
    let failed = rs.iter().filter(|r| r.llm != "off" && !ok(r)).count();
    let ms = |v: Option<i64>| v.map_or("–".into(), |v| v.to_string());
    [
        rs.len().to_string(),
        ms(pct(&asr, 0.5)),
        ms(pct(&tot, 0.5)),
        ms(pct(&tot, 0.9)),
        if calls == 0 {
            "–".into()
        } else {
            format!("{failed}/{calls}")
        },
    ]
}

struct Day {
    date: NaiveDate,
    n: usize,
    asr: Option<(i64, i64)>,
    tot: Option<(i64, i64)>,
}

/// Every calendar day from the first sample to the last; empty days break the lines.
fn trend(samples: &[&Record]) -> Vec<Day> {
    let mut by: BTreeMap<NaiveDate, Vec<&Record>> = BTreeMap::new();
    for r in samples {
        by.entry(r.at.with_timezone(&Local).date_naive())
            .or_default()
            .push(r);
    }
    let (Some(first), Some(last)) = (by.keys().next().copied(), by.keys().last().copied()) else {
        return vec![];
    };
    first
        .iter_days()
        .take_while(|d| *d <= last)
        .map(|date| {
            let rs = by.get(&date).cloned().unwrap_or_default();
            let asr: Vec<i64> = rs.iter().map(|r| r.asr_ms).collect();
            let tot: Vec<i64> = rs.iter().map(|r| r.total_ms).collect();
            Day {
                date,
                n: rs.len(),
                asr: p50_90(&asr),
                tot: p50_90(&tot),
            }
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Menu {
    Raw,
    Kinds,
}

pub struct Perf {
    model: Entity<Model>,
    /// The last `range` records (time order), engine-filtered.
    rows: Vec<Record>,
    /// Indexes into `rows` in table order.
    order: Vec<usize>,
    sort: (usize, bool),
    has_both: bool,
    engine: usize,
    total: usize,
    records_size: u64,
    audio_size: u64,
    memory: Option<u64>,
    events: Vec<Event>,
    filter: Entity<InputState>,
    kinds: [bool; 5],
    menu: Option<Menu>,
    selected: Option<DateTime<Utc>>,
    json_open: bool,
    hover_trend: Option<usize>,
    hover_stage: Option<usize>,
    report: report::Report,
    hover_heat: Option<usize>,
    hover_speed: Option<usize>,
    hover_hist: Option<usize>,
}

impl Perf {
    pub fn new(model: Entity<Model>, window: &mut Window, cx: &mut Context<Self>) -> Perf {
        cx.observe(&model, |this, _, cx| {
            this.reload(cx);
            cx.notify();
        })
        .detach();
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("筛选"));
        cx.subscribe_in(&filter, window, |_, _, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        if !model.read(cx).facts.fixture {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(2))
                        .await;
                    let alive = this.update(cx, |p, cx| {
                        p.memory = footprint();
                        cx.notify();
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }
        let mut p = Perf {
            model,
            rows: vec![],
            order: vec![],
            sort: (0, true),
            has_both: false,
            engine: 0,
            total: 0,
            records_size: 0,
            audio_size: 0,
            memory: footprint(),
            events: vec![],
            filter,
            kinds: [true; 5],
            menu: None,
            selected: None,
            json_open: false,
            hover_trend: None,
            hover_stage: None,
            report: Default::default(),
            hover_heat: None,
            hover_speed: None,
            hover_hist: None,
        };
        p.reload(cx);
        p
    }

    fn reload(&mut self, cx: &App) {
        let store = &self.model.read(cx).store;
        let all = &store.all;
        let n = range();
        let cut = if n == 0 {
            0
        } else {
            all.len().saturating_sub(n as usize)
        };
        self.has_both = all.iter().any(local) && all.iter().any(|r| !local(r));
        let engine = if self.has_both { self.engine } else { 0 };
        self.rows = all[cut..]
            .iter()
            .filter(|r| match engine {
                1 => !local(r),
                2 => local(r),
                _ => true,
            })
            .cloned()
            .collect();
        self.total = all.len();
        self.records_size = std::fs::metadata(store.dir().join("records.jsonl"))
            .map(|m| m.len())
            .unwrap_or(0);
        self.audio_size = dir_size(&store.audio_dir());
        self.events = events();
        let samples: Vec<&Record> = self.rows.iter().filter(|r| sample(r)).collect();
        self.report = report::Report::new(store, &self.rows, &samples);
        self.resort();
    }

    fn resort(&mut self) {
        let rows = &self.rows;
        let mut order: Vec<usize> = (0..rows.len()).collect();
        let (col, desc) = self.sort;
        order.sort_by(|a, b| {
            let (x, y) = (&rows[*a], &rows[*b]);
            let o = match col {
                1 => x.app.cmp(&y.app),
                2 => x.audio_ms.cmp(&y.audio_ms),
                3 => x.first_text_ms.cmp(&y.first_text_ms),
                4 => x.asr_ms.cmp(&y.asr_ms),
                5 => x.llm_ms.cmp(&y.llm_ms),
                6 => x.total_ms.cmp(&y.total_ms),
                7 => x.noise_rms.cmp(&y.noise_rms),
                8 => x.speech_rms.cmp(&y.speech_rms),
                9 => x.text.cmp(&y.text),
                _ => x.at.cmp(&y.at),
            }
            .then(x.at.cmp(&y.at));
            if desc { o.reverse() } else { o }
        });
        self.order = order;
    }

    fn samples(&self) -> Vec<&Record> {
        self.rows.iter().filter(|r| sample(r)).collect()
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let m = self.model.clone();
        let range = range();
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(px(10.))
            .child(div().text_color(hex(TEXT)).min_w_0().child(format!(
                "以下是最近 {} 句的粘贴延迟、改动、电平和原始数据，用来排查慢和漏字",
                self.rows.len()
            )))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(8.))
                    .when(self.has_both, |d| {
                        d.child(segmented(
                            "perf-engine",
                            &["全部引擎", "火山引擎", "本地"],
                            self.engine,
                            pick(cx, |this, i: &usize, cx| {
                                this.engine = *i;
                                this.reload(cx);
                                cx.notify();
                            }),
                        ))
                    })
                    .child(segmented(
                        "perf-range",
                        &["最近 50 句", "最近 200 句", "全部"],
                        RANGES.iter().position(|r| *r == range).unwrap_or(1),
                        move |i, _, cx| m.update(cx, |m, cx| m.set("perfRange", RANGES[i], cx)),
                    )),
            )
    }

    fn status(&self) -> Div {
        let line = |name: &str, sub: String, value: String| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(name.to_string())
                        .child(hint(sub)),
                )
                .child(div().flex_none().text_color(hex(TEXT)).child(value))
        };
        let audio = if self.audio_size > 0 {
            bytes(self.audio_size, 1000.)
        } else {
            "无".into()
        };
        card(
            "运行状态",
            None,
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(line(
                    "内存占用",
                    "进程 phys_footprint".into(),
                    self.memory.map_or("–".into(), |b| bytes(b, 1024.)),
                ))
                .child(line(
                    "数据",
                    super::data_dir_label(),
                    format!(
                        "{} 句 · records.jsonl {} · 录音 {audio}",
                        thousands(self.total as i64),
                        bytes(self.records_size, 1000.)
                    ),
                )),
            None,
        )
    }

    fn latency(&self, samples: &[&Record]) -> Div {
        // Five tiles share the row, so they get less side padding than the Overview's four.
        let tile = |title: &str, value: &str, unit: Option<&str>, sub: String| {
            tile(title, value, unit, sub).px(px(10.))
        };
        let lat = |title: &str, v: Vec<i64>| match p50_90(&v) {
            Some((a, b)) => tile(
                title,
                &a.to_string(),
                Some("ms"),
                format!("p90 {b} · {} 句", v.len()),
            ),
            None => tile(title, "–", None, "无数据".into()),
        };
        let calls = samples.iter().filter(|r| r.llm != "off").count();
        let oks = samples.iter().filter(|r| ok(r)).count();
        let failed = calls - oks.min(calls);
        let t = samples.iter().filter(|r| r.llm == "timeout").count();
        let s = samples.iter().filter(|r| r.llm == "skipped").count();
        let fallback = if calls == 0 {
            tile("LLM 回退", "–", None, "润色未启用".into())
        } else {
            tile(
                "LLM 回退",
                &format!("{failed}/{calls}"),
                None,
                format!("超时 {t}·跳过 {s}·出错 {}", failed.saturating_sub(t + s)),
            )
        };
        card(
            "延迟",
            None,
            div()
                .flex()
                .gap(px(8.))
                .child(lat(
                    "首字",
                    samples.iter().filter_map(|r| r.first_text_ms).collect(),
                ))
                .child(lat(
                    "松键 → 识别",
                    samples.iter().map(|r| r.asr_ms).collect(),
                ))
                .child(lat(
                    "LLM",
                    samples
                        .iter()
                        .filter(|r| ok(r))
                        .filter_map(|r| r.llm_ms)
                        .collect(),
                ))
                .child(lat(
                    "松键 → 粘贴",
                    samples.iter().map(|r| r.total_ms).collect(),
                ))
                .child(fallback),
            Some(
                "中位数；小字为 p90 和句数。首字：按下到第一个字。识别：松键到最终结果（按 esc 跳过润色时含等待）。LLM：润色耗时，只算成功的。粘贴：松键到粘贴完成。",
            ),
        )
    }

    fn trend(&self, days: &[Day], cx: &mut Context<Self>) -> Div {
        let n = days.len();
        let caption = match self.hover_trend.filter(|i| *i < n).map(|i| &days[i]) {
            Some(d) => {
                let pair =
                    |v: Option<(i64, i64)>| v.map_or("–".into(), |(a, b)| format!("{a}/{b} ms"));
                format!(
                    "{}/{} · {} 句 · 识别 {} · 粘贴 {}",
                    d.date.month(),
                    d.date.day(),
                    d.n,
                    pair(d.asr),
                    pair(d.tot)
                )
            }
            None => format!(
                "{} 天 · {} 句",
                days.iter().filter(|d| d.n > 0).count(),
                days.iter().map(|d| d.n).sum::<usize>()
            ),
        };
        let max = days
            .iter()
            .flat_map(|d| [d.asr.map(|v| v.0), d.tot.map(|v| v.0)])
            .flatten()
            .max()
            .unwrap_or(0) as f64;
        let (step, top) = ticks(max);
        let h = 164.;
        let (lines, labels) = grid(step, top, h);
        let series: Vec<(Vec<Option<i64>>, u32)> = vec![
            (days.iter().map(|d| d.asr.map(|v| v.0)).collect(), CYAN),
            (days.iter().map(|d| d.tot.map(|v| v.0)).collect(), TEXT2),
        ];
        let plot = canvas(
            |_, _, _| {},
            move |b, _, window, _| {
                let at = |i: usize, v: i64| {
                    point(
                        b.origin.x + b.size.width * ((i as f32 + 0.5) / n as f32),
                        b.origin.y + b.size.height * (1. - (v as f64 / top) as f32),
                    )
                };
                for (vals, color) in &series {
                    let mut path: Option<PathBuilder> = None;
                    let flush = |p: Option<PathBuilder>, window: &mut Window| {
                        if let Some(Ok(p)) = p.map(|p| p.build()) {
                            window.paint_path(p, hex(*color));
                        }
                    };
                    for (i, v) in vals.iter().enumerate() {
                        match v {
                            Some(v) => {
                                let pt = at(i, *v);
                                window.paint_quad(fill(
                                    Bounds::new(pt - point(px(2.), px(2.)), size(px(4.), px(4.))),
                                    hex(*color),
                                ));
                                match path.as_mut() {
                                    Some(p) => p.line_to(pt),
                                    None => {
                                        let mut p = PathBuilder::stroke(px(1.5));
                                        p.move_to(pt);
                                        path = Some(p);
                                    }
                                }
                            }
                            None => flush(path.take(), window),
                        }
                    }
                    flush(path.take(), window);
                }
            },
        )
        .absolute()
        .inset_0();
        // Weekly ticks past 30 days, as Swift does; every other day from 17 days so the M/d labels don't collide.
        let label_every = if n > 30 {
            7
        } else if n > 16 {
            2
        } else {
            1
        };
        let columns = (0..n).map(|i| {
            hit(SharedString::from(format!("perf-trend-{i}")))
                .flex_1()
                .h_full()
                .when(self.hover_trend == Some(i), |c| c.bg(hexa(0xffffff08)))
                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                    if *on {
                        this.hover_trend = Some(i);
                    } else if this.hover_trend == Some(i) {
                        this.hover_trend = None;
                    }
                    cx.notify();
                }))
        });
        let axis = days.iter().enumerate().map(|(i, d)| {
            div()
                .flex_1()
                .relative()
                .when((n - 1 - i).is_multiple_of(label_every), |c| {
                    c.child(axis_label(
                        format!("{}/{}", d.date.month(), d.date.day()),
                        i == n - 1,
                    ))
                })
        });
        card(
            "趋势",
            None,
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(caption_line(
                    caption,
                    legend(&[("识别", CYAN), ("粘贴", TEXT2)]),
                ))
                .child(plot_frame(labels, lines, h, |d| {
                    d.child(div().absolute().inset_0().flex().children(columns))
                        .child(plot)
                }))
                .child(axis_row(axis)),
            Some("每天的中位数；悬停看 p90 和句数。"),
        )
    }

    fn stages(&self, samples: &[&Record], cx: &mut Context<Self>) -> Div {
        let last: Vec<&Record> = samples[samples.len().saturating_sub(STAGES)..].to_vec();
        let n = last.len();
        let parts = |r: &Record| {
            let llm = r.llm_ms.unwrap_or(0);
            let good = if ok(r) { llm } else { 0 };
            let bad = if !matches!(r.llm.as_str(), "ok" | "off" | "skipped") {
                llm
            } else {
                0
            };
            let rest = 0.max(r.total_ms - r.asr_ms - r.llm_ms.unwrap_or(0));
            [r.asr_ms, good, bad, rest]
        };
        let caption = match self.hover_stage.filter(|i| *i < n).map(|i| last[i]) {
            Some(r) => format!(
                "{} · {} · 识别 {} ms · LLM {} · 粘贴 {} ms\n{}",
                r.at.with_timezone(&Local).format("%H:%M:%S"),
                r.app.as_deref().unwrap_or("未知应用"),
                r.asr_ms,
                if ok(r) {
                    r.llm_ms.map_or("–".into(), |v| format!("{v} ms"))
                } else {
                    r.llm.clone()
                },
                r.total_ms,
                r.text
            ),
            None => {
                let asr: Vec<i64> = last.iter().map(|r| r.asr_ms).collect();
                let tot: Vec<i64> = last.iter().map(|r| r.total_ms).collect();
                let ms = |v: Option<i64>| v.map_or("–".into(), |v| format!("{v} ms"));
                format!(
                    "{n} 句 · 识别中位 {} · 粘贴中位 {}",
                    ms(pct(&asr, 0.5)),
                    ms(pct(&tot, 0.5))
                )
            }
        };
        let max = last
            .iter()
            .map(|r| parts(r).iter().sum::<i64>())
            .max()
            .unwrap_or(0) as f64;
        let (step, top) = ticks(max);
        let h = 184.;
        let (lines, labels) = grid(step, top, h);
        let hovering = self.hover_stage.is_some();
        let bars = last.iter().enumerate().map(|(i, r)| {
            let [asr, good, bad, rest] = parts(r);
            let seg = |v: i64, color: u32| {
                div()
                    .w_full()
                    .flex_none()
                    .h(relative((v as f64 / top) as f32))
                    .bg(hex(color))
            };
            hit(SharedString::from(format!("perf-stage-{i}")))
                .flex_1()
                .h_full()
                .px(px(1.))
                .flex()
                .flex_col()
                .justify_end()
                .when(hovering && self.hover_stage != Some(i), |c| c.opacity(0.35))
                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                    if *on {
                        this.hover_stage = Some(i);
                    } else if this.hover_stage == Some(i) {
                        this.hover_stage = None;
                    }
                    cx.notify();
                }))
                .child(seg(rest, TEXT3))
                .child(seg(bad, AMBER))
                .child(seg(good, VIOLET))
                .child(seg(asr, CYAN))
        });
        card(
            "阶段拆分",
            None,
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(caption_line(
                    caption,
                    legend(&[
                        ("识别", CYAN),
                        ("LLM", VIOLET),
                        ("LLM 失败", AMBER),
                        ("其余", TEXT3),
                    ]),
                ))
                .child(plot_frame(labels, lines, h, |d| {
                    d.child(div().absolute().inset_0().flex().items_end().children(bars))
                })),
            Some(
                "每根柱是一句，从左到右按时间，最多 60 句。灰色是识别和 LLM 之外的时间（热词替换、粘贴）。悬停看具体值。",
            ),
        )
    }

    fn groups(&self, samples: &[&Record]) -> Div {
        let mut engines: BTreeMap<&str, Vec<&Record>> = BTreeMap::new();
        let mut apps: BTreeMap<String, Vec<&Record>> = BTreeMap::new();
        for r in samples {
            engines.entry(engine_name(r)).or_default().push(r);
            apps.entry(r.app.clone().unwrap_or_else(|| "未知应用".into()))
                .or_default()
                .push(r);
        }
        let mut by_count: Vec<(String, Vec<&Record>)> = apps.into_iter().collect();
        by_count.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
        let mut app_rows: Vec<(String, Vec<&Record>)> = by_count.iter().take(6).cloned().collect();
        let rest: Vec<&Record> = by_count
            .iter()
            .skip(6)
            .flat_map(|(_, v)| v.clone())
            .collect();
        if !rest.is_empty() {
            app_rows.push(("其他".into(), rest));
        }
        let mut engine_rows: Vec<(String, Vec<&Record>)> = vec![];
        for name in ["火山引擎", "本地"] {
            if let Some(v) = engines.remove(name) {
                engine_rows.push((name.into(), v));
            }
        }
        let cols = ["句数", "识别 p50", "粘贴 p50", "粘贴 p90", "回退"];
        let head = div()
            .flex()
            .h(px(18.))
            .items_center()
            .border_b_1()
            .border_color(hexa(LINE))
            .child(div().flex_1())
            .children(cols.iter().map(|c| {
                hint(c.to_string())
                    .w(px(72.))
                    .flex_none()
                    .flex()
                    .justify_end()
            }));
        let section =
            |title: &str, rows: Vec<(String, Vec<&Record>)>| {
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .pt(px(6.))
                            .pb(px(2.))
                            .text_size(px(11.))
                            .font_weight(bold())
                            .text_color(hex(TEXT2))
                            .child(title.to_string()),
                    )
                    .children(rows.into_iter().map(|(name, rs)| {
                        div()
                            .flex()
                            .h(px(20.))
                            .items_center()
                            .child(div().flex_1().min_w_0().truncate().child(name))
                            .children(group_cells(&rs).into_iter().map(|v| {
                                div().w(px(72.)).flex_none().flex().justify_end().child(v)
                            }))
                    }))
            };
        card(
            "分组",
            None,
            div()
                .flex()
                .flex_col()
                .child(head)
                .child(section("引擎", engine_rows))
                .child(section("应用", app_rows)),
            Some("回退 = 超时、跳过或出错 / 调用次数。"),
        )
    }

    fn levels(&self, samples: &[&Record]) -> Div {
        let silence = config::value("silenceRMS")
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(SILENCE_DEFAULT);
        let lv: Vec<(&Record, i64, i64)> = samples
            .iter()
            .filter_map(|r| Some((*r, r.noise_rms?, r.speech_rms?)))
            .collect();
        let bin = |v: i64| (v.max(0) / 50).min(29) as usize;
        let mut noise = [0i64; 30];
        let mut speech = [0i64; 30];
        for (_, nz, sp) in &lv {
            noise[bin(*nz)] += 1;
            speech[bin(*sp)] += 1;
        }
        let max = noise
            .iter()
            .chain(speech.iter())
            .copied()
            .max()
            .unwrap_or(0) as f64;
        let (step, top) = ticks(max);
        let h = 140.;
        let (lines, labels) = grid(step, top, h);
        let bins = (0..30).map(|i| {
            let bar = |v: i64, color: u32| {
                div()
                    .flex_1()
                    .h(relative((v as f64 / top) as f32))
                    .bg(hex(color))
            };
            div()
                .flex_1()
                .h_full()
                .px(px(1.))
                .flex()
                .items_end()
                .child(bar(noise[i], TEXT3))
                .child(bar(speech[i], CYAN))
        });
        let mark = |v: f64, color: u32| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative((v / 1500.).clamp(0., 1.) as f32))
                .w(px(1.5))
                .bg(hex(color))
        };
        let x_labels = (0..=6).map(|k| {
            let v = k * 250;
            let d = div()
                .absolute()
                .top(px(3.))
                .text_size(px(10.))
                .text_color(hex(HINT))
                .child(v.to_string());
            if k == 6 {
                d.right_0()
            } else {
                d.left(relative(v as f32 / 1500.))
                    .ml(px(if k == 0 { 0. } else { -10. }))
            }
        });
        let mut mics: BTreeMap<String, Vec<(i64, i64)>> = BTreeMap::new();
        for (r, nz, sp) in &lv {
            mics.entry(r.mic.clone().unwrap_or_else(|| "未记录设备".into()))
                .or_default()
                .push((*nz, *sp));
        }
        let mic_rows = mics.into_iter().map(|(name, v)| {
            let nz: Vec<i64> = v.iter().map(|x| x.0).collect();
            let sp: Vec<i64> = v.iter().map(|x| x.1).collect();
            let sp50 = pct(&sp, 0.5).unwrap_or(0);
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().w(px(150.)).flex_none().truncate().child(name))
                .child(hint(format!(
                    "{} 句 · 噪声中位 {} · 语音中位 {sp50}",
                    v.len(),
                    pct(&nz, 0.5).unwrap_or(0)
                )))
                .when(sp50 < QUIET_RMS, |d| {
                    d.child(hint(" · 语音电平偏低").ml(px(-10.)).text_color(hex(AMBER)))
                })
        });
        card(
            "电平",
            None,
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(caption_line(
                    format!(
                        "静音阈值 {}（橙线）· 语音过低 {QUIET_RMS}（灰线）",
                        silence as i64
                    ),
                    legend(&[("噪声", TEXT3), ("语音", CYAN)]),
                ))
                .child(plot_frame(labels, lines, h, |d| {
                    d.child(div().absolute().inset_0().flex().items_end().children(bins))
                        .child(mark(silence, AMBER))
                        .child(mark(QUIET_RMS as f64, TEXT2))
                }))
                .child(
                    div()
                        .flex()
                        .h(px(16.))
                        .child(div().w(px(44.)).flex_none())
                        .child(div().flex_1().relative().children(x_labels)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .pt(px(4.))
                        .children(mic_rows),
                ),
            Some(
                "噪声 = 每句 100 ms 片段 RMS 的 p10，语音 = p90。静音阈值在「设置 → 松键收尾」；语音低于 150 时识别容易漏字。",
            ),
        )
    }

    fn raw_row(&self, k: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let r = &self.rows[self.order[k]];
        let at = r.at;
        let selected = self.selected == Some(at);
        let num = |v: Option<i64>| v.map_or("".into(), |v| v.to_string());
        let llm: (String, Option<u32>) = match r.llm.as_str() {
            "ok" => (num(r.llm_ms), None),
            "off" => ("–".into(), None),
            other => (other.to_string(), Some(AMBER)),
        };
        let prefix = if r.is_failure() {
            "[失败] "
        } else if r.outcome.as_deref() == Some("saved") {
            "[未粘贴] "
        } else {
            ""
        };
        let body = if r.text.is_empty() {
            r.error.clone().unwrap_or_default()
        } else {
            r.text.clone()
        };
        let cells: [(String, Option<u32>); 10] = [
            (
                r.at.with_timezone(&Local)
                    .format("%m-%d %H:%M:%S")
                    .to_string(),
                None,
            ),
            (r.app.clone().unwrap_or_default(), None),
            (format!("{:.1}", r.audio_ms as f64 / 1000.), None),
            (num(r.first_text_ms), None),
            (r.asr_ms.to_string(), None),
            llm,
            (r.total_ms.to_string(), None),
            (num(r.noise_rms), None),
            (num(r.speech_rms), None),
            (format!("{prefix}{body}"), None),
        ];
        table_row(SharedString::from(format!("perf-row-{k}")), k, selected)
            .w_full()
            .h(px(ROW_H))
            .flex_none()
            .cursor_default()
            .children(
                cells
                    .into_iter()
                    .zip(COLUMNS)
                    .map(|((text, color), (_, w))| {
                        cell(w)
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .when_some(color.filter(|_| !selected), |c, col| c.text_color(hex(col)))
                            .child(text)
                    }),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(at);
                this.json_open = false;
                cx.notify();
            }))
    }

    fn raw(&self, cx: &mut Context<Self>) -> Div {
        let (col, desc) = self.sort;
        let header = div()
            .flex()
            .flex_none()
            .h(px(17.))
            .bg(hex(RAISED))
            .border_b_1()
            .border_color(hexa(LINE))
            .text_size(px(11.))
            .children(COLUMNS.iter().enumerate().map(|(i, (label, w))| {
                let c = hit(SharedString::from(format!("perf-sort-{i}")))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(6.))
                    .when(i > 0, |d| d.border_l_1().border_color(hexa(LINE)))
                    .when(i == col, |d| d.bg(tint(CYAN, 0x1f)))
                    .child(div().whitespace_nowrap().child(label.to_string()))
                    .when(i == col, |d| {
                        d.child(icon(
                            if desc {
                                "plaid/down.svg"
                            } else {
                                "plaid/up.svg"
                            },
                            7.,
                            hex(TEXT2),
                        ))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sort = if this.sort.0 == i {
                            (i, !this.sort.1)
                        } else {
                            (i, i == 0)
                        };
                        this.resort();
                        cx.notify();
                    }));
                if *w > 0. {
                    c.w(px(*w)).flex_none()
                } else {
                    c.flex_1().min_w_0()
                }
            }));
        let list = uniform_list(
            "perf-raw-list",
            self.order.len(),
            cx.processor(|this, range: Range<usize>, _, cx| {
                range.map(|k| this.raw_row(k, cx)).collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .text_size(px(11.));
        let more = self.raw_menu(cx);
        let mut body = div().flex().flex_col().gap(px(10.)).child(
            table_well()
                .h(px(360.))
                .flex_none()
                .child(header)
                .child(list),
        );
        if let Some(r) = self
            .selected
            .and_then(|at| self.rows.iter().find(|r| r.at == at))
        {
            body = body.child(self.inspector(r, cx));
        }
        card(
            format!("原始记录（{}）", self.rows.len()),
            Some(more.into_any_element()),
            body,
            Some("点一行看三个文本版本的差异和完整 JSON。列标题可排序；时长单位为秒，其余为 ms。"),
        )
    }

    fn raw_menu(&self, cx: &mut Context<Self>) -> Div {
        let open = self.menu == Some(Menu::Raw);
        div()
            .flex()
            .flex_col()
            .items_end()
            .child(icon_button(
                "perf-raw-more",
                "plaid/action.svg",
                cx.listener(|this, _, _, cx| {
                    this.menu = if this.menu == Some(Menu::Raw) {
                        None
                    } else {
                        Some(Menu::Raw)
                    };
                    cx.notify();
                }),
            ))
            .when(open, |d| {
                d.child(deferred(
                    anchored().snap_to_window_with_margin(px(8.)).child(
                        menu(
                            "perf-raw-menu",
                            200.,
                            vec![
                                MenuEntry {
                                    label: "复制为 CSV".into(),
                                    checked: false,
                                    rule: false,
                                },
                                MenuEntry {
                                    label: "在访达中显示 records.jsonl".into(),
                                    checked: false,
                                    rule: false,
                                },
                            ],
                            pick(cx, |this, i: &usize, cx| {
                                this.menu = None;
                                match i {
                                    0 => cx.write_to_clipboard(ClipboardItem::new_string(csv(
                                        &this.rows,
                                    ))),
                                    _ => super::reveal(&config::data_dir().join("records.jsonl")),
                                }
                                cx.notify();
                            }),
                        )
                        .on_mouse_down_out(cx.listener(
                            |this, _, _, cx| {
                                this.menu = None;
                                cx.notify();
                            },
                        )),
                    ),
                ))
            })
    }

    fn inspector(&self, r: &Record, cx: &mut Context<Self>) -> Div {
        let audio = r
            .audio
            .as_ref()
            .map(|a| self.model.read(cx).store.audio_dir().join(a))
            .filter(|p| p.exists());
        let mut sub = format!(
            "{} · {:.1} 秒 · {}",
            r.at.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S"),
            r.audio_ms as f64 / 1000.,
            engine_name(r)
        );
        if let Some(m) = &r.mic {
            sub += &format!(" · {m}");
        }
        let field = |label: &str, value: AnyElement| {
            div()
                .flex()
                .gap(px(10.))
                .child(
                    hint(label.to_string())
                        .w(px(52.))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .pt(px(1.)),
                )
                .child(div().flex_1().min_w_0().child(value))
        };
        let (deleted, inserted) = diff(&r.raw, &r.text);
        let mut fields: Vec<Div> = vec![];
        if r.is_failure() || r.outcome.as_deref() == Some("saved") {
            let mut s = if r.is_failure() {
                "识别失败".to_string()
            } else {
                "已保存，未自动粘贴".to_string()
            };
            if let Some(e) = &r.error {
                s += &format!("：{e}");
            }
            fields.push(field(
                "状态",
                div().text_color(hex(AMBER)).child(s).into_any_element(),
            ));
        }
        if r.raw != r.text {
            let style = HighlightStyle {
                color: Some(hex(RED)),
                strikethrough: Some(StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(hex(RED)),
                }),
                ..Default::default()
            };
            fields.push(field(
                "识别",
                StyledText::new(r.raw.clone())
                    .with_highlights(deleted.into_iter().map(|g| (g, style)))
                    .into_any_element(),
            ));
        }
        if !r.text.is_empty() {
            let style = HighlightStyle {
                color: Some(hex(GREEN)),
                background_color: Some(tint(GREEN, 0x26)),
                ..Default::default()
            };
            fields.push(field(
                "粘贴",
                StyledText::new(r.text.clone())
                    .with_highlights(inserted.into_iter().map(|g| (g, style)))
                    .into_any_element(),
            ));
        }
        if let Some(e) = &r.edited {
            fields.push(field("修改后", div().child(e.clone()).into_any_element()));
        }
        if !matches!(r.llm.as_str(), "ok" | "off") {
            fields.push(field(
                "LLM",
                div()
                    .text_color(hex(AMBER))
                    .child(r.llm.clone())
                    .into_any_element(),
            ));
        }
        let pretty = json(r);
        let copy = pretty.clone();
        let open = self.json_open;
        group_body()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                hit("perf-inspector")
                                    .font_weight(bold())
                                    .child(r.app.clone().unwrap_or_else(|| "未知应用".into())),
                            )
                            .child(hint(sub)),
                    )
                    .child(button("perf-copy-json", "复制 JSON", move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                    }))
                    .when_some(audio, |d, p| {
                        d.child(icon_button(
                            "perf-audio",
                            "plaid/folder.svg",
                            move |_, _, _| super::reveal(&p),
                        ))
                    }),
            )
            .children(fields)
            .child(disclosure("perf-json", open, "原始 JSON", {
                let toggle = pick(cx, |this, _, cx| {
                    this.json_open = !this.json_open;
                    cx.notify();
                });
                move |window: &mut Window, cx: &mut App| toggle(0, window, cx)
            }))
            .when(open, |d| {
                d.child(
                    div()
                        .id("perf-json-scroll")
                        .max_h(px(260.))
                        .overflow_y_scroll()
                        .p(px(8.))
                        .bg(hex(WELL))
                        .border_1()
                        .border_color(hexa(LINE2))
                        .font(mono())
                        .text_size(px(11.))
                        .child(hit("perf-json-body").child(pretty)),
                )
            })
    }

    fn events_card(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let needle = self.filter.read(cx).value().to_lowercase();
        let idx = |k: Kind| match k {
            Kind::Error => 0,
            Kind::Key => 1,
            Kind::Record => 2,
            Kind::Asr => 3,
            Kind::Other => 4,
        };
        let shown: Vec<&Event> = self
            .events
            .iter()
            .filter(|e| self.kinds[idx(e.kind)])
            .filter(|e| needle.is_empty() || e.msg.to_lowercase().contains(&needle))
            .collect();
        let today = Local::now().date_naive();
        let day_label = |d: NaiveDate| {
            if d == today {
                "今天".to_string()
            } else if Some(d) == today.pred_opt() {
                "昨天".to_string()
            } else {
                format!("{}月{}日", d.month(), d.day())
            }
        };
        let mut rows: Vec<AnyElement> = vec![];
        let mut last_day = None;
        for (i, e) in shown.iter().enumerate() {
            if last_day != Some(e.day) {
                last_day = Some(e.day);
                rows.push(
                    div()
                        .px(px(8.))
                        .py(px(2.))
                        .bg(hex(RAISED))
                        .border_b_1()
                        .border_color(hexa(LINE))
                        .text_size(px(11.))
                        .font_weight(bold())
                        .text_color(hex(TEXT2))
                        .child(day_label(e.day))
                        .into_any_element(),
                );
            }
            let id = if e.kind == Kind::Error {
                format!("perf-event-err-{i}")
            } else {
                format!("perf-event-{i}")
            };
            rows.push(
                hit(SharedString::from(id))
                    .flex()
                    .gap(px(10.))
                    .px(px(8.))
                    .py(px(1.))
                    .text_size(px(11.))
                    .child(
                        div()
                            .flex_none()
                            .font(mono())
                            .text_color(hex(HINT))
                            .child(e.time.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(e.kind == Kind::Error, |d| d.text_color(hex(AMBER)))
                            .child(e.msg.clone()),
                    )
                    .into_any_element(),
            );
        }
        let empty = self.events.is_empty();
        let well = div()
            .id("perf-events")
            .flex_none()
            .h(px(if empty { 140. } else { 300. }))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .bg(hex(WELL))
            .border_1()
            .border_color(hexa(LINE2));
        let well = if empty {
            well.items_center()
                .justify_center()
                .gap(px(4.))
                .child(div().font_weight(bold()).child("还没有事件记录"))
                .child(hint("按键、录音和识别会记在 events.log。"))
        } else if shown.is_empty() {
            well.items_center()
                .justify_center()
                .child(hint("没有匹配的事件"))
        } else {
            well.children(rows)
        };
        let kinds_open = self.menu == Some(Menu::Kinds);
        let controls = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(super::field(&self.filter, 160., window, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(small_button(
                        "perf-kinds",
                        "类型",
                        cx.listener(|this, _, _, cx| {
                            this.menu = if this.menu == Some(Menu::Kinds) {
                                None
                            } else {
                                Some(Menu::Kinds)
                            };
                            cx.notify();
                        }),
                    ))
                    .when(kinds_open, |d| {
                        d.child(deferred(
                            anchored().snap_to_window_with_margin(px(8.)).child(
                                menu(
                                    "perf-kinds-menu",
                                    110.,
                                    KINDS
                                        .iter()
                                        .zip(self.kinds)
                                        .map(|(k, on)| MenuEntry {
                                            label: (*k).into(),
                                            checked: on,
                                            rule: false,
                                        })
                                        .collect(),
                                    pick(cx, |this, i: &usize, cx| {
                                        this.kinds[*i] = !this.kinds[*i];
                                        this.menu = None;
                                        cx.notify();
                                    }),
                                )
                                .on_mouse_down_out(cx.listener(
                                    |this, _, _, cx| {
                                        this.menu = None;
                                        cx.notify();
                                    },
                                )),
                            ),
                        ))
                    }),
            )
            .child(small_button(
                "perf-events-refresh",
                "刷新",
                cx.listener(|this, _, _, cx| {
                    this.events = events();
                    cx.notify();
                }),
            ))
            .child(icon_button(
                "perf-events-reveal",
                "plaid/folder.svg",
                |_, _, _| super::reveal(&config::data_dir().join("events.log")),
            ));
        card(
            format!("事件日志（{}）", shown.len()),
            Some(controls.into_any_element()),
            div().flex().flex_col().child(well),
            Some("只显示最近 500 行；events.log 超过 256 KB 时自动截掉前一半。"),
        )
    }
}

/// A titled box with an optional control on the title line and a footnote under it.
fn card(
    title: impl Into<SharedString>,
    trailing: Option<AnyElement>,
    content: Div,
    note: Option<&str>,
) -> Div {
    let (head, body) = boxed(title, trailing);
    div()
        .flex()
        .flex_col()
        .flex_none()
        .child(head)
        .child(body.child(content))
        .when_some(note, |d, n| {
            d.child(hint(n.to_string()).mx(px(6.)).mt(px(5.)))
        })
}

fn caption_line(caption: String, right: Div) -> Div {
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap(px(10.))
        .child(
            hint(caption)
                .text_size(px(12.))
                .line_height(px(16.))
                .flex_1()
                .min_w_0()
                .h(px(32.))
                .overflow_hidden(),
        )
        .child(right)
}

fn legend(items: &[(&str, u32)]) -> Div {
    div()
        .flex()
        .flex_none()
        .gap(px(10.))
        .children(items.iter().map(|(name, color)| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(div().size(px(8.)).rounded(px(2.)).bg(hex(*color)))
                .child(hint(name.to_string()))
        }))
}

/// Y labels on the left, gridlines and a baseline; `fill` adds the marks.
fn plot_frame(labels: Vec<Div>, lines: Vec<Div>, h: f32, fill: impl FnOnce(Div) -> Div) -> Div {
    div()
        .flex()
        .child(div().w(px(44.)).flex_none().relative().children(labels))
        .child(fill(
            div()
                .flex_1()
                .min_w_0()
                .relative()
                .h(px(h))
                .border_b_1()
                .border_color(hexa(LINE2))
                .children(lines),
        ))
}

fn axis_row(cells: impl Iterator<Item = Div>) -> Div {
    div()
        .flex()
        .h(px(16.))
        .child(div().w(px(44.)).flex_none())
        .child(div().flex_1().flex().children(cells))
}

/// Small square bezel button holding a glyph.
fn icon_button(
    id: &'static str,
    path: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    hit(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(px(24.))
        .h(px(20.))
        .rounded(px(4.))
        .bg(hex(RAISED2))
        .border_1()
        .border_color(hexa(LINE2))
        .hover(|s| s.bg(hex(HOVER)))
        .child(icon(path, 12., hex(TEXT)))
        .on_click(on_click)
}

/// Adapts a Perf update to the `Fn(usize, ..)` callbacks the Mavericks controls take.
fn pick(
    cx: &Context<Perf>,
    f: impl Fn(&mut Perf, &usize, &mut Context<Perf>) + 'static,
) -> impl Fn(usize, &mut Window, &mut App) + 'static {
    let e = cx.entity().downgrade();
    move |i, _, cx| {
        e.update(cx, |this, cx| f(this, &i, cx)).ok();
    }
}

impl Render for Perf {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = super::page_body("perf")
            .child(self.report_usage(cx))
            .child(self.header(cx));
        if self.total == 0 {
            let (_, body) = boxed("", None);
            return page
                .child(
                    body.flex_none()
                        .h(px(180.))
                        .items_center()
                        .justify_center()
                        .child(div().font_weight(bold()).child("还没有听写记录"))
                        .child(
                            hint("说几句之后，这里会显示每句的延迟、电平和原始记录。")
                                .text_size(px(12.)),
                        ),
                )
                .child(self.status())
                .child(self.events_card(window, cx));
        }
        let samples = self.samples();
        let days = trend(&samples);
        let partial = samples.len() != self.rows.len();
        let latency = self.latency(&samples);
        let trend = (days.iter().filter(|d| d.n > 0).count() >= 2).then(|| self.trend(&days, cx));
        let stages = self.stages(&samples, cx);
        let groups = self.groups(&samples);
        let levels = self.levels(&samples);
        let raw = self.raw(cx);
        page.when(!samples.is_empty(), |d| d.child(self.report_records(cx)))
            .child(latency)
            .children(trend)
            .child(stages)
            .child(groups)
            .child(levels)
            .when(partial, |d| {
                d.child(hint(
                    "延迟和电平统计只包含已粘贴的成功听写；失败及仅保存的结果仍在原始记录中。",
                ))
            })
            .child(raw)
            .child(self.status())
            .child(self.events_card(window, cx))
    }
}

#[cfg(feature = "snapshot")]
pub(super) fn check(ui: &mut super::snapshot::Ui) -> Result<(), String> {
    use super::Page;
    use super::snapshot::expect;
    let before = config::value("perfRange");
    expect(
        "perfRange unset in a fresh folder",
        before.is_none(),
        &before,
    )?;
    ui.open(Page::Perf, super::overview::fixture_facts())?;
    expect(
        "default range: last 200 records in the table, inspector closed",
        ui.has("perf-range-1") && !ui.has("perf-inspector"),
        &before,
    )?;
    ui.shot("perf")?;
    // Report cards: a heatmap day (the fixture's newest usage), the speed line and a histogram bin.
    let day = (0..182)
        .rev()
        .map(|i| format!("perf-heat-{i}"))
        .find(|id| ui.has(id))
        .ok_or("FAIL no heatmap cells")?;
    ui.scroll(&day, 0.)?;
    ui.shot("perf-heat-hover")?;
    ui.scroll("perf-speed-29", 0.)?;
    ui.shot("perf-speed-hover")?;
    bring(ui, "perf-hist-4")?;
    ui.scroll("perf-hist-4", 0.)?;
    ui.shot("perf-report")?;
    for (id, want) in [
        ("perf-range-0", "50"),
        ("perf-range-2", "0"),
        ("perf-range-1", "200"),
    ] {
        ui.click(id)?;
        let v = config::value("perfRange");
        expect(&format!("perfRange {want}"), v.as_deref() == Some(want), &v)?;
    }
    if ui.has("perf-engine-2") {
        ui.click("perf-engine-2")?;
        expect(
            "engine filter: local only",
            ui.has("perf-row-0") && !ui.has("perf-row-3"),
            &None,
        )?;
        ui.click("perf-engine-0")?;
    }
    // Hover a trend day, then a stage bar: each caption switches to the hovered item.
    bring(ui, "perf-trend-20")?;
    ui.scroll("perf-trend-20", 0.)?;
    ui.shot("perf-trend-hover")?;
    bring(ui, "perf-stage-59")?;
    ui.scroll("perf-stage-59", 0.)?;
    ui.shot("perf-stages-hover")?;
    // Newest first, the fifth row is a hand-corrected fixture record: raw vs pasted diff plus 修改后.
    bring(ui, "perf-raw-more")?;
    ui.shot("perf-raw")?;
    bring(ui, "perf-row-4")?;
    ui.click("perf-row-4")?;
    expect("row opens the inspector", ui.has("perf-inspector"), &None)?;
    bring(ui, "perf-inspector")?;
    ui.shot("perf-inspector-diff")?;
    // LLM column descending puts a timed-out record first: the LLM field and the JSON disclosure.
    bring(ui, "perf-sort-5")?;
    ui.click("perf-sort-5")?;
    ui.click("perf-sort-5")?;
    ui.click("perf-row-0")?;
    bring(ui, "perf-json")?;
    ui.click("perf-json")?;
    expect("raw JSON disclosed", ui.has("perf-json-body"), &None)?;
    bring(ui, "perf-inspector")?;
    ui.shot("perf-inspector")?;
    bring(ui, "perf-raw-more")?;
    ui.click("perf-raw-more")?;
    expect("raw menu open", ui.has("perf-raw-menu-0"), &None)?;
    ui.shot("perf-raw-menu")?;
    ui.click("perf-row-0")?; // a click outside dismisses the menu; the row is already selected
    expect("raw menu closed", !ui.has("perf-raw-menu-0"), &None)?;
    let event = |ui: &super::snapshot::Ui, i: usize| {
        [format!("perf-event-{i}"), format!("perf-event-err-{i}")]
            .into_iter()
            .find(|id| ui.has(id))
    };
    let deep = event(ui, 8).ok_or("FAIL fewer than 9 events")?;
    bring(ui, &deep)?;
    let errors =
        |ui: &super::snapshot::Ui| (0..EVENTS).any(|i| ui.has(&format!("perf-event-err-{i}")));
    expect("events list shows an error line", errors(ui), &None)?;
    ui.shot("perf-events")?;
    ui.click("perf-kinds")?;
    expect("kinds menu open", ui.has("perf-kinds-menu-0"), &None)?;
    ui.shot("perf-kinds-menu")?;
    ui.click("perf-kinds-menu-0")?;
    expect("errors hidden after toggling 错误 off", !errors(ui), &None)?;
    ui.shot("perf-events-filtered")?;
    expect(
        "event kinds",
        kind("asr failed: timeout") == Kind::Error
            && kind("key down 63") == Kind::Key
            && kind("start") == Kind::Record
            && kind("esc") == Kind::Record
            && kind("finished 312 ms") == Kind::Asr
            && kind("launch 0.11.0") == Kind::Other,
        &None,
    )?;
    let r = Record {
        app: Some("Slack".into()),
        llm: "off".into(),
        text: "say \"hi\", ok".into(),
        ..Default::default()
    };
    let c = csv(std::slice::from_ref(&r));
    expect(
        "csv quoting",
        c.starts_with("at,app,engine,")
            && c.contains(",\"Slack\",")
            && c.ends_with(",\"say \"\"hi\"\", ok\"\n"),
        &Some(c.clone()),
    )?;
    let j = json(&r);
    let (a, l) = (j.find("\"asrMs\""), j.find("\"llm\""));
    expect(
        "json keys sorted",
        matches!((a, l), (Some(a), Some(l)) if a < l),
        &Some(j.clone()),
    )?;
    let (del, ins) = diff("帮我改成异步的", "帮我把它改成异步的。");
    expect(
        "char diff",
        del.is_empty() && ins.len() == 2 && &"帮我把它改成异步的。"[ins[0].clone()] == "把它",
        &Some(format!("{del:?} {ins:?}")),
    )?;
    ui.close()
}

/// Scrolls the page until the control is inside the window, using whichever known control is on screen as the
/// place to scroll over.
#[cfg(feature = "snapshot")]
fn bring(ui: &mut super::snapshot::Ui, id: &str) -> Result<(), String> {
    let visible = |b: Bounds<Pixels>| b.center().y > px(70.) && b.center().y < px(660.);
    for _ in 0..12 {
        let b = ui.bounds(id).ok_or(format!("FAIL no control {id:?}"))?;
        if visible(b) {
            return Ok(());
        }
        let anchor = [
            "perf-heat-0",
            "perf-range-0",
            "perf-hist-0",
            "perf-hist-23",
            "perf-trend-0",
            "perf-trend-20",
            "perf-stage-0",
            "perf-stage-30",
            "perf-sort-0",
            "perf-inspector",
            "perf-json",
            "perf-copy-json",
            "perf-raw-more",
            "perf-kinds",
            "perf-event-0",
            id,
        ]
        .into_iter()
        .find(|a| ui.bounds(a).is_some_and(visible))
        .ok_or(format!("FAIL nothing on screen to scroll over for {id:?}"))?;
        let dy = ((px(360.) - b.center().y) / px(1.)).clamp(-600., 600.);
        ui.scroll(anchor, dy)?;
    }
    Err(format!("FAIL could not scroll {id:?} into view"))
}
