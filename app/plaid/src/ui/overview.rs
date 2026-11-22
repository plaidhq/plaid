//! 概览: usage over a picked range (today, 7/30/90 days, everything) from usage.json: one summary card with the
//! totals and the chart, top apps and the hours of day, then the 性能 diagnostics for the same range, folded to a
//! summary line until opened (Overview.swift + Perf.swift).
use super::Model;
use super::mav::*;
use super::perf::Perf;
use crate::config;
use chrono::{Datelike, Duration, Local, NaiveDate, Timelike};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::store::{Store, Usage};
use std::collections::BTreeMap;

/// Typing speed the "time saved" estimate assumes, characters per minute (pinyin input, mixed text).
const TYPING_CPM: f64 = 50.;
/// Range values stored in `overviewRange`: days back including today, 0 = everything.
const RANGES: [i64; 5] = [1, 7, 30, 90, 0];
const RANGE_NAMES: [&str; 5] = ["今天", "7 天", "30 天", "90 天", "全部"];
/// Main chart plot area.
const PLOT_H: f32 = 150.;

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
    /// (characters, dictations) per local hour.
    hours: [(i64, i64); 24],
    /// First day with per-hour counts, when the range has dictation days without them.
    week_from: Option<NaiveDate>,
}

pub struct Overview {
    model: Entity<Model>,
    perf: Entity<Perf>,
    data: Data,
    hover: Option<usize>,
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
            data: Data::default(),
            hover: None,
            hover_hour: None,
        };
        o.reload(cx);
        o
    }

    fn reload(&mut self, cx: &App) {
        self.data = build(&self.model.read(cx).store, range());
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

impl Overview {
    /// The range's totals over the chart, in one card. Hovering a column swaps the caption for that column's counts.
    fn summary(&self, cx: &mut Context<Self>) -> Div {
        let d = &self.data;
        let s = d.sum;
        let saved = 0.max((s.chars as f64 / TYPING_CPM * 60000.) as i64 - s.ms);
        let figure = |label: &str, value: String| {
            div()
                .flex()
                .items_baseline()
                .gap(px(6.))
                .whitespace_nowrap()
                .child(hint(label.to_string()).text_size(px(12.)))
                .child(div().text_size(px(14.)).text_color(hex(TEXT)).child(value))
        };
        let hero = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .whitespace_nowrap()
                    .child(
                        div()
                            .font(mono())
                            .text_size(px(36.))
                            .line_height(px(40.))
                            .font_weight(medium())
                            .text_color(hex(TEXT))
                            .child(thousands(s.chars)),
                    )
                    .child(div().text_size(px(14.)).text_color(hex(TEXT2)).child("字"))
                    .children(d.prev.map(|p| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .ml(px(10.))
                            .child(percent(s.chars, p.chars))
                            .child(hint(prev_name(d.range)).text_size(px(12.)))
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x(px(20.))
                    .gap_y(px(4.))
                    .child(figure("时长", duration(s.ms)))
                    .child(figure(
                        "语速",
                        s.speed().map_or("–".into(), |v| format!("{v:.0} 字/分")),
                    ))
                    .child(figure("约省", duration(saved))),
            );

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
            None if d.range == 1 => format!("{} 句", thousands(s.n)),
            None => format!("{} 句 · 活跃 {} 天", thousands(s.n), d.active),
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
                            .max_w(px(40.))
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
        let (_, body) = boxed("", None);
        body.flex_none()
            .gap(px(0.))
            .pt(px(16.))
            .pb(px(8.))
            .child(hero)
            .child(
                div()
                    .flex()
                    .items_end()
                    .justify_between()
                    .gap(px(10.))
                    .mt(px(18.))
                    .mb(px(8.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(medium())
                            .text_color(hex(TEXT2))
                            .child(title),
                    )
                    .child(hint(caption).text_size(px(12.)).min_w_0().truncate()),
            )
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
            )
    }

    /// Characters by local hour over the range; not shown for 今天, whose chart is already hourly.
    fn hours(&self, cx: &mut Context<Self>) -> Div {
        let d = &self.data;
        let h = &d.hours;
        let max = h.iter().map(|c| c.0).max().unwrap_or(0);
        let caption = match self.hover_hour {
            Some(i) => match h[i] {
                (0, _) => format!("{i}:00–{}:00 · 没有听写", i + 1),
                (c, n) => format!("{i}:00–{}:00 · {} 字 · {n} 句", i + 1, thousands(c)),
            },
            None if max > 0 => {
                let peak = (0..24).max_by_key(|i| h[*i].0).unwrap_or(0);
                let mut c = format!("说得最多的是 {peak}:00–{}:00", peak + 1);
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
                .justify_center()
                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                    if *on {
                        this.hover_hour = Some(i);
                    } else if this.hover_hour == Some(i) {
                        this.hover_hour = None;
                    }
                    cx.notify();
                }))
                .when(*c > 0, |col| {
                    col.child(
                        bar()
                            .w(relative(0.7))
                            .h(relative(*c as f32 / max.max(1) as f32))
                            .min_h(px(2.))
                            .when(hovering && self.hover_hour != Some(i), |b| b.opacity(0.3)),
                    )
                })
        });
        let axis = (0..24).map(|i| {
            div()
                .flex_1()
                .relative()
                .when(i % 6 == 0, |c| c.child(axis_label(format!("{i}时"), false)))
        });
        let (head, body) = boxed("时段", None);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(head)
            .child(
                body.flex_1()
                    .gap(px(0.))
                    .child(hint(caption).text_size(px(12.)).mb(px(10.)).truncate())
                    .child(
                        div()
                            .flex()
                            .gap(px(2.))
                            .h(px(96.))
                            .border_b_1()
                            .border_color(hexa(LINE3))
                            .children(columns),
                    )
                    .child(div().flex().gap(px(2.)).h(px(16.)).children(axis)),
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
}

impl Render for Overview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let d = &self.data;
        let range = d.range;
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
        let store = &self.model.read(cx).store;
        let ever = !store.usage.is_empty();
        let streak = streak(store);
        let page = super::page_body("overview").child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(12.))
                .child(seg)
                .child(hint(span).text_size(px(12.)).flex_1().truncate())
                .when(streak > 1, |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(5.))
                            .text_size(px(12.))
                            .text_color(hex(TEXT2))
                            .child(icon("plaid/flame.svg", 12., hex(AMBER)))
                            .child(format!("连续 {streak} 天")),
                    )
                }),
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
        page.child(self.summary(cx))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(12.))
                    .child(self.apps())
                    .when(range != 1, |r| r.child(self.hours(cx))),
            )
            .child(hint(format!(
                "约省按打字 {} 字/分钟估算，减去说话时长。字数：汉字每字计 1，英文和数字每词计 1。清空历史不影响用量统计。",
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
    ui.open(super::Page::Overview, fixture_facts())?;
    expect(
        "default range: 7 daily bars, the hours card, 性能 folded",
        ui.has("overview-bar-6")
            && !ui.has("overview-bar-7")
            && ui.has("overview-hour-23")
            && ui.has("perf-toggle")
            && !ui.has("perf-hist-0")
            && !ui.has("perf-row-0"),
        &None,
    )?;
    ui.shot("overview")?;
    ui.scroll("overview-bar-6", 0.)?;
    ui.shot("overview-7-hover")?;
    ui.click("overview-range-0")?;
    expect(
        "today: 24 hourly bars, no hours card",
        config::value("overviewRange").as_deref() == Some("1")
            && ui.has("overview-bar-23")
            && !ui.has("overview-bar-24")
            && !ui.has("overview-hour-0"),
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
    ui.scroll("overview-hour-17", 0.)?;
    ui.shot("overview-hours-hover")?;
    ui.click("overview-range-4")?;
    expect(
        "全部: 7-day bars",
        config::value("overviewRange").as_deref() == Some("0") && ui.has("overview-bar-0"),
        &None,
    )?;
    ui.shot("overview-all")?;
    super::perf::bring(ui, "perf-toggle")?;
    ui.shot("overview-perf-folded")?;
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
