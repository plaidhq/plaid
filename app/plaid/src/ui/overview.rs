//! 概览: usage over a picked range (today, 7/30/90 days, everything) from usage.json, what the LLM and the rules
//! changed, what Plaid has learned, then the 性能 diagnostics for the same range (Overview.swift + Perf.swift).
use super::mav::*;
use super::perf::Perf;
use super::{Model, Page};
use crate::config;
use chrono::{Datelike, Duration, Local, NaiveDate, Timelike};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::store::{Store, Usage};
use std::collections::{BTreeMap, HashSet};

mod charts;

/// Typing speed the "time saved" estimate assumes, characters per minute (pinyin input, mixed text).
const TYPING_CPM: f64 = 50.;
/// Range values stored in `overviewRange`: days back including today, 0 = everything.
const RANGES: [i64; 5] = [1, 7, 30, 90, 0];
const RANGE_NAMES: [&str; 5] = ["今天", "7 天", "30 天", "90 天", "全部"];
/// Plot area; with the 16 px date axis this is Swift's 190 pt chart frame.
const PLOT_H: f32 = 174.;

/// Counts for the 个性化 card; read from disk, so computed when the model changes, not per frame.
#[derive(Default)]
struct Learned {
    hotwords: usize,
    auto: usize,
    edits: usize,
    pending: usize,
}

/// Dictations, audio and characters of a stretch of time.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Sum {
    n: i64,
    ms: i64,
    chars: i64,
}

impl Sum {
    fn add(&mut self, n: i64, ms: i64, chars: i64) {
        self.n += n;
        self.ms += ms;
        self.chars += chars;
    }

    /// Characters per minute of audio.
    fn speed(&self) -> Option<f64> {
        (self.ms > 0 && self.chars > 0).then(|| self.chars as f64 * 60_000. / self.ms as f64)
    }
}

/// One chart column: an hour of today, a day, or up to seven days ending on `last`.
#[derive(Clone, Copy, Debug)]
struct Bucket {
    first: NaiveDate,
    last: NaiveDate,
    hour: Option<u32>,
    sum: Sum,
}

impl Bucket {
    /// Hover caption head.
    fn name(&self) -> String {
        match self.hour {
            Some(h) => format!("{h}:00–{}:00", h + 1),
            None if self.first == self.last => {
                format!(
                    "{}月{}日 {}",
                    self.last.month(),
                    self.last.day(),
                    weekday(self.last)
                )
            }
            None => format!(
                "{}月{}日 – {}月{}日",
                self.first.month(),
                self.first.day(),
                self.last.month(),
                self.last.day()
            ),
        }
    }

    /// Axis label.
    fn tick(&self) -> String {
        match self.hour {
            Some(h) => format!("{h}时"),
            None => format!("{}月{}日", self.last.month(), self.last.day()),
        }
    }
}

/// What the page shows for the picked range; rebuilt when the model or the range changes.
#[derive(Default)]
struct Data {
    range: i64,
    sum: Sum,
    /// Days in the range with any dictation.
    active: usize,
    apps: BTreeMap<String, i64>,
    /// The equal stretch before the range (today: yesterday up to this hour); None when it had no dictation.
    prev: Option<Sum>,
    buckets: Vec<Bucket>,
    /// (characters, dictations) per weekday (Monday first) and local hour.
    week: [[(i64, i64); 24]; 7],
    /// First day with per-hour counts, when the range has dictation days without them.
    week_from: Option<NaiveDate>,
    /// Speed line: the chart's buckets, or the last 7 days when the range is today.
    speed: Vec<Bucket>,
    calendar: Option<charts::Calendar>,
    edits: charts::Edits,
}

pub struct Overview {
    model: Entity<Model>,
    perf: Entity<Perf>,
    learned: Learned,
    data: Data,
    hover: Option<usize>,
    hover_week: Option<usize>,
    hover_speed: Option<usize>,
    hover_heat: Option<usize>,
}

impl Overview {
    pub fn new(model: Entity<Model>, window: &mut Window, cx: &mut Context<Self>) -> Overview {
        cx.observe(&model, |this, _, cx| {
            this.reload(cx);
            cx.notify();
        })
        .detach();
        let perf = cx.new(|cx| Perf::new(model.clone(), window, cx));
        let mut o = Overview {
            model,
            perf,
            learned: Learned::default(),
            data: Data::default(),
            hover: None,
            hover_week: None,
            hover_speed: None,
            hover_heat: None,
        };
        o.reload(cx);
        o
    }

    fn reload(&mut self, cx: &App) {
        let store = &self.model.read(cx).store;
        self.learned = learned(store);
        self.data = build(store, range());
    }
}

pub(super) fn range() -> i64 {
    config::value("overviewRange")
        .and_then(|s| s.parse::<f64>().ok())
        .map(|v| v as i64)
        .filter(|v| RANGES.contains(v))
        .unwrap_or(7)
}

/// First local day of the range; None for everything.
pub(super) fn first_day(range: i64) -> Option<NaiveDate> {
    (range > 0).then(|| Local::now().date_naive() - Duration::days(range - 1))
}

/// "今天", "近 7 天", …, "全部".
pub(super) fn range_label(range: i64) -> String {
    match range {
        0 => "全部".into(),
        1 => "今天".into(),
        n => format!("近 {n} 天"),
    }
}

fn usage_on(store: &Store, d: NaiveDate) -> Option<&Usage> {
    store.usage.get(&d.format("%Y-%m-%d").to_string())
}

fn sum_days(store: &Store, from: NaiveDate, to: NaiveDate) -> Sum {
    let mut s = Sum::default();
    for d in from.iter_days().take_while(|d| *d <= to) {
        if let Some(u) = usage_on(store, d) {
            s.add(u.n, u.ms, u.chars);
        }
    }
    s
}

/// Daily buckets up to a month, else rolling 7-day buckets ending today (the oldest one may be shorter).
fn day_buckets(store: &Store, first: NaiveDate, today: NaiveDate) -> Vec<Bucket> {
    let span = (today - first).num_days() + 1;
    let width = if span <= 31 { 1 } else { 7 };
    let mut out = vec![];
    let mut last = today;
    while last >= first {
        let from = (last - Duration::days(width - 1)).max(first);
        out.push(Bucket {
            first: from,
            last,
            hour: None,
            sum: sum_days(store, from, last),
        });
        last = from - Duration::days(1);
    }
    out.reverse();
    out
}

fn build(store: &Store, range: i64) -> Data {
    let now = Local::now();
    let today = now.date_naive();
    let earliest = store
        .usage
        .keys()
        .next()
        .and_then(|k| NaiveDate::parse_from_str(k, "%Y-%m-%d").ok())
        .unwrap_or(today)
        .min(today);
    let first = first_day(range).unwrap_or(earliest);
    let mut d = Data {
        range,
        ..Default::default()
    };
    for day in first.iter_days().take_while(|d| *d <= today) {
        let Some(u) = usage_on(store, day) else {
            continue;
        };
        d.sum.add(u.n, u.ms, u.chars);
        d.active += (u.n > 0) as usize;
        for (a, c) in &u.apps {
            *d.apps.entry(a.clone()).or_default() += c;
        }
        if u.hours.is_empty() && u.n > 0 {
            d.week_from = Some(today);
        } else if d.week_from.is_some_and(|f| day < f) {
            d.week_from = Some(day);
        }
        let wd = day.weekday().num_days_from_monday() as usize;
        for (h, [n, _, c]) in u.hours.iter().enumerate().take(24) {
            d.week[wd][h].0 += c;
            d.week[wd][h].1 += n;
        }
    }
    d.prev = match range {
        0 => None,
        1 => {
            let y = today - Duration::days(1);
            usage_on(store, y).map(|u| {
                if u.hours.is_empty() {
                    Sum {
                        n: u.n,
                        ms: u.ms,
                        chars: u.chars,
                    }
                } else {
                    let mut s = Sum::default();
                    for [n, ms, c] in u.hours.iter().take(now.hour() as usize + 1) {
                        s.add(*n, *ms, *c);
                    }
                    s
                }
            })
        }
        n => Some(sum_days(
            store,
            first - Duration::days(n),
            first - Duration::days(1),
        )),
    }
    .filter(|s| s.chars > 0);
    d.buckets = if range == 1 {
        let hours = usage_on(store, today)
            .map(|u| u.hours.clone())
            .unwrap_or_default();
        (0..24)
            .map(|h| {
                let [n, ms, chars] = hours.get(h as usize).copied().unwrap_or_default();
                Bucket {
                    first: today,
                    last: today,
                    hour: Some(h),
                    sum: Sum { n, ms, chars },
                }
            })
            .collect()
    } else {
        day_buckets(store, first, today)
    };
    d.speed = if range == 1 {
        day_buckets(store, today - Duration::days(6), today)
    } else {
        d.buckets.clone()
    };
    if range == 0 {
        d.calendar = Some(charts::Calendar::new(store, today));
    }
    d.edits = charts::Edits::new(
        store
            .all
            .iter()
            .filter(|r| r.at.with_timezone(&Local).date_naive() >= first),
    );
    d
}

/// Consecutive days with dictation, ending today (or yesterday while today is still empty).
fn streak(store: &Store) -> usize {
    let today = Local::now().date_naive();
    let n = |i: i64| usage_on(store, today - Duration::days(i)).map_or(0, |u| u.n);
    let skip = (n(0) == 0) as i64;
    (skip..400).take_while(|i| n(*i) > 0).count()
}

/// Hotword lines including learned rules (Hotwords.lines in Swift), for "already a rule" checks.
fn hotword_lines() -> HashSet<String> {
    let dir = config::data_dir();
    let read = |p: std::path::PathBuf| std::fs::read_to_string(p).unwrap_or_default();
    let extra = config::value("hotwordsExtra")
        .map(|p| read(p.into()))
        .unwrap_or_default();
    let body = [
        read(dir.join("hotwords.txt")),
        read(dir.join(plaid_core::store::AUTO_RULES)),
        extra,
    ]
    .join("\n");
    plaid_core::hotwords::lines(&body).into_iter().collect()
}

/// pendingFixes (Dictionary.swift): manual-edit substitutions not yet dismissed, written, learned or aliased.
fn learned(store: &Store) -> Learned {
    let terms: Vec<String> = config::terms(false)
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect();
    let dismissed: HashSet<String> = config::raw("dismissedFixes")
        .unwrap_or_default()
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    let auto = store.auto_rules(&terms, &dismissed);
    let lines = hotword_lines();
    let entries = config::hotwords().entries;
    let pending = store
        .fixes()
        .into_iter()
        .filter(|f| {
            let k = format!("{} -> {}", f.from, f.to);
            !dismissed.contains(&k)
                && !lines.contains(&k)
                && !auto.iter().any(|(a, b, _)| *a == f.from && *b == f.to)
                && !entries
                    .iter()
                    .any(|e| e.target == f.to && e.aliases.contains(&f.from))
        })
        .count();
    Learned {
        hotwords: terms.len(),
        auto: auto.len(),
        edits: store.all.iter().filter(|r| r.edited.is_some()).count(),
        pending,
    }
}

/// Swift `duration(_:)`.
pub(super) fn duration(ms: i64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{} 小时 {} 分", s / 3600, s % 3600 / 60)
    } else if s >= 60 {
        format!("{} 分钟", s / 60)
    } else {
        format!("{s} 秒")
    }
}

/// `Int.formatted()`: grouped thousands.
pub(super) fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn weekday(d: NaiveDate) -> &'static str {
    ["周一", "周二", "周三", "周四", "周五", "周六", "周日"]
        [d.weekday().num_days_from_monday() as usize]
}

/// Round axis steps (1, 2, 5 × 10^k) giving at most four gridlines above zero; returns (step, top).
pub(super) fn ticks(max: f64) -> (f64, f64) {
    if max <= 0. {
        return (1., 1.);
    }
    let raw = max / 4.;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1., 2., 5., 10.]
        .into_iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10. * mag);
    (step, (max / step).ceil() * step)
}

/// Canvas `.stat`: title, big number with small units, caption.
pub(super) fn tile(
    title: &str,
    value: &str,
    unit: Option<&str>,
    sub: impl Into<SharedString>,
) -> Div {
    stat(
        title,
        value,
        unit,
        hint(sub)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis(),
    )
}

/// `tile` with any caption element.
fn stat(title: &str, value: &str, unit: Option<&str>, sub: impl IntoElement) -> Div {
    let numeric = |t: &str| {
        t.chars()
            .all(|c| c.is_ascii_digit() || c == ',' || c == '.' || c == '-' || c == '–' || c == '/')
    };
    let mut parts: Vec<(String, bool)> = value
        .split(' ')
        .map(|t| (t.to_string(), numeric(t)))
        .collect();
    if let Some(u) = unit {
        parts.push((u.to_string(), false));
    }
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .px(px(14.))
        .py(px(12.))
        .rounded(px(10.))
        .bg(hex(SURFACE))
        .border_1()
        .border_color(hexa(LINE))
        .child(hint(title.to_string()).text_size(px(11.)))
        .child(
            div()
                .flex()
                .items_baseline()
                .whitespace_nowrap()
                .overflow_hidden()
                .children(parts.into_iter().map(|(t, num)| {
                    if num {
                        div()
                            .font(mono())
                            .text_size(px(24.))
                            .line_height(px(32.))
                            .font_weight(medium())
                            .text_color(hex(TEXT))
                            .child(t)
                    } else {
                        div()
                            .text_size(px(12.))
                            .text_color(hex(TEXT2))
                            .ml(px(3.))
                            .mr(px(3.))
                            .child(t)
                    }
                })),
        )
        .child(sub)
}

/// Canvas `.boxt` + `.box`, with an optional control at the right of the title line.
pub(super) fn boxed(title: impl Into<SharedString>, trailing: Option<AnyElement>) -> (Div, Div) {
    let head = div()
        .flex()
        .items_end()
        .justify_between()
        .mx(px(6.))
        .mb(px(4.))
        .child(
            div()
                .text_size(px(11.))
                .font_weight(medium())
                .text_color(hex(TEXT2))
                .child(title.into()),
        )
        .children(trailing);
    let body = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(16.))
        .py(px(12.))
        .rounded(px(10.))
        .bg(hex(SURFACE))
        .border_1()
        .border_color(hexa(LINE));
    (head, body)
}

/// A plot column: cyan, brightest at the top.
pub(super) fn bar() -> Div {
    div().rounded_t(px(3.)).bg(linear_gradient(
        180.,
        linear_color_stop(hex(CYAN), 0.),
        linear_color_stop(tint(CYAN, 0x59), 1.),
    ))
}

/// Gridlines and their labels for a plot `h` high with values up to `top`.
pub(super) fn grid(step: f64, top: f64, h: f32) -> (Vec<Div>, Vec<Div>) {
    let n = (top / step).round() as usize;
    let lines = (1..=n)
        .map(|k| {
            let y = h - (k as f64 * step / top) as f32 * h;
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(y))
                .h(px(1.))
                .bg(hexa(LINE))
        })
        .collect();
    let labels = (0..=n)
        .map(|k| {
            let y = h - (k as f64 * step / top) as f32 * h;
            div()
                .absolute()
                .right(px(6.))
                .top(px(y - 7.))
                .font(mono())
                .text_size(px(10.))
                .line_height(px(13.))
                .text_color(hex(TEXT3))
                .child(thousands((k as f64 * step) as i64))
        })
        .collect();
    (lines, labels)
}

/// A label centered under a column, free to overflow it; the last one is pinned to the plot's right edge.
pub(super) fn axis_label(text: impl Into<SharedString>, last: bool) -> Div {
    div()
        .absolute()
        .top(px(3.))
        .left(px(-40.))
        .right(px(if last { 0. } else { -40. }))
        .flex()
        .when(last, |d| d.justify_end())
        .when(!last, |d| d.justify_center())
        .font(mono())
        .text_size(px(10.))
        .text_color(hex(TEXT3))
        .whitespace_nowrap()
        .child(text.into())
}

fn pill(path: &'static str, text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .h(px(22.))
        .px(px(9.))
        .rounded_full()
        .bg(tint(color, 0x14))
        .border_1()
        .border_color(tint(color, 0x40))
        .text_size(px(11.))
        .text_color(hex(color))
        .whitespace_nowrap()
        .child(icon(path, 11., hex(color)))
        .child(text.into())
}

/// What the ↑↓ compares against.
fn prev_name(range: i64) -> String {
    match range {
        1 => "比昨天同一时段".into(),
        n => format!("比前 {n} 天"),
    }
}

/// Up/down against the previous stretch: "↑ 12%" in green, "↓ 8%" in grey.
fn change(text: String, up: bool) -> Div {
    div()
        .flex_none()
        .font(mono())
        .text_size(px(11.))
        .text_color(hex(if up { GREEN } else { TEXT2 }))
        .child(text)
}

fn percent(now: i64, before: i64) -> Div {
    let p = ((now - before) as f64 / before.max(1) as f64 * 100.).round() as i64;
    change(
        if p >= 0 {
            format!("↑ {p}%")
        } else {
            format!("↓ {}%", -p)
        },
        p >= 0,
    )
}

impl Overview {
    fn status(&self, cx: &App) -> Div {
        let m = self.model.read(cx);
        let streak = streak(&m.store);
        let chosen = config::value("micDevice");
        let mic = chosen
            .and_then(|c| m.facts.mics.iter().find(|(id, _)| *id == c))
            .map(|(_, n)| n.clone())
            .or_else(|| m.facts.default_mic.clone())
            .unwrap_or_else(|| "无输入设备".into());
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .when(streak > 1, |d| {
                d.child(pill("plaid/flame.svg", format!("连续 {streak} 天"), AMBER))
            })
            .child(pill("plaid/cloud.svg", "火山引擎", CYAN))
            .child(pill("plaid/mic.svg", mic, TEXT2))
            .when(super::on_if_one("llmEnabled"), |d| {
                d.child(pill("plaid/refine.svg", "LLM 润色", VIOLET))
            })
    }

    fn tiles(&self) -> Div {
        let d = &self.data;
        let s = d.sum;
        let speed = s.speed().unwrap_or(0.);
        let saved = 0.max((s.chars as f64 / TYPING_CPM * 60000.) as i64 - s.ms);
        // The tile's caption: the comparison when there is one, else `plain`.
        let sub = |delta: Option<Div>, plain: String| {
            let row = div()
                .flex()
                .items_center()
                .gap(px(6.))
                .whitespace_nowrap()
                .overflow_hidden();
            match delta {
                Some(c) => row.child(c).child(hint(prev_name(d.range))),
                None => row.child(hint(plain)),
            }
        };
        let prev = d.prev;
        let speed_delta = prev
            .and_then(|p| p.speed())
            .filter(|_| speed > 0.)
            .map(|p| {
                let v = (speed - p).round() as i64;
                change(
                    if v >= 0 {
                        format!("↑ {v} 字/分")
                    } else {
                        format!("↓ {} 字/分", -v)
                    },
                    v >= 0,
                )
            });
        let active = d.active;
        div()
            .flex()
            .flex_none()
            .gap(px(10.))
            .child(stat(
                "听写时长",
                &duration(s.ms),
                None,
                sub(
                    prev.map(|p| percent(s.ms, p.ms)),
                    if d.range == 1 {
                        format!("{} 句", thousands(s.n))
                    } else {
                        format!("活跃 {active} 天")
                    },
                ),
            ))
            .child(stat(
                "听写字数",
                &thousands(s.chars),
                None,
                sub(
                    prev.map(|p| percent(s.chars, p.chars)),
                    format!("{} 句", thousands(s.n)),
                ),
            ))
            .child(stat(
                "平均语速",
                &format!("{speed:.0}"),
                Some("字/分"),
                sub(speed_delta, format!("打字约 {} 字/分", TYPING_CPM as i64)),
            ))
            .child(tile(
                "节省时间",
                &duration(saved),
                None,
                if speed > 0. {
                    format!("比打字快 {:.1} 倍", speed / TYPING_CPM)
                } else {
                    "–".into()
                },
            ))
    }
}

impl Overview {
    fn chart(&self, cx: &mut Context<Self>) -> Div {
        let d = &self.data;
        let b = &d.buckets;
        let n = b.len();
        let picked = self.hover.filter(|i| *i < n).map(|i| &b[i]);
        let caption = match picked {
            Some(k) => format!(
                "{}  {} 字 · {} 句 · {}",
                k.name(),
                thousands(k.sum.chars),
                k.sum.n,
                duration(k.sum.ms)
            ),
            None if d.range == 1 => {
                format!("{} 字 · {} 句", thousands(d.sum.chars), thousands(d.sum.n))
            }
            None => format!("{} 字 · 活跃 {} 天", thousands(d.sum.chars), d.active),
        };
        let hourly = d.range == 1;
        let weekly = b
            .first()
            .is_some_and(|k| k.hour.is_none() && k.first != k.last);
        let title = if hourly {
            "每小时字数"
        } else if weekly {
            "每 7 天字数"
        } else {
            "每日字数"
        };
        let dense = n > 40;
        let ratio = if dense { 0.8 } else { 0.6 };
        let gap = if dense { 0. } else { 2. };
        // Hours every 3; days every day up to a week, else weekly from today back; 7-day buckets about 6 labels.
        let labelled = |i: usize| {
            if hourly {
                i.is_multiple_of(3)
            } else if n <= 7 {
                true
            } else {
                let stride = if weekly { n.div_ceil(6) } else { 7 };
                (n - 1 - i).is_multiple_of(stride)
            }
        };
        let max = b.iter().map(|k| k.sum.chars).max().unwrap_or(0) as f64;
        let (step, top) = ticks(max);
        let (lines, labels) = grid(step, top, PLOT_H);
        let hovering = picked.is_some();
        let columns = b.iter().enumerate().map(|(i, k)| {
            let h = (k.sum.chars as f64 / top) as f32;
            hit(SharedString::from(format!("overview-bar-{i}")))
                .flex_1()
                .h_full()
                .flex()
                .items_end()
                .justify_center()
                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                    if *on {
                        this.hover = Some(i);
                    } else if this.hover == Some(i) {
                        this.hover = None;
                    }
                    cx.notify();
                }))
                .when(k.sum.chars > 0, |c| {
                    c.child(
                        bar()
                            .w(relative(ratio))
                            .h(relative(h))
                            .min_h(px(2.))
                            .when(hovering && self.hover != Some(i), |b| b.opacity(0.3)),
                    )
                })
        });
        let axis = b.iter().enumerate().map(|(i, k)| {
            div().flex_1().relative().when(labelled(i), |c| {
                c.child(axis_label(k.tick(), i == n - 1 && dense))
            })
        });
        let (head, body) = boxed(title, None);
        div().flex().flex_col().flex_none().child(head).child(
            body.pt(px(10.))
                .pb(px(8.))
                .child(hint(caption).text_size(px(12.)))
                .child(
                    div()
                        .flex()
                        .child(div().w(px(44.)).flex_none().relative().children(labels))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .relative()
                                .h(px(PLOT_H))
                                .border_b_1()
                                .border_color(hexa(LINE3))
                                .children(lines)
                                .child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .gap(px(gap))
                                        .children(columns),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .h(px(16.))
                        .child(div().w(px(44.)).flex_none())
                        .child(div().flex_1().flex().gap(px(gap)).children(axis)),
                ),
        )
    }

    fn apps(&self) -> Div {
        let d = &self.data;
        let mut sorted: Vec<(&String, &i64)> = d.apps.iter().filter(|(_, c)| **c > 0).collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let mut rows: Vec<(String, i64)> = sorted
            .iter()
            .take(5)
            .map(|(k, v)| ((*k).clone(), **v))
            .collect();
        let rest: i64 = sorted.iter().skip(5).map(|(_, v)| **v).sum();
        if rest > 0 {
            rows.push(("其他".into(), rest));
        }
        let sum = d.sum.chars.max(1) as f64;
        let (head, body) = boxed("常用应用", None);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(head)
            .child(
                body.flex_1()
                    .gap(px(9.))
                    .when(rows.is_empty(), |b| {
                        b.justify_center()
                            .items_center()
                            .child(hint("这段时间没有听写"))
                    })
                    .children(rows.into_iter().map(|(name, c)| {
                        let share = c as f64 / sum;
                        let mark = app_icon(&name, 16.).map_or_else(
                            || {
                                div()
                                    .size(px(16.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(4.))
                                    .bg(hex(RAISED2))
                                    .text_size(px(9.))
                                    .text_color(hex(TEXT2))
                                    .child(
                                        name.chars()
                                            .next()
                                            .map(|c| c.to_uppercase().collect::<String>())
                                            .unwrap_or_default(),
                                    )
                                    .into_any_element()
                            },
                            IntoElement::into_any_element,
                        );
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .w(px(130.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(7.))
                                    .child(mark)
                                    .child(div().min_w_0().truncate().child(name)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(6.))
                                    .rounded(px(3.))
                                    .bg(hex(RAISED2))
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(share as f32))
                                            .min_w(px(6.))
                                            .rounded(px(3.))
                                            .bg(hgrada((CYAN << 8) | 0x80, (CYAN << 8) | 0xff)),
                                    ),
                            )
                            .child(
                                hint(format!("{}%", (share * 100.).round() as i64))
                                    .font(mono())
                                    .w(px(40.))
                                    .flex_none()
                                    .flex()
                                    .justify_end(),
                            )
                    })),
            )
    }

    fn learned_card(&self) -> Div {
        let l = &self.learned;
        let link = hit("overview-dictionary")
            .text_size(px(11.))
            .text_color(hex(LINK))
            .cursor_pointer()
            .child("打开词典")
            .on_click(|_, _, cx| super::open(Page::Dictionary, cx));
        let (head, body) = boxed("个性化", Some(link.into_any_element()));
        let line = |name: &str, value: String, color: u32| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().w(px(90.)).flex_none().child(name.to_string()))
                .child(
                    div()
                        .flex_1()
                        .font(mono())
                        .text_color(hex(color))
                        .child(value),
                )
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(head)
            .child(
                body.flex_1()
                    .gap(px(10.))
                    .child(line("热词", format!("{} 个", l.hotwords), TEXT))
                    .child(line("自动规则", format!("{} 条", l.auto), TEXT))
                    .child(line("手动修改", format!("{} 句", l.edits), TEXT))
                    .child(line(
                        "待处理建议",
                        format!("{} 条", l.pending),
                        if l.pending > 0 { AMBER } else { TEXT },
                    )),
            )
    }
}

impl Render for Overview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let d = &self.data;
        let range = d.range;
        let headline = match range {
            0 => format!("累计听写 {} 字", thousands(d.sum.chars)),
            1 => format!("今天听写 {} 字", thousands(d.sum.chars)),
            n => format!("近 {n} 天听写 {} 字", thousands(d.sum.chars)),
        };
        let today = Local::now().date_naive();
        let span = match (range, d.buckets.first()) {
            (1, _) => format!("{}月{}日 {}", today.month(), today.day(), weekday(today)),
            (_, Some(b)) => format!("{}月{}日 – 今天", b.first.month(), b.first.day()),
            _ => String::new(),
        };
        let m = self.model.clone();
        let seg = segmented(
            "overview-range",
            &RANGE_NAMES,
            RANGES.iter().position(|r| *r == range).unwrap_or(1),
            move |i, _, cx| m.update(cx, |m, cx| m.set("overviewRange", RANGES[i], cx)),
        );
        let ever = !self.model.read(cx).store.usage.is_empty();
        let page = super::page_body("overview")
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(medium())
                            .text_color(hex(TEXT))
                            .truncate()
                            .child(headline),
                    )
                    .child(self.status(cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(12.))
                    .child(seg)
                    .child(hint(span).text_size(px(12.))),
            );
        if !ever {
            let (_, body) = boxed("", None);
            return page
                .child(
                    body.flex_none()
                        .h(px(220.))
                        .items_center()
                        .justify_center()
                        .child(div().font_weight(bold()).child("还没有听写记录"))
                        .child(
                            hint("按住快捷键说话，松开后文字会粘贴到光标处。").text_size(px(12.)),
                        ),
                )
                .child(self.perf.clone());
        }
        page.child(self.tiles())
            .child(self.chart(cx))
            .when(range != 1, |p| p.child(self.week(cx)))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(12.))
                    .child(self.apps())
                    .child(self.speed(cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(12.))
                    .child(self.edits())
                    .child(self.learned_card()),
            )
            .when(self.data.calendar.is_some(), |p| p.child(self.calendar(cx)))
            .child(hint(format!(
                "节省时间按打字 {} 字/分钟估算，减去说话时长。字数：汉字每字计 1，英文和数字每词计 1。清空历史不影响用量统计。",
                TYPING_CPM as i64
            )))
            .child(self.perf.clone())
    }
}

#[cfg(feature = "snapshot")]
pub(super) fn fixture_facts() -> super::Facts {
    super::Facts {
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

#[cfg(feature = "snapshot")]
pub(super) fn check(ui: &mut super::snapshot::Ui) -> Result<(), String> {
    use super::snapshot::expect;
    let before = config::value("overviewRange");
    expect(
        "overviewRange unset in a fresh folder",
        before.is_none(),
        &before,
    )?;
    ui.open(Page::Overview, fixture_facts())?;
    expect(
        "default range: 7 daily bars, the weekday × hour grid, no calendar",
        ui.has("overview-bar-6")
            && !ui.has("overview-bar-7")
            && ui.has("overview-week-0")
            && !ui.has("overview-heat-0"),
        &None,
    )?;
    ui.shot("overview")?;
    ui.scroll("overview-bar-6", 0.)?;
    ui.shot("overview-7-hover")?;
    ui.click("overview-range-0")?;
    expect(
        "today: 24 hourly bars, no weekday grid",
        config::value("overviewRange").as_deref() == Some("1")
            && ui.has("overview-bar-23")
            && !ui.has("overview-bar-24")
            && !ui.has("overview-week-0"),
        &None,
    )?;
    ui.shot("overview-today")?;
    ui.click("overview-range-2")?;
    expect(
        "range 30: daily bars",
        config::value("overviewRange").as_deref() == Some("30")
            && ui.has("overview-bar-29")
            && !ui.has("overview-bar-30"),
        &None,
    )?;
    ui.shot("overview-30")?;
    ui.click("overview-range-3")?;
    expect(
        "range 90: 13 seven-day bars",
        config::value("overviewRange").as_deref() == Some("90")
            && ui.has("overview-bar-12")
            && !ui.has("overview-bar-13"),
        &None,
    )?;
    ui.scroll("overview-bar-12", 0.)?;
    ui.shot("overview-90-hover")?;
    ui.click("overview-range-4")?;
    expect(
        "全部: the half-year calendar",
        config::value("overviewRange").as_deref() == Some("0") && ui.has("overview-heat-0"),
        &None,
    )?;
    ui.shot("overview-all")?;
    let day = (0..182)
        .rev()
        .map(|i| format!("overview-heat-{i}"))
        .find(|id| ui.has(id))
        .ok_or("FAIL no calendar cells")?;
    super::perf::bring(ui, &day)?;
    ui.scroll(&day, 0.)?;
    ui.shot("overview-calendar-hover")?;
    super::perf::bring(ui, "overview-range-1")?;
    ui.click("overview-range-1")?;
    expect(
        "back to 7",
        config::value("overviewRange").as_deref() == Some("7"),
        &None,
    )?;
    let sum = Sum {
        n: 2,
        ms: 60_000,
        chars: 120,
    };
    expect("speed", sum.speed() == Some(120.), &None)?;
    expect(
        "duration format",
        duration(59_000) == "59 秒"
            && duration(61_000) == "1 分钟"
            && duration(3_725_000) == "1 小时 2 分",
        &None,
    )?;
    expect(
        "thousands",
        thousands(48210) == "48,210"
            && thousands(999) == "999"
            && thousands(1_000_000) == "1,000,000",
        &None,
    )?;
    ui.close()
}
