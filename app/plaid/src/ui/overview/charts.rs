//! 概览 cards under the main chart: the weekday × hour grid, speaking speed, what the LLM and the local rules
//! changed, and the half-year calendar.
use super::super::mav::*;
use super::super::perf::diff;
use super::super::refine::RULE_TILES;
use super::{Overview, boxed, thousands, weekday};
use chrono::{Datelike, Duration, NaiveDate};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::pct;
use plaid_core::rules::{KEYS, Rules};
use plaid_core::store::{Record, Store};

const WEEKS: usize = 26;
const DAYS: usize = WEEKS * 7;
const CELL: f32 = 14.;
const GAP: f32 = 3.;
const WEEKDAYS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];

/// Half a year of daily characters from usage.json, shown under 全部.
pub(super) struct Calendar {
    /// Monday of the first column.
    start: NaiveDate,
    today: NaiveDate,
    /// (characters, dictations) per day, column-major Monday..Sunday.
    heat: Vec<(i64, i64)>,
    cuts: [i64; 3],
    total: i64,
    active: usize,
    streak: usize,
}

impl Calendar {
    pub(super) fn new(store: &Store, today: NaiveDate) -> Calendar {
        let start = today
            - Duration::days(today.weekday().num_days_from_monday() as i64)
            - Duration::weeks(WEEKS as i64 - 1);
        let mut c = Calendar {
            start,
            today,
            heat: vec![],
            cuts: [1; 3],
            total: 0,
            active: 0,
            streak: 0,
        };
        let mut run = 0;
        for i in 0..DAYS {
            let d = start + Duration::days(i as i64);
            let (chars, n) = match super::usage_on(store, d).filter(|_| d <= today) {
                Some(u) => (u.chars, u.n),
                None => (0, 0),
            };
            c.heat.push((chars, n));
            if chars > 0 {
                c.total += chars;
                c.active += 1;
                run += 1;
                c.streak = c.streak.max(run);
            } else {
                run = 0;
            }
        }
        c.cuts = cuts(c.heat.iter().map(|h| h.0));
        c
    }
}

/// Level bounds 1..3 at the quartiles of the non-zero values, rounded; level 4 is anything above the last.
fn cuts(values: impl Iterator<Item = i64>) -> [i64; 3] {
    let mut v: Vec<i64> = values.filter(|c| *c > 0).collect();
    v.sort_unstable();
    let round = |x: i64| {
        let step = if x >= 1000 {
            100
        } else if x >= 100 {
            10
        } else {
            1
        };
        ((x + step / 2) / step * step).max(1)
    };
    let mut out = [1; 3];
    for (k, q) in [0.25, 0.5, 0.75].into_iter().enumerate() {
        out[k] = pct(&v, q).map_or(1, round);
    }
    out
}

fn level(cuts: &[i64; 3], c: i64) -> usize {
    if c <= 0 {
        0
    } else {
        1 + cuts.iter().filter(|t| c >= **t).count()
    }
}

fn level_color(level: usize) -> Hsla {
    match level {
        0 => hex(RAISED2),
        1 => tint(CYAN, 0x38),
        2 => tint(CYAN, 0x6b),
        3 => tint(CYAN, 0xad),
        _ => hex(CYAN),
    }
}

fn cell(level: usize) -> Div {
    div()
        .size(px(CELL))
        .flex_none()
        .rounded(px(3.))
        .bg(level_color(level))
        .when(level == 4, |d| d.shadow(vec![glow(CYAN, 8., 0x59)]))
}

fn small(s: impl Into<SharedString>) -> Div {
    div()
        .font(mono())
        .text_size(px(10.))
        .text_color(hex(TEXT3))
        .whitespace_nowrap()
        .child(s.into())
}

fn big(s: String) -> Div {
    div()
        .font(mono())
        .text_size(px(32.))
        .line_height(px(38.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(hex(TEXT))
        .child(s)
}

fn chars_k(v: i64) -> String {
    if v >= 1000 {
        let k = v as f64 / 1000.;
        if k.fract().abs() < 0.05 {
            format!("{k:.0}k")
        } else {
            format!("{k:.1}k")
        }
    } else {
        v.to_string()
    }
}

/// 少 ■■■■■ 多
fn ramp() -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(3.))
        .child(small("少").mr(px(3.)))
        .children((0..5).map(|k| cell(k).size(px(9.))))
        .child(small("多").ml(px(3.)))
}

/// What the LLM and the local rules changed in the range's records.
#[derive(Default)]
pub(super) struct Edits {
    n: usize,
    llm: usize,
    llm_punct: usize,
    llm_words: usize,
    /// Characters the LLM changed, over the characters it was given.
    llm_changed: (usize, usize),
    /// Records each rule (in `KEYS` order) changes on its own, and records the rules changed at all.
    rules: [usize; 6],
    ruled: usize,
}

impl Edits {
    pub(super) fn new<'a>(rows: impl Iterator<Item = &'a Record>) -> Edits {
        let mut r = Edits::default();
        let single: Vec<Rules> = KEYS
            .iter()
            .map(|k| Rules::load(|key| (key == *k).then(|| "1".to_string())))
            .collect();
        for row in rows {
            r.n += 1;
            if row.llm == "ok" {
                r.llm += 1;
                let (del, ins) = diff(&row.raw, &row.text);
                let spans = del
                    .iter()
                    .map(|x| &row.raw[x.clone()])
                    .chain(ins.iter().map(|x| &row.text[x.clone()]));
                let (mut punct, mut words) = (false, false);
                for s in spans {
                    for c in s.chars() {
                        r.llm_changed.0 += 1;
                        if plaid_core::text::is_punct(c) {
                            punct = true;
                        } else if !c.is_whitespace() {
                            words = true;
                        }
                    }
                }
                r.llm_changed.1 += row.raw.chars().count();
                r.llm_punct += punct as usize;
                r.llm_words += words as usize;
            }
            if let Some(asr) = &row.asr {
                r.ruled += 1;
                // A rule that ran left `raw` as a fixed point (rules are idempotent); one that would change
                // `asr` but not `raw` is taken as switched on.
                for (k, rule) in single.iter().enumerate() {
                    if rule.apply(asr) != *asr && rule.apply(&row.raw) == row.raw {
                        r.rules[k] += 1;
                    }
                }
            }
        }
        r
    }

    /// (label, records, LLM?) rows, rules without hits left out.
    fn rows(&self) -> Vec<(String, usize, bool)> {
        let mut v = vec![];
        if self.llm > 0 {
            v.push(("LLM · 标点".to_string(), self.llm_punct, true));
            v.push(("LLM · 字词".to_string(), self.llm_words, true));
        }
        for (k, n) in self.rules.iter().enumerate() {
            if *n > 0 {
                v.push((RULE_TILES[k].0.to_string(), *n, false));
            }
        }
        v
    }

    fn summary(&self) -> String {
        let mut s = String::new();
        if self.llm > 0 {
            let share = self.llm * 100 / self.n.max(1);
            let changed = self.llm_changed.0 as f64 * 100. / self.llm_changed.1.max(1) as f64;
            let most = if self.llm_punct >= self.llm_words {
                "标点"
            } else {
                "字词"
            };
            s += &format!(
                "{} 句里 {share}% 经过 LLM，平均改动 {changed:.1}% 的字，改得最多的是{most}。",
                self.n
            );
        }
        if self.ruled > 0 {
            let top = (0..6).max_by_key(|k| self.rules[*k]).unwrap_or(0);
            s += &format!(
                "本地规则改了 {} 句，最常见的是「{}」。",
                self.ruled, RULE_TILES[top].0
            );
        }
        if s.is_empty() {
            s = format!("{} 句都没有经过 LLM 润色，本地规则也没有改动。", self.n);
        }
        s
    }
}

impl Overview {
    /// Characters by weekday and hour over the range.
    pub(super) fn week(&self, cx: &mut Context<Self>) -> Div {
        let w = &self.data.week;
        let cuts = cuts(w.iter().flatten().map(|c| c.0));
        let any = w.iter().flatten().any(|c| c.0 > 0);
        let caption = match self.hover_week.filter(|i| *i < 7 * 24) {
            Some(i) => {
                let (wd, h) = (i / 24, i % 24);
                match w[wd][h] {
                    (0, _) => format!("周{} {h}:00–{}:00 · 没有听写", WEEKDAYS[wd], h + 1),
                    (c, n) => format!(
                        "周{} {h}:00–{}:00 · {} 字 · {n} 句",
                        WEEKDAYS[wd],
                        h + 1,
                        thousands(c)
                    ),
                }
            }
            None if any => {
                let (wd, h) = (0..7 * 24)
                    .map(|i| (i / 24, i % 24))
                    .max_by_key(|(wd, h)| w[*wd][*h].0)
                    .unwrap_or((0, 0));
                let mut by_hour = [0i64; 24];
                for row in w {
                    for (h, c) in row.iter().enumerate() {
                        by_hour[h] += c.0;
                    }
                }
                let peak = (0..24).max_by_key(|h| by_hour[*h]).unwrap_or(0);
                format!(
                    "说得最多的时段是 {peak}:00–{}:00，最多的一格是周{} {h} 时",
                    peak + 1,
                    WEEKDAYS[wd]
                )
            }
            None => "这段时间没有按小时的记录".into(),
        };
        let caption = match self.data.week_from {
            Some(d) if self.hover_week.is_none() => {
                format!(
                    "{caption}。按小时的统计从 {}月{}日 开始",
                    d.month(),
                    d.day()
                )
            }
            _ => caption,
        };
        let hovered = self.hover_week;
        let rows = (0..7).map(|wd| {
            div()
                .flex()
                .items_center()
                .gap(px(GAP))
                .child(small(WEEKDAYS[wd]).w(px(18.)).flex_none().text_size(px(9.)))
                .children((0..24).map(|h| {
                    let i = wd * 24 + h;
                    hit(SharedString::from(format!("overview-week-{i}")))
                        .flex_1()
                        .h(px(16.))
                        .rounded(px(3.))
                        .bg(level_color(level(&cuts, w[wd][h].0)))
                        .when(hovered == Some(i), |c| c.border_1().border_color(hex(TEXT)))
                        .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                            if *on {
                                this.hover_week = Some(i);
                            } else if this.hover_week == Some(i) {
                                this.hover_week = None;
                            }
                            cx.notify();
                        }))
                }))
        });
        let axis = div()
            .flex()
            .gap(px(GAP))
            .child(div().w(px(18.)).flex_none())
            .children((0..24).map(|h| {
                div()
                    .flex_1()
                    .when(h % 3 == 0, |c| c.child(small(format!("{h}"))))
            }));
        let (head, body) = boxed("星期 × 时段", Some(ramp().into_any_element()));
        div().flex().flex_col().flex_none().child(head).child(
            body.gap(px(GAP))
                .child(hint(caption).text_size(px(12.)).mb(px(4.)))
                .children(rows)
                .child(axis.mt(px(2.))),
        )
    }

    /// Characters per minute of audio, per chart bucket (the last 7 days for 今天).
    pub(super) fn speed(&self, cx: &mut Context<Self>) -> Div {
        let d = &self.data;
        let b = &d.speed;
        let n = b.len();
        let values: Vec<Option<f64>> = b.iter().map(|k| k.sum.speed()).collect();
        let points: Vec<(usize, f64)> = values
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.map(|v| (i, v)))
            .collect();
        let mut all = super::Sum::default();
        for k in b {
            all.add(k.sum.n, k.sum.ms, k.sum.chars);
        }
        let avg = all.speed();
        let now = d.sum.speed();
        let delta = now.zip(d.prev.and_then(|p| p.speed())).map(|(a, p)| {
            let v = (a - p).round() as i64;
            super::change(
                if v >= 0 {
                    format!("↑ {v}")
                } else {
                    format!("↓ {}", -v)
                },
                v >= 0,
            )
        });
        let head = div()
            .flex()
            .items_baseline()
            .gap(px(8.))
            .child(big(now.map_or("–".into(), |v| format!("{v:.0}"))))
            .child(div().text_color(hex(TEXT2)).child("字/分"))
            .children(delta.map(|c| c.ml_auto()));
        let middle = match self.hover_speed.filter(|i| *i < n) {
            Some(i) => match values[i] {
                Some(v) => format!("{} · {v:.0} 字/分", b[i].name()),
                None => format!("{} · 没有听写", b[i].name()),
            },
            None => avg.map_or(String::new(), |a| format!("虚线 = 平均 {a:.0}")),
        };
        let chart = if points.len() < 2 {
            div()
                .h(px(96.))
                .flex()
                .items_center()
                .justify_center()
                .child(hint("有两段以上的听写后显示速度变化"))
        } else {
            let avg = avg.unwrap_or(0.);
            let lo = points.iter().map(|p| p.1).fold(avg, f64::min) * 0.85;
            let hi = points.iter().map(|p| p.1).fold(avg, f64::max) * 1.08;
            let hover = self.hover_speed;
            let plot = canvas(
                |_, _, _| {},
                move |b, _, window, _| {
                    let at = |i: usize, v: f64| {
                        point(
                            b.origin.x + b.size.width * ((i as f32 + 0.5) / n as f32),
                            b.origin.y + b.size.height * (1. - ((v - lo) / (hi - lo)) as f32),
                        )
                    };
                    let bottom = b.origin.y + b.size.height;
                    let mut area = PathBuilder::fill();
                    let (i0, _) = points[0];
                    area.move_to(point(at(i0, 0.).x, bottom));
                    for (i, v) in &points {
                        area.line_to(at(*i, *v));
                    }
                    area.line_to(point(at(points[points.len() - 1].0, 0.).x, bottom));
                    area.close();
                    if let Ok(p) = area.build() {
                        window.paint_path(
                            p,
                            linear_gradient(
                                180.,
                                linear_color_stop(tint(CYAN, 0x47), 0.),
                                linear_color_stop(tint(CYAN, 0x00), 1.),
                            ),
                        );
                    }
                    let y = at(0, avg).y;
                    let mut dash = PathBuilder::stroke(px(1.)).dash_array(&[px(2.), px(4.)]);
                    dash.move_to(point(b.origin.x, y));
                    dash.line_to(point(b.origin.x + b.size.width, y));
                    if let Ok(p) = dash.build() {
                        window.paint_path(p, hexa(LINE3));
                    }
                    let mut line = PathBuilder::stroke(px(2.));
                    line.move_to(at(points[0].0, points[0].1));
                    for (i, v) in &points[1..] {
                        line.line_to(at(*i, *v));
                    }
                    if let Ok(p) = line.build() {
                        window.paint_path(p, hex(CYAN));
                    }
                    let mut dot = |i: usize, v: f64, r: f32| {
                        let c = at(i, v);
                        window.paint_quad(
                            fill(
                                Bounds::new(c - point(px(r), px(r)), size(px(2. * r), px(2. * r))),
                                hex(CYAN),
                            )
                            .corner_radii(px(r)),
                        );
                    };
                    let (li, lv) = points[points.len() - 1];
                    dot(li, lv, 4.);
                    if let Some((i, v)) = hover.and_then(|h| points.iter().find(|p| p.0 == h)) {
                        dot(*i, *v, 3.);
                    }
                },
            )
            .absolute()
            .inset_0();
            let columns = (0..n).map(|i| {
                hit(SharedString::from(format!("overview-speed-{i}")))
                    .flex_1()
                    .h_full()
                    .when(self.hover_speed == Some(i), |c| c.bg(hexa(0xffffff08)))
                    .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                        if *on {
                            this.hover_speed = Some(i);
                        } else if this.hover_speed == Some(i) {
                            this.hover_speed = None;
                        }
                        cx.notify();
                    }))
            });
            div()
                .relative()
                .h(px(96.))
                .child(div().absolute().inset_0().flex().children(columns))
                .child(plot)
        };
        let first = b.first().map_or(String::new(), |k| {
            format!("{}月{}日", k.first.month(), k.first.day())
        });
        let title = if d.range == 1 {
            "说话速度 · 近 7 天"
        } else {
            "说话速度"
        };
        let (head_line, body) = boxed(title, None);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(head_line)
            .child(
                body.flex_1().gap(px(6.)).child(head).child(chart).child(
                    div()
                        .flex()
                        .justify_between()
                        .gap(px(6.))
                        .child(small(first))
                        .child(small(middle).text_color(hex(TEXT2)))
                        .child(small("今天")),
                ),
            )
    }

    pub(super) fn edits(&self) -> Div {
        let e = &self.data.edits;
        let rows = e.rows();
        let max = rows.iter().map(|r| r.1).max().unwrap_or(0).max(1) as f32;
        let (head, body) = boxed("润色改了什么", None);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(head)
            .child(
                body.flex_1()
                    .gap(px(10.))
                    .children(rows.into_iter().map(|(label, n, llm)| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .w(px(96.))
                                    .flex_none()
                                    .text_color(hex(TEXT))
                                    .whitespace_nowrap()
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(6.))
                                    .rounded(px(3.))
                                    .bg(hex(RAISED2))
                                    .child(
                                        div()
                                            .h_full()
                                            .rounded(px(3.))
                                            .w(relative(n as f32 / max))
                                            .bg(hex(if llm { VIOLET } else { CYAN })),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(44.))
                                    .flex_none()
                                    .flex()
                                    .justify_end()
                                    .font(mono())
                                    .text_color(hex(TEXT2))
                                    .child(format!("{n} 句")),
                            )
                    }))
                    .child(div().flex_1())
                    .child(if e.n == 0 {
                        hint("这段时间没有听写记录")
                    } else {
                        div()
                            .px(px(12.))
                            .py(px(10.))
                            .rounded(px(10.))
                            .bg(tint(VIOLET, 0x14))
                            .border_1()
                            .border_color(tint(VIOLET, 0x33))
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(hex(TEXT))
                            .child(e.summary())
                    }),
            )
    }

    /// Half a year of days, under 全部.
    pub(super) fn calendar(&self, cx: &mut Context<Self>) -> Div {
        let Some(r) = &self.data.calendar else {
            return div();
        };
        let (start, today) = (r.start, r.today);
        let date = |i: usize| start + Duration::days(i as i64);
        let caption = match self.hover_heat.filter(|i| *i < r.heat.len()) {
            Some(i) => {
                let d = date(i);
                let head = format!("{}月{}日 {}", d.month(), d.day(), weekday(d));
                match r.heat[i] {
                    (0, _) => format!("{head} · 没有听写"),
                    (c, n) => format!("{head} · {} 字 · {n} 句", thousands(c)),
                }
            }
            None => format!(
                "字 · 近半年 · 活跃 {} 天 · 最长连续 {} 天",
                r.active, r.streak
            ),
        };
        let columns = (0..WEEKS).map(|w| {
            div()
                .flex()
                .flex_col()
                .gap(px(GAP))
                .children((0..7).map(|dow| {
                    let i = w * 7 + dow;
                    if date(i) > today {
                        return div().size(px(CELL)).into_any_element();
                    }
                    let hovered = self.hover_heat == Some(i);
                    hit(SharedString::from(format!("overview-heat-{i}")))
                        .child(
                            cell(level(&r.cuts, r.heat[i].0))
                                .when(hovered, |c| c.border_1().border_color(hex(TEXT))),
                        )
                        .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                            if *on {
                                this.hover_heat = Some(i);
                            } else if this.hover_heat == Some(i) {
                                this.hover_heat = None;
                            }
                            cx.notify();
                        }))
                        .into_any_element()
                }))
        });
        let months = (0..WEEKS).map(|w| {
            let d = date(w * 7);
            // A month is labelled on its first column; the leading partial month only if it has room.
            let first = if w == 0 {
                (1..4).all(|k| date(k * 7).month() == d.month())
            } else {
                date((w - 1) * 7).month() != d.month()
            };
            div()
                .w(px(CELL))
                .flex_none()
                .when(first, |c| c.child(small(format!("{}月", d.month()))))
        });
        let weekdays = div()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .children((0..7).map(|k| {
                small(if k % 2 == 0 { WEEKDAYS[k] } else { "" })
                    .h(px(CELL))
                    .line_height(px(CELL))
                    .text_size(px(9.))
            }));
        let c = r.cuts;
        let ramp = [
            "0".to_string(),
            format!("< {}", chars_k(c[0])),
            format!("< {}", chars_k(c[1])),
            format!("< {}", chars_k(c[2])),
            format!("{}+", chars_k(c[2])),
        ];
        let legend = div()
            .ml_auto()
            .flex()
            .flex_col()
            .justify_end()
            .gap(px(4.))
            .when(r.active > 0, |d| {
                d.children(ramp.into_iter().enumerate().map(|(k, label)| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(cell(k).size(px(10.)))
                        .child(small(label).text_color(hex(TEXT2)))
                }))
            });
        let (head, body) = boxed("近半年", None);
        div().flex().flex_col().flex_none().child(head).child(
            body.gap(px(12.))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(14.))
                        .child(big(thousands(r.total)))
                        .child(div().text_color(hex(TEXT2)).child(caption)),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(weekdays)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .child(div().flex().gap(px(GAP)).children(columns))
                                .child(div().flex().gap(px(GAP)).children(months)),
                        )
                        .child(legend),
                ),
        )
    }
}
