//! 概览: usage totals from usage.json, the daily chart, per-app share and what Plaid has learned (Overview.swift).
use super::mav::*;
use super::{Model, Page};
use crate::config;
use chrono::{Datelike, Local, NaiveDate};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::store::{Store, Usage};
use std::collections::HashSet;

/// Typing speed the "time saved" estimate assumes, characters per minute (pinyin input, mixed text).
const TYPING_CPM: f64 = 50.;
const RANGES: [i64; 3] = [7, 30, 90];
/// Plot area; with the 16 px date axis this is Swift's 190 pt chart frame.
const PLOT_H: f32 = 174.;
const ORANGE: u32 = 0xb25e00;

/// Counts for the 个性化 card; read from disk, so computed when the model changes, not per frame.
#[derive(Default)]
struct Learned {
    hotwords: usize,
    auto: usize,
    edits: usize,
    pending: usize,
}

pub struct Overview {
    model: Entity<Model>,
    learned: Learned,
    hover: Option<usize>,
}

impl Overview {
    pub fn new(model: Entity<Model>, _: &mut Window, cx: &mut Context<Self>) -> Overview {
        cx.observe(&model, |this, m, cx| {
            this.learned = learned(&m.read(cx).store);
            cx.notify();
        })
        .detach();
        let learned = learned(&model.read(cx).store);
        Overview {
            model,
            learned,
            hover: None,
        }
    }
}

fn range() -> i64 {
    config::value("overviewRange")
        .and_then(|s| s.parse::<f64>().ok())
        .map(|v| v as i64)
        .filter(|v| RANGES.contains(v))
        .unwrap_or(30)
}

/// The last `n` local days, oldest first.
fn days(store: &Store, n: i64) -> Vec<(NaiveDate, Usage)> {
    let today = Local::now().date_naive();
    (0..n)
        .rev()
        .map(|i| {
            let d = today - chrono::Duration::days(i);
            let u = store
                .usage
                .get(&d.format("%Y-%m-%d").to_string())
                .cloned()
                .unwrap_or_default();
            (d, u)
        })
        .collect()
}

fn total(store: &Store) -> Usage {
    let mut t = Usage::default();
    for u in store.usage.values() {
        t.n += u.n;
        t.ms += u.ms;
        t.chars += u.chars;
        for (a, c) in &u.apps {
            *t.apps.entry(a.clone()).or_default() += c;
        }
    }
    t
}

/// Rolling last 7 days vs the 7 before.
fn headline(store: &Store) -> String {
    let d = days(store, 14);
    let now: i64 = d[7..].iter().map(|(_, u)| u.chars).sum();
    let before: i64 = d[..7].iter().map(|(_, u)| u.chars).sum();
    let mut s = format!("近 7 天听写 {} 字", thousands(now));
    if before > 0 {
        let p = ((now - before) as f64 / before as f64 * 100.).round() as i64;
        s += &if p >= 0 {
            format!("，比前 7 天多 {p}%")
        } else {
            format!("，比前 7 天少 {}%", -p)
        };
    }
    s
}

/// Consecutive days with dictation, ending today (or yesterday while today is still empty).
fn streak(store: &Store) -> usize {
    let d: Vec<i64> = days(store, 400).iter().rev().map(|(_, u)| u.n).collect();
    let tail = if d.first() == Some(&0) {
        &d[1..]
    } else {
        &d[..]
    };
    tail.iter().take_while(|n| **n > 0).count()
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

pub(super) fn inset_white() -> BoxShadow {
    shadow(0., 1., 0., 0., 0xffffffff, true)
}

/// Canvas `.stat`: title, big number with small units, caption.
pub(super) fn tile(
    title: &str,
    value: &str,
    unit: Option<&str>,
    sub: impl Into<SharedString>,
) -> Div {
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
        .py(px(10.))
        .rounded(px(5.))
        .bg(vgrad(0xfbfbfb, 0xefefef))
        .border_1()
        .border_color(hexa(0x00000029))
        .shadow(vec![inset_white()])
        .child(hint(title.to_string()))
        .child(
            div()
                .flex()
                .items_baseline()
                .whitespace_nowrap()
                .overflow_hidden()
                .children(parts.into_iter().map(|(t, num)| {
                    if num {
                        div()
                            .text_size(px(24.))
                            .line_height(px(30.))
                            .font_weight(bold())
                            .text_color(hex(0x2b2b2b))
                            .child(t)
                    } else {
                        div()
                            .text_size(px(12.))
                            .text_color(hex(0x555555))
                            .ml(px(3.))
                            .mr(px(3.))
                            .child(t)
                    }
                })),
        )
        .child(
            hint(sub)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis(),
        )
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
                .text_color(hex(0x3a3a3a))
                .child(title.into()),
        )
        .children(trailing);
    let body = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(16.))
        .py(px(12.))
        .rounded(px(5.))
        .bg(hexa(0x0000000a))
        .border_1()
        .border_color(hexa(0x00000024));
    (head, body)
}

/// The canvas `.bar`: a glossy vertical cylinder, lit in the middle.
pub(super) fn bar() -> Div {
    div()
        .flex()
        .rounded_t(px(2.))
        .overflow_hidden()
        .border_1()
        .border_color(hexa(0x1e468c59))
        .child(div().flex_1().h_full().bg(linear_gradient(
            90.,
            linear_color_stop(hex(0x4f8fdd), 0.),
            linear_color_stop(hex(0x9fcaf6), 1.),
        )))
        .child(div().flex_1().h_full().bg(linear_gradient(
            90.,
            linear_color_stop(hex(0x5c9ae3), 0.),
            linear_color_stop(hex(0x3b7ccf), 1.),
        )))
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
                .bg(hexa(0x00000014))
        })
        .collect();
    let labels = (0..=n)
        .map(|k| {
            let y = h - (k as f64 * step / top) as f32 * h;
            div()
                .absolute()
                .right(px(6.))
                .top(px(y - 7.))
                .text_size(px(10.))
                .line_height(px(13.))
                .text_color(hex(HINT))
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
        .text_size(px(10.))
        .text_color(hex(HINT))
        .whitespace_nowrap()
        .child(text.into())
}

fn pill(path: &'static str, text: impl Into<SharedString>, color: u32, fill: u32) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .h(px(19.))
        .px(px(8.))
        .rounded_full()
        .bg(hexa(fill))
        .border_1()
        .border_color(hexa((color << 8) | 0x40))
        .text_size(px(11.))
        .text_color(hex(color))
        .whitespace_nowrap()
        .child(icon(path, 11., hex(color)))
        .child(text.into())
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
                d.child(pill(
                    "plaid/flame.svg",
                    format!("连续 {streak} 天"),
                    0xb8590a,
                    0xf0882a1c,
                ))
            })
            .child(pill("plaid/cloud.svg", "火山引擎", 0x2a62c9, 0x2a62c91a))
            .child(pill("plaid/mic.svg", mic, 0x555555, 0x00000010))
            .when(super::on_if_one("llmEnabled"), |d| {
                d.child(pill("plaid/refine.svg", "LLM 优化", 0x7a3db8, 0x7a3db81a))
            })
    }

    fn tiles(&self, total: &Usage, cx: &App) -> Div {
        let store = &self.model.read(cx).store;
        let today = store
            .usage
            .get(&Local::now().format("%Y-%m-%d").to_string())
            .cloned()
            .unwrap_or_default();
        let speed = if total.ms > 0 {
            (total.chars as f64 / (total.ms as f64 / 60000.)) as i64
        } else {
            0
        };
        let saved = 0.max((total.chars as f64 / TYPING_CPM * 60000.) as i64 - total.ms);
        div()
            .flex()
            .flex_none()
            .gap(px(10.))
            .child(tile(
                "听写时长",
                &duration(total.ms),
                None,
                if today.n == 0 {
                    "今天还没有听写".to_string()
                } else {
                    format!("今天 {}", duration(today.ms))
                },
            ))
            .child(tile(
                "听写字数",
                &thousands(total.chars),
                None,
                format!(
                    "今天 {} · 累计 {} 句",
                    thousands(today.chars),
                    thousands(total.n)
                ),
            ))
            .child(tile(
                "平均语速",
                &speed.to_string(),
                Some("字/分"),
                format!("打字约 {} 字/分", TYPING_CPM as i64),
            ))
            .child(tile(
                "节省时间",
                &duration(saved),
                None,
                if speed > 0 {
                    format!("比打字快 {:.1} 倍", speed as f64 / TYPING_CPM)
                } else {
                    "–".into()
                },
            ))
    }

    fn chart(&self, cx: &mut Context<Self>) -> Div {
        let range = range();
        let d = days(&self.model.read(cx).store, range);
        let n = d.len();
        let picked = self.hover.filter(|i| *i < n).map(|i| &d[i]);
        let sum: i64 = d.iter().map(|(_, u)| u.chars).sum();
        let caption = match picked {
            Some((day, u)) => format!(
                "{}月{}日 {}  {} 字 · {} 句 · {}",
                day.month(),
                day.day(),
                weekday(*day),
                thousands(u.chars),
                u.n,
                duration(u.ms)
            ),
            None => format!(
                "{} 字 · 活跃 {} 天",
                thousands(sum),
                d.iter().filter(|(_, u)| u.n > 0).count()
            ),
        };
        let stride = match range {
            7 => 1,
            30 => 7,
            _ => 14,
        };
        let ratio = if range == 90 { 0.8 } else { 0.6 };
        let max = d.iter().map(|(_, u)| u.chars).max().unwrap_or(0) as f64;
        let (step, top) = ticks(max);
        let (lines, labels) = grid(step, top, PLOT_H);
        let m = self.model.clone();
        let seg = segmented(
            "overview-range",
            &["7 天", "30 天", "90 天"],
            RANGES.iter().position(|r| *r == range).unwrap_or(1),
            move |i, _, cx| m.update(cx, |m, cx| m.set("overviewRange", RANGES[i], cx)),
        );
        let hovering = picked.is_some();
        let columns = d.iter().enumerate().map(|(i, (_, u))| {
            let h = (u.chars as f64 / top) as f32;
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
                .when(u.chars > 0, |c| {
                    c.child(
                        bar()
                            .w(relative(ratio))
                            .h(relative(h))
                            .min_h(px(2.))
                            .when(hovering && self.hover != Some(i), |b| b.opacity(0.3)),
                    )
                })
        });
        let axis = d.iter().enumerate().map(|(i, (day, _))| {
            div()
                .flex_1()
                .relative()
                .when((n - 1 - i).is_multiple_of(stride), |c| {
                    c.child(axis_label(
                        format!("{}月{}日", day.month(), day.day()),
                        i == n - 1 && range == 90,
                    ))
                })
        });
        let (head, body) = boxed("每日字数", None);
        div().flex().flex_col().flex_none().child(head).child(
            body.pt(px(10.))
                .pb(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(hint(caption).text_size(px(12.)))
                        .child(seg),
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
                                .border_color(hex(0x9a9a9a))
                                .children(lines)
                                .child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .gap(px(if range == 90 { 0. } else { 2. }))
                                        .children(columns),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .h(px(16.))
                        .child(div().w(px(44.)).flex_none())
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .gap(px(if range == 90 { 0. } else { 2. }))
                                .children(axis),
                        ),
                ),
        )
    }

    fn apps(&self, total: &Usage) -> Div {
        let mut sorted: Vec<(&String, &i64)> = total.apps.iter().collect();
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
        let sum = total.chars.max(1) as f64;
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
                    .children(rows.into_iter().map(|(name, c)| {
                        let share = c as f64 / sum;
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(div().w(px(110.)).flex_none().truncate().child(name))
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(10.))
                                    .rounded(px(5.))
                                    .bg(vgrad(0xcfcfcf, 0xe6e6e6))
                                    .shadow(vec![shadow(0., 1., 1., 0., 0x00000040, true)])
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(share as f32))
                                            .min_w(px(6.))
                                            .rounded(px(5.))
                                            .overflow_hidden()
                                            .flex()
                                            .flex_col()
                                            .child(div().flex_1().bg(vgrad(0xa7cdf7, 0x4b8fe2)))
                                            .child(div().flex_1().bg(vgrad(0x3a80dc, 0x77b2f2))),
                                    ),
                            )
                            .child(
                                hint(format!("{}%", (share * 100.).round() as i64))
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
                        .font_weight(bold())
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
                    .child(line("热词", format!("{} 个", l.hotwords), 0x000000))
                    .child(line("自动规则", format!("{} 条", l.auto), 0x000000))
                    .child(line("手动修改", format!("{} 句", l.edits), 0x000000))
                    .child(line(
                        "待处理建议",
                        format!("{} 条", l.pending),
                        if l.pending > 0 { ORANGE } else { 0x000000 },
                    )),
            )
    }
}

impl Render for Overview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (total, headline) = {
            let store = &self.model.read(cx).store;
            (total(store), headline(store))
        };
        let page = super::page_body("overview").child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .gap(px(10.))
                .child(
                    div()
                        .font_weight(bold())
                        .text_color(hex(0x333333))
                        .truncate()
                        .child(headline),
                )
                .child(self.status(cx)),
        );
        if total.n == 0 {
            let (_, body) = boxed("", None);
            return page.child(
                body.flex_none()
                    .h(px(220.))
                    .items_center()
                    .justify_center()
                    .child(div().font_weight(bold()).child("还没有听写记录"))
                    .child(hint("按住快捷键说话，松开后文字会粘贴到光标处。").text_size(px(12.))),
            );
        }
        page.child(self.tiles(&total, cx))
            .child(self.chart(cx))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(12.))
                    .child(self.apps(&total))
                    .child(self.learned_card()),
            )
            .child(hint(format!(
                "节省时间按打字 {} 字/分钟估算，减去说话时长。字数：汉字每字计 1，英文和数字每词计 1。清空历史不影响统计。",
                TYPING_CPM as i64
            )))
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
        "default range shows 30 bars",
        ui.has("overview-bar-29") && !ui.has("overview-bar-30"),
        &None,
    )?;
    ui.shot("overview")?;
    ui.click("overview-range-0")?;
    expect(
        "range 7",
        config::value("overviewRange").as_deref() == Some("7")
            && ui.has("overview-bar-6")
            && !ui.has("overview-bar-7"),
        &before,
    )?;
    ui.shot("overview-7")?;
    ui.click("overview-range-2")?;
    expect(
        "range 90",
        config::value("overviewRange").as_deref() == Some("90") && ui.has("overview-bar-89"),
        &None,
    )?;
    // Pointer over the newest bar: the chart caption switches to that day.
    ui.scroll("overview-bar-89", 0.)?;
    ui.shot("overview-90-hover")?;
    ui.click("overview-range-1")?;
    expect(
        "range 30",
        config::value("overviewRange").as_deref() == Some("30") && !ui.has("overview-bar-30"),
        &None,
    )?;
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
