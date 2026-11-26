//! 概览, from usage.json: 今天 on top as a reading row (characters, speaking time, time saved, polish latency) and a
//! speed scale, no chart; below it the 回顾 cards (7 and 30 days side by side, or one 90-day / everything card) with
//! a column per day, the apps and hours of the picked range, then the 性能 diagnostics for that range, folded to a
//! summary line until opened.
use super::Model;
use super::mav::*;
use super::perf::Perf;
use crate::config;
use chrono::{Datelike, Duration, Local, NaiveDate, Timelike};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::pct;
use plaid_core::store::{Store, Usage};
use std::collections::BTreeMap;

/// Typing speed the "time saved" estimate assumes, characters per minute (pinyin input, mixed text).
const TYPING_CPM: f64 = 50.;
/// Range values stored in `overviewRange`: days back including today, 0 = everything. 30 shows the 7- and 30-day
/// cards; the apps, hours and 性能 below cover the stored range.
const RANGES: [i64; 3] = [30, 90, 0];
const RANGE_NAMES: [&str; 3] = ["7 / 30 天", "90 天", "全部"];

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
}

/// What the page shows for the picked range; rebuilt when the model or the range changes.
#[derive(Default)]
struct Data {
    range: i64,
    sum: Sum,
    /// Days in the range, today included.
    days: i64,
    /// Days in the range with any dictation.
    active: usize,
    apps: BTreeMap<String, i64>,
    /// The equal stretch before the range (today: yesterday up to this hour); None when it had no dictation.
    prev: Option<Sum>,
    buckets: Vec<Bucket>,
    /// (characters, dictations) per local hour.
    hours: [(i64, i64); 24],
    /// First day with per-hour counts, when the range has dictation days without them.
    week_from: Option<NaiveDate>,
}

pub struct Overview {
    model: Entity<Model>,
    perf: Entity<Perf>,
    today: Data,
    /// Last 7 days, for the saved-time caption.
    week: Sum,
    /// Characters per minute over the last 30 days.
    month_speed: Option<f64>,
    /// Today's LLM polish: median ms and calls.
    polish: Option<(i64, usize)>,
    /// 回顾 cards: 7 and 30 days, or the one picked range.
    cards: Vec<Data>,
    /// (card, column) under the pointer.
    hover: Option<(usize, usize)>,
    hover_hour: Option<usize>,
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
            today: Data::default(),
            week: Sum::default(),
            month_speed: None,
            polish: None,
            cards: vec![],
            hover: None,
            hover_hour: None,
        };
        o.reload(cx);
        o
    }

    fn reload(&mut self, cx: &App) {
        let store = &self.model.read(cx).store;
        let range = range();
        self.today = build(store, 1);
        self.cards = match range {
            30 => vec![build(store, 7), build(store, 30)],
            r => vec![build(store, r)],
        };
        let today = Local::now().date_naive();
        self.week = sum_days(store, today - Duration::days(6), today);
        self.month_speed = sum_days(store, today - Duration::days(29), today).speed();
        let polish: Vec<i64> = store
            .all
            .iter()
            .filter(|r| r.llm == "ok" && r.at.with_timezone(&Local).date_naive() == today)
            .filter_map(|r| r.llm_ms)
            .collect();
        self.polish = pct(&polish, 0.5).map(|ms| (ms, polish.len()));
    }
}

/// The stored range; the old 今天 / 7 天 values show the 7 / 30 cards.
pub(super) fn range() -> i64 {
    config::value("overviewRange")
        .and_then(|s| s.parse::<f64>().ok())
        .map(|v| match v as i64 {
            1 | 7 => 30,
            v => v,
        })
        .filter(|v| RANGES.contains(v))
        .unwrap_or(30)
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
        days: (today - first).num_days() + 1,
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
        for (h, [n, _, c]) in u.hours.iter().enumerate().take(24) {
            d.hours[h].0 += c;
            d.hours[h].1 += n;
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
    d
}

/// Consecutive days with dictation, ending today (or yesterday while today is still empty).
fn streak(store: &Store) -> usize {
    let today = Local::now().date_naive();
    let n = |i: i64| usage_on(store, today - Duration::days(i)).map_or(0, |u| u.n);
    let skip = (n(0) == 0) as i64;
    (skip..400).take_while(|i| n(*i) > 0).count()
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
        .bg(hexa(GLASS))
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
        .bg(hexa(GLASS))
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

/// Characters' worth of typing time minus the time spent speaking.
fn saved_ms(s: Sum) -> i64 {
    0.max((s.chars as f64 / TYPING_CPM * 60000.) as i64 - s.ms)
}

/// "9:06", "1:02:05".
fn clock(ms: i64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Saved time as a big number and its unit: seconds under a minute, minutes under an hour, else hours.
fn saved_parts(ms: i64) -> (String, &'static str) {
    let s = ms / 1000;
    if s < 60 {
        (s.to_string(), "秒")
    } else if s < 3600 {
        ((s / 60).to_string(), "分钟")
    } else {
        (format!("{:.1}", s as f64 / 3600.), "小时")
    }
}

/// Short saved time for captions: "45 分钟", "2.6 小时".
fn saved_text(ms: i64) -> String {
    let (v, u) = saved_parts(ms);
    format!("{v} {u}")
}

/// A label over a value in the reading: "字数", "1,284", "+18% 比昨天同一时段".
fn reading(label: &str, value: Div, note: impl IntoElement, first: bool) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(10.))
        .py(px(20.))
        .pr(px(20.))
        .when(!first, |c| {
            c.pl(px(20.)).border_l_1().border_color(hexa(LINE))
        })
        .child(hint(label.to_string()))
        .child(value)
        .child(note)
}

/// Big monospace number with a small sans unit.
fn figure(value: String, unit: Option<&str>, size: f32, color: u32) -> Div {
    div()
        .flex()
        .items_baseline()
        .whitespace_nowrap()
        .overflow_hidden()
        .child(
            div()
                .font(mono())
                .text_size(px(size))
                .line_height(px(size * 1.1))
                .font_weight(medium())
                .text_color(hex(color))
                .child(value),
        )
        .children(unit.map(|u| {
            div()
                .ml(px(6.))
                .text_size(px(13.))
                .text_color(hex(TEXT2))
                .child(u.to_string())
        }))
}

fn note(text: impl Into<SharedString>) -> Div {
    hint(text)
        .text_color(hex(TEXT3))
        .whitespace_nowrap()
        .overflow_hidden()
        .text_ellipsis()
}

/// "日均 1,766" in a card's footer: grey label, white value.
fn fact(label: &str, value: String, color: u32) -> Div {
    div()
        .flex()
        .gap(px(5.))
        .whitespace_nowrap()
        .child(hint(label.to_string()))
        .child(div().text_size(px(12.)).text_color(hex(color)).child(value))
}

/// A column for the stretch that includes today.
fn bar_now() -> Div {
    div().rounded_t(px(3.)).bg(linear_gradient(
        180.,
        linear_color_stop(hex(VIOLET), 0.),
        linear_color_stop(tint(VIOLET, 0x4d), 1.),
    ))
}

/// Segment colors of the app split, most used first; the last is 其他.
const APP_COLORS: [u32; 5] = [CYAN, VIOLET, GREEN, AMBER, TEXT3];

impl Overview {
    /// 今天: the reading row (characters, speaking time, time saved, polish latency) and the speed scale.
    fn today(&self, cx: &App) -> Div {
        let d = &self.today;
        let s = d.sum;
        let words = reading(
            "字数",
            figure(thousands(s.chars), None, 52., CYAN),
            div()
                .flex()
                .gap(px(6.))
                .whitespace_nowrap()
                .overflow_hidden()
                .children(d.prev.map(|p| percent(s.chars, p.chars)))
                .child(note(match d.prev {
                    Some(_) => format!("{} · {} 句", prev_name(1), s.n),
                    None => format!("{} 句", s.n),
                })),
            true,
        )
        .flex_grow(1.3);
        let speaking = reading(
            "说话",
            figure(clock(s.ms), None, 40., TEXT),
            note(if s.n > 0 {
                format!("每句平均 {} 秒", (s.ms / s.n + 500) / 1000)
            } else {
                "还没有听写".into()
            }),
            false,
        );
        let (v, u) = saved_parts(saved_ms(s));
        let saved = reading(
            "约省",
            figure(v, Some(u), 40., GREEN),
            note(format!("近 7 天累计 {}", saved_text(saved_ms(self.week)))),
            false,
        );
        let polish = match self.polish {
            Some((ms, n)) => reading(
                "润色",
                figure(ms.to_string(), Some("ms"), 40., TEXT),
                note(format!("中位 · {n} 句调用 LLM")),
                false,
            ),
            None => reading(
                "润色",
                figure("–".into(), None, 40., TEXT3),
                note("今天没有调用 LLM"),
                false,
            ),
        };
        let today = Local::now().date_naive();
        let streak = streak(&self.model.read(cx).store);
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(16.))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(12.))
                    .mt(px(6.))
                    .child(div().text_size(px(26.)).font_weight(medium()).child("今天"))
                    .child(
                        div()
                            .flex_1()
                            .font(mono())
                            .text_size(px(12.))
                            .text_color(hex(TEXT2))
                            .child(format!(
                                "{}月{}日 {}",
                                today.month(),
                                today.day(),
                                weekday(today)
                            )),
                    )
                    .when(streak > 1, |r| {
                        r.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .text_size(px(12.))
                                .text_color(hex(TEXT2))
                                .child(icon("plaid/flame.svg", 12., hex(AMBER)))
                                .child(format!("连续 {streak} 天")),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .border_t_1()
                    .border_b_1()
                    .border_color(hexa(LINE2))
                    .child(words)
                    .child(speaking)
                    .child(saved)
                    .child(polish),
            )
            .child(self.speed_scale())
    }

    /// Today's speed on a track against typing speed and the 30-day average.
    fn speed_scale(&self) -> Div {
        let now = self.today.sum.speed();
        let avg = self.month_speed;
        let top =
            (now.unwrap_or(0.).max(avg.unwrap_or(0.)).max(TYPING_CPM) * 1.25 / 50.).ceil() * 50.;
        let top = top.max(200.);
        let at = |v: f64| (v / top).min(1.) as f32;
        let mark = |v: f64, color: Hsla, tall: bool| {
            div()
                .absolute()
                .left(relative(at(v)))
                .top(px(if tall { 10. } else { 14. }))
                .w(px(if tall { 3. } else { 2. }))
                .h(px(if tall { 22. } else { 14. }))
                .rounded(px(1.))
                .bg(color)
        };
        let label = |v: f64, text: String, color: u32, above: bool| {
            div()
                .absolute()
                .left(relative(at(v)))
                .ml(px(-50.))
                .w(px(100.))
                .top(px(if above { -8. } else { 32. }))
                .flex()
                .justify_center()
                .font(mono())
                .text_size(px(11.))
                .text_color(hex(color))
                .whitespace_nowrap()
                .child(text)
        };
        let mut right = vec![];
        if let Some(v) = now {
            right.push(format!("比打字快 {:.1}×", v / TYPING_CPM));
        }
        if let Some(a) = avg {
            right.push(format!("30 天平均 {a:.0}"));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(hint("语速 · 字/分"))
                    .child(note(right.join(" · "))),
            )
            .child(
                div()
                    .relative()
                    .h(px(50.))
                    .mx(px(4.))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(px(20.))
                            .h(px(2.))
                            .rounded(px(1.))
                            .bg(hexa(LINE2)),
                    )
                    .children(now.map(|v| {
                        div()
                            .absolute()
                            .left_0()
                            .top(px(20.))
                            .h(px(2.))
                            .w(relative(at(v)))
                            .rounded(px(1.))
                            .bg(hgrada((CYAN << 8) | 0x33, (CYAN << 8) | 0xff))
                    }))
                    .child(mark(TYPING_CPM, hex(TEXT3), false))
                    .child(label(
                        TYPING_CPM,
                        format!("打字 {}", TYPING_CPM as i64),
                        TEXT3,
                        false,
                    ))
                    .children(avg.map(|a| mark(a, tint(TEXT, 0x80), false)))
                    .children(avg.map(|a| label(a, format!("均 {a:.0}"), TEXT3, false)))
                    .children(now.map(|v| mark(v, hex(CYAN), true)))
                    .children(now.map(|v| label(v, format!("{v:.0}"), CYAN, true)))
                    .when(now.is_none(), |t| {
                        t.child(
                            div()
                                .absolute()
                                .left_0()
                                .top(px(-6.))
                                .child(note("今天还没有听写")),
                        )
                    }),
            )
    }

    /// One retro card: total and change, a column per day (or per 7 days), and a footer of averages that turns
    /// into the hovered column's counts.
    fn card(&self, k: usize, cx: &mut Context<Self>) -> Div {
        let d = &self.cards[k];
        let s = d.sum;
        let b = &d.buckets;
        let n = b.len();
        let weekly = b.first().is_some_and(|k| k.first != k.last);
        let hover = self
            .hover
            .filter(|(c, i)| *c == k && *i < n)
            .map(|(_, i)| i);
        let max = b.iter().map(|k| k.sum.chars).max().unwrap_or(0).max(1) as f32;
        let gap = if n <= 7 {
            10.
        } else if n <= 31 {
            3.
        } else {
            2.
        };
        let id = d.range;
        let columns = b.iter().enumerate().map(|(i, bk)| {
            let h = bk.sum.chars as f32 / max;
            let now = i == n - 1;
            hit(SharedString::from(format!("overview-{id}-{i}")))
                .flex_1()
                .h_full()
                .flex()
                .items_end()
                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                    if *on {
                        this.hover = Some((k, i));
                    } else if this.hover == Some((k, i)) {
                        this.hover = None;
                    }
                    cx.notify();
                }))
                .when(bk.sum.chars > 0, |c| {
                    c.child(
                        if now { bar_now() } else { bar() }
                            .w_full()
                            .h(relative(h))
                            .min_h(px(2.))
                            .when(hover.is_some_and(|x| x != i), |b| b.opacity(0.3)),
                    )
                })
        });
        let date = |d: NaiveDate| format!("{}/{}", d.month(), d.day());
        let tick = |i: usize| -> Option<String> {
            let bk = &b[i];
            if i == n - 1 {
                Some("今天".into())
            } else if n <= 7 {
                Some(weekday(bk.last).into())
            } else if !weekly {
                (i == 0 || i == n / 2).then(|| date(bk.last))
            } else {
                (n - 1 - i)
                    .is_multiple_of(n.div_ceil(5))
                    .then(|| date(bk.last))
            }
        };
        let axis = (0..n).map(|i| {
            div()
                .flex_1()
                .relative()
                .children(tick(i).map(|t| axis_label(t, i == n - 1 && n > 7)))
        });
        let footer = match hover.map(|i| &b[i]) {
            Some(bk) => div().child(hint(format!(
                "{}  {} 字 · {} 句 · {}",
                bk.name(),
                thousands(bk.sum.chars),
                bk.sum.n,
                duration(bk.sum.ms)
            ))),
            None => {
                let avg = fact("日均", thousands(s.chars / d.days.max(1)), TEXT);
                let saved = fact("约省", saved_text(saved_ms(s)), GREEN);
                let first = if d.range == 7 {
                    fact(
                        "语速",
                        s.speed().map_or("–".into(), |v| format!("{v:.0}")),
                        TEXT,
                    )
                } else {
                    fact("活跃", format!("{} 天", d.active), TEXT)
                };
                div().child(first).child(avg).child(saved)
            }
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(14.))
            .px(px(22.))
            .py(px(20.))
            .rounded(px(14.))
            .bg(hexa(GLASS))
            .border_1()
            .border_color(hexa(LINE))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(hint(range_label(d.range)))
                    .children(d.prev.map(|p| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(note(prev_name(d.range)).text_size(px(11.)))
                            .child(percent(s.chars, p.chars))
                    })),
            )
            .child(figure(thousands(s.chars), Some("字"), 30., TEXT))
            .child(div().flex().gap(px(gap)).h(px(84.)).children(columns))
            .child(
                div()
                    .flex()
                    .gap(px(gap))
                    .h(px(14.))
                    .mt(px(-10.))
                    .children(axis),
            )
            .child(div().h(px(1.)).bg(hexa(LINE)))
            .child(
                footer
                    .flex()
                    .justify_between()
                    .gap(px(12.))
                    .h(px(17.))
                    .overflow_hidden(),
            )
    }

    /// Apps as one split bar with a legend, and characters by hour of day, for the picked range.
    fn breakdown(&self, cx: &mut Context<Self>) -> Div {
        let Some(d) = self.cards.last() else {
            return div();
        };
        let mut sorted: Vec<(&String, &i64)> = d.apps.iter().filter(|(_, c)| **c > 0).collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let mut rows: Vec<(String, i64)> = sorted
            .iter()
            .take(4)
            .map(|(k, v)| ((*k).clone(), **v))
            .collect();
        let rest: i64 = sorted.iter().skip(4).map(|(_, v)| **v).sum();
        if rest > 0 {
            rows.push(("其他".into(), rest));
        }
        let total = rows.iter().map(|r| r.1).sum::<i64>().max(1) as f32;
        let colors = |i: usize, name: &str| {
            if name == "其他" {
                TEXT3
            } else {
                APP_COLORS[i.min(3)]
            }
        };
        let split = div()
            .flex()
            .gap(px(2.))
            .h(px(8.))
            .rounded_full()
            .overflow_hidden()
            .bg(hex(RAISED2))
            .children(rows.iter().enumerate().map(|(i, (name, c))| {
                div()
                    .h_full()
                    .flex_shrink(1.)
                    .w(relative(*c as f32 / total))
                    .bg(hex(colors(i, name)))
            }));
        let legend = div()
            .flex()
            .flex_wrap()
            .gap_x(px(16.))
            .gap_y(px(6.))
            .children(rows.iter().enumerate().map(|(i, (name, c))| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .text_color(hex(TEXT2))
                    .child(div().size(px(8.)).rounded(px(2.)).bg(hex(colors(i, name))))
                    .children(app_icon(name, 14.))
                    .child(name.clone())
                    .child(
                        div()
                            .font(mono())
                            .text_color(hex(TEXT3))
                            .child(format!("{}%", (*c as f32 / total * 100.).round() as i64)),
                    )
            }));
        let label = range_label(d.range);
        let apps = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(hint(format!("{label}常用应用")))
            .when(rows.is_empty(), |c| c.child(note("这段时间没有听写")))
            .when(!rows.is_empty(), |c| c.child(split).child(legend));

        let h = &d.hours;
        let max = h.iter().map(|c| c.0).max().unwrap_or(0);
        let caption = match self.hover_hour {
            Some(i) => match h[i] {
                (0, _) => format!("{i}:00–{}:00 · 没有听写", i + 1),
                (c, n) => format!("{i}:00–{}:00 · {} 字 · {n} 句", i + 1, thousands(c)),
            },
            None if max > 0 => {
                let peak = (0..24).max_by_key(|i| h[*i].0).unwrap_or(0);
                let mut c = format!("{label}时段 · 最多 {peak}:00–{}:00", peak + 1);
                if let Some(f) = d.week_from {
                    c += &format!("，从 {}月{}日 起按小时统计", f.month(), f.day());
                }
                c
            }
            None => "这段时间没有按小时的记录".into(),
        };
        let hovering = self.hover_hour.is_some();
        let columns = h.iter().enumerate().map(|(i, (c, _))| {
            hit(SharedString::from(format!("overview-hour-{i}")))
                .flex_1()
                .h_full()
                .flex()
                .items_end()
                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                    if *on {
                        this.hover_hour = Some(i);
                    } else if this.hover_hour == Some(i) {
                        this.hover_hour = None;
                    }
                    cx.notify();
                }))
                .child(
                    if *c > 0 { bar() } else { div().bg(hexa(LINE2)) }
                        .w_full()
                        .h(if *c > 0 {
                            relative(*c as f32 / max.max(1) as f32)
                        } else {
                            relative(0.)
                        })
                        .min_h(px(2.))
                        .when(hovering && self.hover_hour != Some(i), |b| b.opacity(0.3)),
                )
        });
        let axis = (0..24).map(|i| {
            div()
                .flex_1()
                .relative()
                .when(i % 6 == 0, |c| c.child(axis_label(format!("{i}时"), false)))
        });
        let hours = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(hint(caption).truncate())
            .child(div().flex().gap(px(3.)).h(px(36.)).children(columns))
            .child(
                div()
                    .flex()
                    .gap(px(3.))
                    .h(px(14.))
                    .mt(px(-8.))
                    .children(axis),
            );
        div()
            .flex()
            .flex_none()
            .gap(px(28.))
            .px(px(22.))
            .py(px(16.))
            .rounded(px(14.))
            .bg(hexa(GLASS))
            .border_1()
            .border_color(hexa(LINE))
            .child(apps)
            .child(hours)
    }
}

impl Render for Overview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = super::page_body("overview");
        let ever = !self.model.read(cx).store.usage.is_empty();
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
                            hint("按住说话键开始听写，松开后文字粘贴到光标处。").text_size(px(12.)),
                        ),
                )
                .child(self.perf.clone());
        }
        let m = self.model.clone();
        let seg = segmented(
            "overview-range",
            &RANGE_NAMES,
            RANGES.iter().position(|r| *r == range()).unwrap_or(0),
            move |i, _, cx| m.update(cx, |m, cx| m.set("overviewRange", RANGES[i], cx)),
        );
        let cards: Vec<Div> = (0..self.cards.len()).map(|k| self.card(k, cx)).collect();
        page.gap(px(18.))
            .child(self.today(cx))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .mt(px(8.))
                    .child(div().text_size(px(15.)).font_weight(medium()).child("回顾"))
                    .child(seg),
            )
            .child(div().flex().flex_none().gap(px(14.)).mt(px(-6.)).children(cards))
            .child(self.breakdown(cx))
            .child(hint(format!(
                "约省按打字 {} 字/分钟估算，减去说话时长。字数：汉字每字计 1，英文和数字每词计 1。润色为今天调用 LLM 的耗时中位数。清空历史不影响用量统计。",
                TYPING_CPM as i64
            )))
            .child(self.perf.clone())
    }
}

#[cfg(feature = "snapshot")]
pub(super) fn fixture_facts() -> super::Facts {
    super::Facts {
        volc_key: true,
        cloud_token: false,
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
    ui.open(super::Page::Overview, fixture_facts())?;
    expect(
        "default: 7- and 30-day cards, the hours strip, 性能 folded",
        ui.has("overview-7-6")
            && !ui.has("overview-7-7")
            && ui.has("overview-30-29")
            && !ui.has("overview-30-30")
            && ui.has("overview-hour-23")
            && ui.has("perf-toggle")
            && !ui.has("perf-hist-0")
            && !ui.has("perf-row-0"),
        &None,
    )?;
    ui.shot("overview")?;
    ui.scroll("overview-7-5", 0.)?;
    ui.shot("overview-7-hover")?;
    ui.scroll("overview-30-20", 0.)?;
    ui.shot("overview-30-hover")?;
    super::perf::bring(ui, "overview-range-1")?;
    ui.click("overview-range-1")?;
    expect(
        "range 90: one card of 13 seven-day bars",
        config::value("overviewRange").as_deref() == Some("90")
            && ui.has("overview-90-12")
            && !ui.has("overview-90-13")
            && !ui.has("overview-7-0"),
        &None,
    )?;
    ui.scroll("overview-90-12", 0.)?;
    ui.shot("overview-90-hover")?;
    super::perf::bring(ui, "overview-hour-17")?;
    ui.scroll("overview-hour-17", 0.)?;
    ui.shot("overview-hours-hover")?;
    ui.click("overview-range-2")?;
    expect(
        "全部: 7-day bars",
        config::value("overviewRange").as_deref() == Some("0") && ui.has("overview-0-0"),
        &None,
    )?;
    ui.shot("overview-all")?;
    super::perf::bring(ui, "perf-toggle")?;
    ui.shot("overview-perf-folded")?;
    super::perf::bring(ui, "overview-range-0")?;
    ui.click("overview-range-0")?;
    expect(
        "back to 7 / 30",
        config::value("overviewRange").as_deref() == Some("30"),
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
        "clock and saved format",
        clock(546_000) == "9:06"
            && clock(3_725_000) == "1:02:05"
            && saved_text(30_000) == "30 秒"
            && saved_text(960_000) == "16 分钟"
            && saved_text(9_360_000) == "2.6 小时",
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
