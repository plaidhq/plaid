//! 报告 summary cards above the diagnostics: half a year of daily characters, speaking speed, release-to-paste
//! spread and what the LLM and the local rules changed.
use super::super::mav::*;
use super::super::overview::thousands;
use super::super::refine::RULE_TILES;
use super::{Perf, diff, ok};
use chrono::{Datelike, Duration, Local, NaiveDate};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::pct;
use plaid_core::rules::{KEYS, Rules};
use plaid_core::store::{Record, Store};

const WEEKS: usize = 26;
const DAYS: usize = WEEKS * 7;
const CELL: f32 = 14.;
const GAP: f32 = 3.;
const SPEED_DAYS: usize = 30;
const BINS: usize = 24;
const BIN_MS: i64 = 125;
const WEEKDAYS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];

#[derive(Default)]
pub(super) struct Report {
    /// Monday of the first heatmap column.
    start: Option<NaiveDate>,
    today: Option<NaiveDate>,
    /// (characters, dictations) per heatmap day, column-major Monday..Sunday.
    heat: Vec<(i64, i64)>,
    /// Upper bounds of levels 1..3; level 4 is anything at or above the last.
    cuts: [i64; 3],
    total: i64,
    active: usize,
    streak: usize,
    /// Characters per minute of audio, per day for the last 30 days.
    speed: Vec<Option<f64>>,
    speed_avg: Option<f64>,
    speed_week: Option<f64>,
    /// (direct, LLM) dictations per 125 ms release → paste bin.
    hist: [(usize, usize); BINS],
    p50: (Option<i64>, Option<i64>),
    pasted: usize,
    /// Records in the range, LLM-refined ones, and those whose diff touched punctuation / other characters.
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

impl Report {
    pub(super) fn new(store: &Store, rows: &[Record], samples: &[&Record]) -> Report {
        let mut r = Report::default();
        let today = Local::now().date_naive();
        let start = today
            - Duration::days(today.weekday().num_days_from_monday() as i64)
            - Duration::weeks(WEEKS as i64 - 1);
        r.today = Some(today);
        r.start = Some(start);
        let day = |d: NaiveDate| store.usage.get(&d.format("%Y-%m-%d").to_string());
        let mut run = 0;
        for i in 0..DAYS {
            let d = start + Duration::days(i as i64);
            let (c, n) = match day(d).filter(|_| d <= today) {
                Some(u) => (u.chars, u.n),
                None => (0, 0),
            };
            r.heat.push((c, n));
            if c > 0 {
                r.total += c;
                r.active += 1;
                run += 1;
                r.streak = r.streak.max(run);
            } else {
                run = 0;
            }
        }
        let mut active: Vec<i64> = r.heat.iter().map(|h| h.0).filter(|c| *c > 0).collect();
        active.sort_unstable();
        let round = |v: i64| {
            let step = if v >= 1000 { 100 } else { 10 };
            ((v + step / 2) / step * step).max(1)
        };
        for (k, q) in [0.25, 0.5, 0.75].into_iter().enumerate() {
            r.cuts[k] = pct(&active, q).map_or(1, round);
        }

        let mut sum = (0, 0);
        let mut week = (0, 0);
        for k in 0..SPEED_DAYS {
            let d = today - Duration::days((SPEED_DAYS - 1 - k) as i64);
            let u = day(d).filter(|u| u.ms > 0 && u.chars > 0);
            r.speed
                .push(u.map(|u| u.chars as f64 * 60_000. / u.ms as f64));
            if let Some(u) = u {
                sum = (sum.0 + u.chars, sum.1 + u.ms);
                if k >= SPEED_DAYS - 7 {
                    week = (week.0 + u.chars, week.1 + u.ms);
                }
            }
        }
        let rate = |(c, ms): (i64, i64)| (ms > 0).then(|| c as f64 * 60_000. / ms as f64);
        r.speed_avg = rate(sum);
        r.speed_week = rate(week);

        let (mut direct, mut refined) = (vec![], vec![]);
        for s in samples {
            let b = ((s.total_ms.max(0) / BIN_MS) as usize).min(BINS - 1);
            if ok(s) {
                r.hist[b].1 += 1;
                refined.push(s.total_ms);
            } else {
                r.hist[b].0 += 1;
                direct.push(s.total_ms);
            }
        }
        direct.sort_unstable();
        refined.sort_unstable();
        r.p50 = (pct(&direct, 0.5), pct(&refined, 0.5));
        r.pasted = samples.len();

        let single: Vec<Rules> = KEYS
            .iter()
            .map(|k| Rules::load(|key| (key == *k).then(|| "1".to_string())))
            .collect();
        r.n = rows.len();
        for row in rows {
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

    fn level(&self, c: i64) -> usize {
        if c <= 0 {
            0
        } else {
            1 + self.cuts.iter().filter(|t| c >= **t).count()
        }
    }

    /// (label, records, LLM?) rows for the 改了什么 card, rules without hits left out.
    fn edits(&self) -> Vec<(String, usize, bool)> {
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
                "最近 {} 句里 {share}% 经过 LLM，平均改动 {changed:.1}% 的字，改得最多的是{most}。",
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
            s = format!("最近 {} 句没有经过 LLM 润色，本地规则也没有改动。", self.n);
        }
        s
    }
}

fn panel() -> Div {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .bg(hex(SURFACE))
        .border_1()
        .border_color(hexa(LINE))
        .rounded(px(16.))
        .px(px(18.))
        .py(px(16.))
}

fn title(s: &str) -> Div {
    div()
        .text_size(px(12.))
        .text_color(hex(TEXT2))
        .child(s.to_string())
}

fn big(s: String) -> Div {
    div()
        .font(mono())
        .text_size(px(40.))
        .line_height(px(44.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(hex(TEXT))
        .child(s)
}

fn small(s: impl Into<SharedString>) -> Div {
    div()
        .font(mono())
        .text_size(px(10.))
        .text_color(hex(TEXT3))
        .whitespace_nowrap()
        .child(s.into())
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

fn seconds(ms: i64) -> String {
    format!("{:.2}s", ms as f64 / 1000.)
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

impl Perf {
    /// Heatmap and speed: from the daily usage totals, so they cover more than the kept records.
    pub(super) fn report_usage(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_none()
            .gap(px(14.))
            .child(
                self.heatmap(cx)
                    .flex_basis(px(0.))
                    .flex_grow(2.)
                    .flex_shrink(1.),
            )
            .child(self.speed(cx).flex_1())
    }

    /// Release → paste spread and what changed, for the records in the range.
    pub(super) fn report_records(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_none()
            .gap(px(14.))
            .child(self.histogram(cx).flex_1())
            .child(self.edits().flex_1())
    }

    fn heatmap(&self, cx: &mut Context<Self>) -> Div {
        let r = &self.report;
        let (Some(start), Some(today)) = (r.start, r.today) else {
            return div();
        };
        let date = |i: usize| start + Duration::days(i as i64);
        let caption = match self.hover_heat.filter(|i| *i < r.heat.len()) {
            Some(i) => {
                let d = date(i);
                let head = format!(
                    "{}月{}日 周{}",
                    d.month(),
                    d.day(),
                    WEEKDAYS[d.weekday().num_days_from_monday() as usize]
                );
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
                    hit(SharedString::from(format!("perf-heat-{i}")))
                        .child(
                            cell(r.level(r.heat[i].0))
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
        panel()
            .gap(px(12.))
            .px(px(20.))
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
            )
    }

    fn speed(&self, cx: &mut Context<Self>) -> Div {
        let r = &self.report;
        let head = div()
            .flex()
            .items_baseline()
            .gap(px(8.))
            .child(big(r
                .speed_week
                .map_or("–".into(), |v| format!("{v:.0}"))))
            .child(div().text_color(hex(TEXT2)).child("字/分"))
            .when_some(r.speed_week.zip(r.speed_avg), |d, (w, a)| {
                let delta = (w - a).round() as i64;
                d.child(
                    div()
                        .ml_auto()
                        .font(mono())
                        .text_size(px(11.))
                        .text_color(hex(if delta >= 0 { GREEN } else { TEXT2 }))
                        .child(if delta >= 0 {
                            format!("↑ {delta}")
                        } else {
                            format!("↓ {}", -delta)
                        }),
                )
            });
        let points: Vec<(usize, f64)> = r
            .speed
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.map(|v| (i, v)))
            .collect();
        let n = r.speed.len();
        let today = r.today.unwrap_or_default();
        let middle = match self.hover_speed.filter(|i| *i < n) {
            Some(i) => {
                let d = today - Duration::days((n - 1 - i) as i64);
                match r.speed[i] {
                    Some(v) => format!("{}月{}日 · {v:.0} 字/分", d.month(), d.day()),
                    None => format!("{}月{}日 · 没有听写", d.month(), d.day()),
                }
            }
            None => r
                .speed_avg
                .map_or(String::new(), |a| format!("虚线 = 30 天平均 {a:.0}")),
        };
        let first = today - Duration::days(n.saturating_sub(1) as i64);
        let chart = if points.len() < 2 {
            div()
                .h(px(96.))
                .flex()
                .items_center()
                .justify_center()
                .child(hint("说话满两天后显示近 30 天的速度"))
        } else {
            let avg = r.speed_avg.unwrap_or(0.);
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
                hit(SharedString::from(format!("perf-speed-{i}")))
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
        panel()
            .gap(px(6.))
            .child(title("说话速度 · 近 7 天"))
            .child(head)
            .child(chart)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap(px(6.))
                    .child(small(format!("{}月{}日", first.month(), first.day())))
                    .child(small(middle).text_color(hex(TEXT2)))
                    .child(small("今天")),
            )
    }

    fn histogram(&self, cx: &mut Context<Self>) -> Div {
        let r = &self.report;
        let max = r.hist.iter().map(|b| b.0 + b.1).max().unwrap_or(0).max(1) as f32;
        let hovering = self.hover_hist.is_some();
        let bars = r.hist.iter().enumerate().map(|(i, (d, l))| {
            let seg = |n: usize, color: u32, top: bool| {
                div()
                    .flex_none()
                    .w_full()
                    .h(relative(n as f32 / max))
                    .min_h(px(2.))
                    .bg(hex(color))
                    .when(top, |s| s.rounded_t(px(3.)))
            };
            hit(SharedString::from(format!("perf-hist-{i}")))
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .justify_end()
                .gap(px(2.))
                .when(hovering && self.hover_hist != Some(i), |c| c.opacity(0.4))
                .when(*l > 0, |c| c.child(seg(*l, VIOLET, true)))
                .when(*d > 0, |c| c.child(seg(*d, CYAN, *l == 0)))
                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                    if *on {
                        this.hover_hist = Some(i);
                    } else if this.hover_hist == Some(i) {
                        this.hover_hist = None;
                    }
                    cx.notify();
                }))
        });
        let caption = match self.hover_hist.filter(|i| *i < BINS) {
            Some(i) => {
                let (d, l) = r.hist[i];
                let lo = i as i64 * BIN_MS;
                let span = if i == BINS - 1 {
                    format!("{lo} ms 以上")
                } else {
                    format!("{lo}–{} ms", lo + BIN_MS)
                };
                format!("{span} · 直接 {d} 句 · LLM {l} 句")
            }
            None => format!("最近 {} 句已粘贴的听写 · 每格 {BIN_MS} ms", r.pasted),
        };
        let key = |name: &str, color: u32| {
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .text_size(px(11.))
                .text_color(hex(TEXT2))
                .child(div().w(px(8.)).h(px(3.)).rounded(px(2.)).bg(hex(color)))
                .child(name.to_string())
        };
        let p50 = |v: Option<i64>, size: f32, color: u32, label: &str| {
            v.map(|v| {
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(size))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(hex(color))
                            .child(seconds(v)),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(hex(TEXT2))
                            .child(label.to_string()),
                    )
            })
        };
        panel()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(title("松开到粘贴").flex_1())
                    .child(key("直接", CYAN))
                    .child(key("LLM", VIOLET)),
            )
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(14.))
                    .font(mono())
                    .children(p50(r.p50.0, 28., TEXT, "P50 直接"))
                    .children(p50(r.p50.1, 20., VIOLET, "P50 LLM")),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(110.))
                    .mt(px(6.))
                    .flex()
                    .items_end()
                    .gap(px(3.))
                    .border_b_1()
                    .border_color(hexa(LINE2))
                    .children(bars),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(small("0s"))
                    .child(small("1s"))
                    .child(small("2s"))
                    .child(small("3s+")),
            )
            .child(hint(caption).text_size(px(11.)))
    }

    fn edits(&self) -> Div {
        let r = &self.report;
        let rows = r.edits();
        let max = rows.iter().map(|e| e.1).max().unwrap_or(0).max(1) as f32;
        panel()
            .gap(px(10.))
            .child(title("LLM 和规则改了什么"))
            .children(rows.into_iter().map(|(label, n, llm)| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .text_size(px(12.))
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
            .child(
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
                    .child(r.summary()),
            )
    }
}
