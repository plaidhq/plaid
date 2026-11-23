//! 性能 release → paste histogram, split into direct and LLM-refined pastes.
use super::super::mav::*;
use super::{Perf, ok};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::pct;
use plaid_core::store::Record;

const BINS: usize = 24;
const BIN_MS: i64 = 125;

#[derive(Default)]
pub(super) struct Report {
    /// (direct, LLM) dictations per 125 ms release → paste bin.
    hist: [(usize, usize); BINS],
    p50: (Option<i64>, Option<i64>),
    pasted: usize,
}

impl Report {
    pub(super) fn new(samples: &[&Record]) -> Report {
        let mut r = Report::default();
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

        r
    }
}

fn panel() -> Div {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .bg(hexa(GLASS))
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

fn small(s: impl Into<SharedString>) -> Div {
    div()
        .font(mono())
        .text_size(px(10.))
        .text_color(hex(TEXT3))
        .whitespace_nowrap()
        .child(s.into())
}

fn seconds(ms: i64) -> String {
    format!("{:.2}s", ms as f64 / 1000.)
}

impl Perf {
    pub(super) fn histogram(&self, cx: &mut Context<Self>) -> Div {
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
                .when(*l > 0, |c| c.child(seg(*l, LLM, true)))
                .when(*d > 0, |c| c.child(seg(*d, ACCENT, *l == 0)))
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
            None => format!("{} 句已粘贴的听写 · 每格 {BIN_MS} ms", r.pasted),
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
                    .child(key("直接", ACCENT))
                    .child(key("LLM", LLM)),
            )
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(14.))
                    .font(mono())
                    .children(p50(r.p50.0, 28., TEXT, "P50 直接"))
                    .children(p50(r.p50.1, 20., LLM, "P50 LLM")),
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
}
