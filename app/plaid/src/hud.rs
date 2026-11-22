// Recording HUD: a small always-on-top window that never takes focus and lets the mouse through.
// GPUI's PopUp kind gives a non-activating NSPanel at NSPopUpWindowLevel on macOS and a
// WS_EX_TOOLWINDOW | WS_EX_TOPMOST window on Windows; the rest is patched through the native handle.
use crate::ui::mav;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::Hud as Msg;
use std::time::{Duration, Instant};

/// The panel per the design; the window adds room around it for the drop shadow. The panel sits at the bottom
/// center of the window and fits the text: it widens up to `PANEL_MAX_W`, then grows upward up to `MAX_LINES`.
/// The window is sized for the largest panel. The panel holds only the text; the light along its bottom edge carries
/// the input level and the state.
const PANEL_MIN_W: f32 = 260.;
const PANEL_MAX_W: f32 = 560.;
const PANEL_PAD_X: f32 = 16.;
const PANEL_PAD_Y: f32 = 10.;
const RADIUS: f32 = 12.;
/// Label and key hints above the text, only for states that ask something of the user.
const HEADER_H: f32 = 16.;
const HEADER_GAP: f32 = 4.;
const TEXT_SIZE: f32 = 13.5;
const LINE_H: f32 = 19.;
const MAX_LINES: usize = 3;
/// Panel width around the text.
const CHROME_W: f32 = 2. * PANEL_PAD_X;
/// Width the text wraps at.
const TEXT_MAX_W: f32 = PANEL_MAX_W - CHROME_W;
const PANEL_MAX_H: f32 = 2. * PANEL_PAD_Y + HEADER_H + HEADER_GAP + MAX_LINES as f32 * LINE_H;
const PAD_TOP: f32 = 14.;
const PAD_BOTTOM: f32 = 34.;
const W: f32 = PANEL_MAX_W + 40.;
const H: f32 = PAD_TOP + PANEL_MAX_H + PAD_BOTTOM;
/// Gap between the panel and the bottom of the visible screen area.
const LIFT: f32 = 40.;
/// Edge light: thickness, and its width as a fraction of the panel at silence and at full level.
const EDGE_H: f32 = 2.;
const EDGE_MIN: f32 = 0.08;
const EDGE_MAX: f32 = 0.8;
/// While processing, a segment of this fraction of the panel sweeps across once per `SWEEP_S` (there and back).
const SWEEP_W: f32 = 0.28;
const SWEEP_S: f32 = 1.6;
/// A state change morphs the edge light and text color over this long instead of cutting.
const MORPH_S: f32 = 0.28;
/// The auto-hide fades the panel out over this long.
const FADE_S: f32 = 0.2;
/// New text types on at a speed (characters per 60 Hz frame) that eases toward clearing the backlog in `TYPE_CATCH`
/// frames, by `TYPE_EASE` per frame, never below `TYPE_MIN`. Speech is recognized 2-3 characters every half second
/// or so; pacing to that rate keeps the text flowing between partials instead of typing each one at once and
/// stalling. Replaying a real 27 s utterance's partials, this trails the recognized text by about 0.7 s (p50) and
/// waits with nothing left to type 23% of the time; a 45-frame catch trailed by 1.0 s, too slow for fast speech.
/// Once the key is released the backlog clears within a few hundred ms.
const TYPE_CATCH: f32 = 24.;
const TYPE_EASE: f32 = 0.1;
const TYPE_MIN: f32 = 0.04;
const TYPE_CATCH_END: f32 = 12.;
const TYPE_MIN_END: f32 = 0.2;
/// Characters a correction changed fade in again from this alpha over `REFADE_S` instead of swapping at once.
const REFADE_FROM: f32 = 0.3;
const REFADE_S: f32 = 0.25;
/// Each new character fades in over about this many frames.
const TYPE_FADE: f32 = 12.;
/// Panel width, text rows and scroll close this share of the gap to their target per frame (about 200 ms).
const SIZE_EASE: f32 = 0.25;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Listen,
    Quiet,
    Finishing,
    Refining,
    Done,
    Saved,
    Flash,
    Failed,
    Ask,
    Running,
    Note,
}

/// What the window has to do after a message.
#[derive(PartialEq, Debug)]
pub enum Effect {
    Ignore,
    Redraw,
    Hide,
    /// Show, then hide after this many seconds unless something newer arrives.
    Show(Option<f32>),
}

pub struct Hud {
    state: State,
    text: SharedString,
    /// The live text parses as a voice command.
    command: bool,
    /// Tap-started dictation that keeps going after key-up.
    continuous: bool,
    /// The previous state was a command run, so `Done` reports the command, not a paste.
    ran: bool,
    /// Input level in 0...1 per 100 ms buffer: rises with the input, decays over a few buffers.
    level: f32,
    /// What the edge shows: eases toward `level` every frame so 10 Hz levels move smoothly.
    shown: f32,
    /// Phase origin for the processing sweep.
    clock: Instant,
    /// Text width and rows the panel shows; both only grow during an utterance so a rewrite of the partial doesn't
    /// make the panel jump.
    text_w: f32,
    rows: usize,
    /// First wrapped row in view: the text scrolls so the row being revealed is the last one shown.
    top: f32,
    /// Width, rows and top as drawn: they ease toward the targets above. `snap` jumps them there on the next render.
    size: [f32; 3],
    snap: bool,
    /// Last measured text, so animation frames don't reshape it.
    fit: Option<(SharedString, Fit)>,
    /// Characters in `text`, how many of them are revealed so far (fractional: the head is fading in), and the
    /// reveal speed in characters per frame.
    chars: usize,
    reveal: f32,
    vel: f32,
    /// The characters a correction rewrote, and when.
    refade: Option<(std::ops::Range<usize>, Instant)>,
    /// What the last frame showed, the start of the next morph, and when that morph started.
    lit: Lit,
    from: Lit,
    morph: Option<Instant>,
    /// Auto-hide fade-out in progress since.
    fade: Option<Instant>,
    /// When the last animation frame was drawn, while frames keep coming; steps scale with the real interval.
    last: Option<Instant>,
    /// Hiding: render nothing.
    blank: bool,
    visible: bool,
    /// Bumped on every shown change; a pending auto-hide only fires if nothing changed since.
    epoch: u64,
}

/// What the light along the panel's bottom edge shows.
#[derive(PartialEq, Debug)]
enum Edge {
    /// Centered, width and brightness from the level.
    Level(u32, f32),
    /// Centered, fixed fraction of the panel.
    Fixed(u32, f32),
    /// A segment sweeping across: work in progress.
    Sweep(u32),
}

impl Hud {
    pub fn new() -> Hud {
        Hud {
            state: State::Listen,
            text: "".into(),
            command: false,
            continuous: false,
            ran: false,
            level: 0.,
            shown: 0.,
            clock: Instant::now(),
            text_w: 0.,
            rows: 1,
            top: 0.,
            size: [0.; 3],
            snap: true,
            fit: None,
            chars: 0,
            reveal: 0.,
            vel: 0.,
            refade: None,
            lit: Lit::default(),
            from: Lit::default(),
            morph: None,
            fade: None,
            last: None,
            blank: false,
            visible: false,
            epoch: 0,
        }
    }

    /// Updates the state for one controller message.
    pub fn apply(&mut self, m: Msg) -> Effect {
        let prev = self.state;
        let old = self.text.clone();
        let (state, hide_after) = match m {
            Msg::Level(r) => {
                if !matches!(self.state, State::Listen | State::Quiet) {
                    return Effect::Ignore;
                }
                let v = level(r);
                self.level = if v > self.level {
                    v
                } else {
                    self.level * 0.6 + v * 0.4
                };
                return Effect::Redraw;
            }
            Msg::Hide => return Effect::Hide,
            Msg::Unquiet if self.state != State::Quiet => return Effect::Ignore,
            Msg::Listen => {
                self.text = "".into();
                self.command = false;
                self.continuous = false;
                self.level = 0.;
                self.shown = 0.;
                self.resize();
                self.vel = 0.;
                (State::Listen, None)
            }
            Msg::Unquiet => (State::Listen, None),
            Msg::Quiet(s) => {
                self.text = s.into();
                (State::Quiet, None)
            }
            Msg::Text { text, command } => {
                self.text = text.into();
                self.command = command;
                (State::Listen, None)
            }
            Msg::Continuous => {
                self.continuous = true;
                (self.state, None)
            }
            Msg::Finishing => {
                if self.state == State::Quiet {
                    self.text = "".into();
                }
                (State::Finishing, None)
            }
            Msg::Refining(t) => self.with(t, State::Refining, None),
            Msg::Done(t) => {
                self.ran = prev == State::Running;
                self.with(t, State::Done, Some(0.7))
            }
            Msg::Saved(t) => self.with(t, State::Saved, Some(8.)),
            Msg::Flash(t) => self.with(t, State::Flash, Some(5.)),
            Msg::Failed(t) => self.with(t, State::Failed, Some(5.)),
            Msg::Ask(t) => self.with(t, State::Ask, Some(8.)),
            Msg::Running(t) => self.with(t, State::Running, None),
            Msg::Note(t) => self.with(t, State::Note, Some(4.)),
        };
        if !matches!(prev, State::Finishing | State::Running | State::Refining)
            && matches!(state, State::Finishing | State::Running | State::Refining)
        {
            self.clock = Instant::now();
        }
        // Morph from what is on screen; a fresh show starts from its target.
        if self.visible && state != prev {
            self.from = self.lit;
            self.morph = Some(Instant::now());
        } else if !self.visible {
            self.morph = None;
        }
        if !self.visible {
            self.snap = true;
        }
        self.fade = None;
        self.state = state;
        if self.text != old {
            self.chars = self.text.chars().count();
            // Within one visible utterance, as many characters as were shown stay shown (a correction swaps in place
            // rather than retracting) and the rest types on; anything else appears whole.
            let utterance = |s| {
                matches!(
                    s,
                    State::Listen | State::Finishing | State::Refining | State::Done
                )
            };
            self.refade = None;
            self.reveal = if self.visible && utterance(prev) && utterance(state) {
                let changed = changed(&old, &self.text);
                if (changed.start as f32) < self.reveal && !changed.is_empty() {
                    self.refade = Some((changed, Instant::now()));
                }
                self.reveal.min(self.chars as f32)
            } else {
                self.chars as f32
            };
        }
        if !matches!(state, State::Listen | State::Quiet | State::Finishing) {
            self.command = false;
        }
        Effect::Show(hide_after)
    }

    fn with(&mut self, t: String, s: State, hide_after: Option<f32>) -> (State, Option<f32>) {
        self.text = t.into();
        // The utterance's text stays at its size through refining and the paste; anything else sizes afresh.
        if !matches!(s, State::Refining | State::Done) {
            self.resize();
        }
        (s, hide_after)
    }

    /// Next render sizes the panel afresh from its text.
    fn resize(&mut self) {
        self.text_w = 0.;
        self.rows = 1;
    }

    /// Some of the text is still to be revealed.
    fn typing(&self) -> bool {
        self.reveal < self.chars as f32
    }

    /// Panel size or scroll is still on its way to the target.
    fn sizing(&self) -> bool {
        self.size != self.size_target()
    }

    fn size_target(&self) -> [f32; 3] {
        [
            (self.text_w + CHROME_W).clamp(PANEL_MIN_W, PANEL_MAX_W),
            self.rows as f32,
            self.top,
        ]
    }

    /// Advances animation by `n` 60 Hz frames (fractional: frames come at the display's rate, with jitter): reveal
    /// more text, ease the panel size, and take about 60 ms to close most of the gap to the latest level. False when
    /// nothing on screen would change, so a steady level costs no redraw.
    pub(crate) fn frame(&mut self, n: f32) -> bool {
        // A per-frame easing share, compounded over `n` frames.
        let ease = |k: f32| 1. - (1. - k).powf(n);
        let typing = self.typing();
        if typing {
            let (catch, min) = self.pace();
            let left = self.chars as f32 - self.reveal;
            self.vel += (left / catch - self.vel) * ease(TYPE_EASE);
            self.reveal = (self.reveal + self.vel.max(min) * n).min(self.chars as f32);
        }
        if self
            .refade
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed().as_secs_f32() >= REFADE_S)
        {
            self.refade = None;
        }
        let sizing = self.sizing();
        if sizing {
            let target = self.size_target();
            // Settle within half a pixel (width) or a fortieth of a row.
            let near = [0.5, 0.025, 0.025];
            self.size = std::array::from_fn(|i| {
                let gap = target[i] - self.size[i];
                if gap.abs() < near[i] {
                    target[i]
                } else {
                    self.size[i] + gap * ease(SIZE_EASE)
                }
            });
        }
        let gap = self.level - self.shown;
        let level = gap.abs() >= 0.002;
        self.shown = if level {
            self.shown + gap * ease(0.3)
        } else {
            self.level
        };
        typing
            || sizing
            || self.refade.is_some()
            || level
            || self.morph.is_some()
            || self.fade.is_some()
            || matches!(self.edge(), Edge::Sweep(_))
    }

    /// Fades over the full text: the newest revealed characters partly, the rest fully. The text is always laid out
    /// whole, so revealing more never moves what is already shown.
    fn fades(&self) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
        if !self.typing() && self.refade.is_none() {
            return vec![];
        }
        let fade = |f: f32| HighlightStyle {
            fade_out: Some(f),
            ..Default::default()
        };
        let tail = (self.vel.max(self.pace().1) * TYPE_FADE).max(1.);
        let n = self.reveal.ceil() as usize;
        let typed = n.saturating_sub(tail.ceil() as usize + 1);
        // Corrected characters come back up from `REFADE_FROM`, easing out.
        let (from, refade, refaded) = match &self.refade {
            Some((r, t)) => {
                let k = (t.elapsed().as_secs_f32() / REFADE_S).min(1.);
                let a = REFADE_FROM + (1. - REFADE_FROM) * (1. - (1. - k).powi(2));
                (r.start.min(typed), a, r.clone())
            }
            None => (typed, 1., 0..0),
        };
        let mut out: Vec<_> = self
            .text
            .char_indices()
            .enumerate()
            .take(n)
            .skip(from)
            .filter_map(|(k, (i, c))| {
                let mut alpha = ((self.reveal - k as f32) / tail).clamp(0., 1.);
                if refaded.contains(&k) {
                    alpha = alpha.min(refade);
                }
                (alpha < 1.).then(|| (i..i + c.len_utf8(), fade(1. - alpha)))
            })
            .collect();
        let hidden = self.byte_at(n);
        if hidden < self.text.len() {
            out.push((hidden..self.text.len(), fade(1.)));
        }
        out
    }

    /// Reveal pacing: steady while listening, catching up once the speech has ended.
    fn pace(&self) -> (f32, f32) {
        if self.state == State::Listen {
            (TYPE_CATCH, TYPE_MIN)
        } else {
            (TYPE_CATCH_END, TYPE_MIN_END)
        }
    }

    /// Byte offset of character `n` in `text`.
    fn byte_at(&self, n: usize) -> usize {
        self.text
            .char_indices()
            .nth(n)
            .map_or(self.text.len(), |(i, _)| i)
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn mid_reveal(&mut self, behind: f32) {
        self.reveal = self.chars as f32 - behind;
    }

    fn edge(&self) -> Edge {
        match self.state {
            State::Listen => Edge::Level(if self.command { CYAN } else { WHITE }, self.shown),
            State::Quiet => Edge::Fixed(AMBER, 0.3),
            State::Finishing | State::Running => Edge::Sweep(WHITE),
            // The LLM pass is told apart by color alone; esc still pastes the raw text.
            State::Refining => Edge::Sweep(VIOLET),
            State::Done | State::Note => Edge::Fixed(GREEN, 1.),
            State::Saved | State::Flash => Edge::Fixed(AMBER, 1.),
            State::Failed => Edge::Fixed(RED, 1.),
            State::Ask => Edge::Fixed(CYAN, 1.),
        }
    }

    /// Where the edge light and text color should be now, ignoring any morph.
    fn target(&self) -> Lit {
        let (x, w, color, a) = match self.edge() {
            Edge::Level(c, l) => {
                let w = EDGE_MIN + (EDGE_MAX - EDGE_MIN) * l;
                ((1. - w) / 2., w, c, 0.35 + 0.65 * l)
            }
            Edge::Fixed(c, f) => ((1. - f) / 2., f, c, 0.9),
            Edge::Sweep(c) => {
                let t = self.clock.elapsed().as_secs_f32() / SWEEP_S;
                // Starts centered, where the level light was, then swings side to side.
                let p = 0.5 + 0.5 * (t * std::f32::consts::TAU).sin();
                ((1. - SWEEP_W) * p, SWEEP_W, c, 0.75)
            }
        };
        let live = matches!(self.state, State::Listen | State::Finishing);
        let text = if self.text.is_empty() {
            0xffffff73
        } else if matches!(
            self.state,
            State::Finishing | State::Refining | State::Running
        ) {
            0xffffff99
        } else if self.state == State::Quiet {
            AMBER << 8 | 0xff
        } else if self.command && live {
            CYAN << 8 | 0xff
        } else {
            0xffffffff
        };
        Lit {
            x,
            w,
            rgb: rgba(color << 8 | 0xff),
            a,
            text: rgba(text),
        }
    }

    /// This frame's edge light and text color: the target, or on the way to it after a state change.
    fn current(&mut self) -> Lit {
        let target = self.target();
        let t = self
            .morph
            .map_or(1., |m| m.elapsed().as_secs_f32() / MORPH_S);
        self.lit = if t >= 1. {
            self.morph = None;
            target
        } else {
            // Ease out: most of the move up front, settling gently.
            self.from.lerp(&target, 1. - (1. - t).powi(3))
        };
        self.lit
    }

    /// Panel opacity: 1, or falling during the auto-hide fade.
    fn opacity(&self) -> f32 {
        self.fade.map_or(1., |f| {
            (1. - f.elapsed().as_secs_f32() / FADE_S).clamp(0., 1.)
        })
    }
}

/// Edge light geometry (fractions of the track), color and alpha, plus the text color; all interpolate.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Lit {
    x: f32,
    w: f32,
    rgb: [f32; 4],
    a: f32,
    text: [f32; 4],
}

impl Lit {
    fn lerp(&self, to: &Lit, k: f32) -> Lit {
        let f = |a: f32, b: f32| a + (b - a) * k;
        let v = |a: [f32; 4], b: [f32; 4]| std::array::from_fn(|i| f(a[i], b[i]));
        Lit {
            x: f(self.x, to.x),
            w: f(self.w, to.w),
            rgb: v(self.rgb, to.rgb),
            a: f(self.a, to.a),
            text: v(self.text, to.text),
        }
    }
}

fn rgba(c: u32) -> [f32; 4] {
    std::array::from_fn(|i| ((c >> (24 - 8 * i)) & 0xff) as f32 / 255.)
}

fn color([r, g, b, a]: [f32; 4]) -> Rgba {
    Rgba { r, g, b, a }
}

/// Buffer RMS (Int16 scale) to 0...1: log scale between 60 (silence) and 3000 (loud speech).
fn level(rms: f64) -> f32 {
    let (lo, hi) = (60f64.ln(), 3000f64.ln());
    ((rms.max(1.).ln() - lo) / (hi - lo)).clamp(0., 1.) as f32
}

const WHITE: u32 = 0xffffff;
const AMBER: u32 = 0xffc65a;
const CYAN: u32 = 0x7fd8ff;
const GREEN: u32 = 0x7ee08a;
const RED: u32 = 0xff7a70;
const VIOLET: u32 = 0xb59cff;

/// A light segment `w` wide starting at `x`, fading out toward both ends, with a soft glow from its middle.
fn light(x: f32, w: f32, rgb: [f32; 4], alpha: f32) -> Div {
    let a = alpha.clamp(0., 1.);
    let at = |a: f32| color([rgb[0], rgb[1], rgb[2], a]);
    let half = |from: f32, to: f32| {
        div().w(px(w / 2.)).h_full().bg(linear_gradient(
            90.,
            linear_color_stop(at(from), 0.),
            linear_color_stop(at(to), 1.),
        ))
    };
    div()
        .absolute()
        .bottom_0()
        .left(px(x))
        .w(px(w))
        .h(px(EDGE_H))
        .flex()
        .child(half(0., a))
        .child(half(a, 0.))
        .child(
            // Glow: a short core whose blurred shadow lights the panel edge around the middle.
            div()
                .absolute()
                .bottom_0()
                .left(px(w * 0.35))
                .w(px(w * 0.3))
                .h(px(EDGE_H))
                .shadow(vec![BoxShadow {
                    color: at(a * 0.6).into(),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(10.),
                    spread_radius: px(0.),
                    inset: false,
                }]),
        )
}

/// Translucent key cap for the dark panel.
fn key(text: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(20.))
        .h(px(HEADER_H))
        .px(px(5.))
        .rounded(px(4.))
        .bg(linear_gradient(
            180.,
            linear_color_stop(mav::hexa(0xffffff3d), 0.),
            linear_color_stop(mav::hexa(0xffffff1a), 1.),
        ))
        .border_1()
        .border_color(mav::hexa(0xffffff4d))
        .text_color(mav::hex(WHITE))
        .child(text)
}

enum Hint {
    Key(&'static str),
    Text(&'static str),
}

impl Render for Hud {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        use Hint::*;
        if self.blank {
            self.last = None;
            return div().into_any_element();
        }
        // Animation runs on the display's frames: each render steps by the time since the last one and asks for the
        // next while anything moves.
        let now = Instant::now();
        let n = self
            .last
            .map_or(1., |t| (now - t).as_secs_f32() * 60.)
            .min(4.);
        let busy = self.visible && self.frame(n);
        // Only states that ask something of the user get a header; everything else is the text alone.
        let header: Option<(&str, Vec<Hint>)> = match self.state {
            State::Saved => Some(("未粘贴", vec![Text("在历史中查看")])),
            State::Failed => Some(("失败", vec![])),
            State::Ask => Some((
                "执行命令？",
                vec![Key("⏎"), Text("执行"), Key("esc"), Text("取消")],
            )),
            _ => None,
        };
        let ((text, fades), placeholder) = if self.text.is_empty() {
            let p = match self.state {
                State::Listen => "请说话…",
                _ => "等待最终结果…",
            };
            ((SharedString::from(p), vec![]), true)
        } else {
            ((self.text.clone(), self.fades()), false)
        };
        if !matches!(&self.fit, Some((t, _)) if *t == text) {
            self.fit = Some((text.clone(), fit(window, &text)));
        }
        let Some((_, m)) = &self.fit else {
            unreachable!()
        };
        // The row holding the character being revealed is the last one in view.
        let head = self.byte_at((self.reveal.ceil() as usize).saturating_sub(1));
        let row = if placeholder {
            0
        } else {
            m.starts.partition_point(|&s| s <= head).saturating_sub(1)
        };
        self.text_w = self.text_w.max(m.w);
        if !placeholder {
            self.rows = self.rows.max((row + 1).min(MAX_LINES));
        }
        self.top = (row + 1).saturating_sub(MAX_LINES) as f32;
        if self.snap {
            self.size = self.size_target();
            self.snap = false;
        }
        let [w, rows, top] = self.size;
        // Whole pixels, and an even width so the centered panel's edges land on pixels: text never sits between
        // pixels while the panel eases.
        let panel_w = (w / 2.).round() * 2.;
        // The light runs between the corner curves, so it never spills past the rounded edge.
        let track = panel_w - 2. * RADIUS;
        let lit = self.current();
        if busy || (self.visible && self.sizing()) {
            self.last = Some(now);
            window.request_animation_frame();
        } else {
            self.last = None;
        }
        let edge = light(RADIUS + lit.x * track, lit.w * track, lit.rgb, lit.a);
        // The text wraps at the widest panel and is clipped to the rows in view, scrolled to keep the newest row
        // last; the panel easing around it never reflows it.
        let line = div()
            .h(px((rows * LINE_H).round()))
            .w(px(panel_w - CHROME_W))
            .overflow_hidden()
            .text_size(px(TEXT_SIZE))
            .line_height(px(LINE_H))
            .text_color(color(lit.text));
        let scroll = div().w(px(TEXT_MAX_W)).mt(px(-(top * LINE_H).round()));
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .items_center()
            .pb(px(PAD_BOTTOM))
            .child(
                div()
                    .relative()
                    .opacity(self.opacity())
                    .w(px(panel_w))
                    .px(px(PANEL_PAD_X))
                    .py(px(PANEL_PAD_Y))
                    .flex()
                    .flex_col()
                    .gap(px(HEADER_GAP))
                    .rounded(px(RADIUS))
                    .bg(mav::hexa(0x181818db))
                    .shadow(vec![
                        mav::shadow(0., 0., 0., 1., 0x0000008c, false),
                        mav::shadow(0., 1., 0., 0., 0xffffff24, true),
                        mav::shadow(0., 4., 12., 0., 0x00000033, false),
                    ])
                    .font(mav::font())
                    .text_color(mav::hex(WHITE))
                    .when_some(header, |d, (label, hints)| {
                        d.child(
                            div()
                                .h(px(HEADER_H))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .text_size(px(11.))
                                .child(
                                    div()
                                        .flex_1()
                                        .font_weight(mav::bold())
                                        .text_color(mav::hexa(0xffffff99))
                                        .child(label),
                                )
                                .children(hints.into_iter().map(|h| {
                                    match h {
                                        Key(k) => key(k).into_any_element(),
                                        Text(t) => div()
                                            .text_color(mav::hexa(0xffffffbf))
                                            .child(t)
                                            .into_any_element(),
                                    }
                                })),
                        )
                    })
                    .child(line.child(scroll.child(StyledText::new(text).with_highlights(fades))))
                    .child(edge),
            )
            .into_any_element()
    }
}

/// The characters of `new` that replace something in `old`: after the shared start, up to where the longest
/// remaining tail of `old` reappears. Pure insertions and deletions give an empty range; text appended at the end
/// isn't counted.
fn changed(old: &str, new: &str) -> std::ops::Range<usize> {
    let (old, new): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let start = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let (old, rest) = (&old[start..], &new[start..]);
    if old.is_empty() {
        return start..start;
    }
    let end = (0..old.len())
        .find_map(|j| rest.windows(old.len() - j).position(|w| w == &old[j..]))
        .unwrap_or(rest.len());
    start..start + end
}

/// `text` wrapped at `TEXT_MAX_W`: its width, and the byte offset each wrapped row starts at.
struct Fit {
    w: f32,
    starts: Vec<usize>,
}

fn fit(window: &Window, text: &SharedString) -> Fit {
    let run = TextRun {
        len: text.len(),
        font: mav::font(),
        color: mav::hex(WHITE),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let Ok(lines) = window.text_system().shape_text(
        text.clone(),
        px(TEXT_SIZE),
        &[run],
        Some(px(TEXT_MAX_W)),
        None,
    ) else {
        return Fit {
            w: TEXT_MAX_W,
            starts: vec![0],
        };
    };
    let w = lines
        .iter()
        .map(|l| f32::from(l.width()))
        .fold(0., f32::max);
    // A wrap boundary names the glyph that starts the next row; hard line breaks start rows too.
    let mut starts = vec![];
    let mut at = 0;
    for l in lines.iter() {
        starts.push(at);
        let runs = &l.unwrapped_layout.runs;
        starts.extend(
            l.wrap_boundaries()
                .iter()
                .map(|b| at + runs[b.run_ix].glyphs[b.glyph_ix].index),
        );
        at += l.len() + 1;
    }
    // A pixel of slack so the laid-out text never wraps where the measurement didn't.
    Fit {
        w: (w.ceil() + 1.).min(TEXT_MAX_W),
        starts,
    }
}

/// Opens the HUD hidden, bottom center of the primary display (macOS re-anchors on each show). Show and hide go through `apply`/`hide`, which
/// order the native window without activating the app.
pub fn open(cx: &mut App) -> Result<WindowHandle<Hud>> {
    let display = cx.primary_display().map(|d| d.visible_bounds());
    let origin = display.map_or(point(px(200.), px(200.)), |b| {
        point(
            b.origin.x + (b.size.width - px(W)) / 2.,
            b.origin.y + b.size.height - px(LIFT - PAD_BOTTOM + H),
        )
    });
    let handle = cx.open_window(options(origin), |_, cx| cx.new(|_| Hud::new()))?;
    handle.update(cx, |_, window, _| native::configure(window))?;
    Ok(handle)
}

pub fn options(origin: Point<Pixels>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin,
            size: size(px(W), px(H)),
        })),
        titlebar: None,
        focus: false,
        show: false,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        // The HUD is never the active window; GPUI's default caps inactive windows at 30 fps, which made the typing
        // and the edge light visibly step. It animates only while shown and moving.
        inactive_frame_interval: None,
        ..Default::default()
    }
}

pub fn hide(handle: &WindowHandle<Hud>, cx: &mut App) {
    let _ = handle.update(cx, |hud, window, cx| {
        hud.epoch += 1;
        hud.visible = false;
        hud.fade = None;
        // Draw a blank frame before ordering out: the window keeps its last frame while hidden, and the next show
        // would flash this utterance's text before redrawing.
        hud.blank = true;
        cx.notify();
        let (this, epoch) = (cx.entity(), hud.epoch);
        // Callbacks run at the start of a frame, so the nested one runs after the blank frame was presented.
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                if this.read(cx).epoch == epoch {
                    native::set_visible(window, false);
                }
            })
        });
    });
}

/// Applies one controller HUD change to the window: show, hide, schedule the auto-hide, animate the edge light.
pub fn apply(handle: &WindowHandle<Hud>, m: Msg, cx: &mut App) {
    let Ok((effect, epoch)) = handle.update(cx, |hud, window, cx| {
        let effect = hud.apply(m);
        match effect {
            Effect::Ignore | Effect::Hide => {}
            Effect::Redraw => cx.notify(),
            Effect::Show(_) => {
                hud.epoch += 1;
                hud.visible = true;
                hud.blank = false;
                if native::set_visible(window, true) {
                    // GPUI's own move/resize callbacks can't update the window from inside this update.
                    window.bounds_changed(cx);
                }
                cx.notify();
            }
        }
        (effect, hud.epoch)
    }) else {
        return;
    };
    let handle = *handle;
    match effect {
        Effect::Hide => hide(&handle, cx),
        Effect::Show(Some(secs)) => {
            cx.spawn(async move |cx| {
                let wait = |s: f32| cx.background_executor().timer(Duration::from_secs_f32(s));
                wait(secs).await;
                // Fade out, then hide, as long as nothing was shown meanwhile.
                let current = |hud: &Hud| hud.epoch == epoch;
                let fading = cx.update(|cx| {
                    handle
                        .update(cx, |hud, _, cx| {
                            let go = current(hud);
                            if go {
                                hud.fade = Some(Instant::now());
                                cx.notify();
                            }
                            go
                        })
                        .unwrap_or(false)
                });
                if !fading {
                    return;
                }
                wait(FADE_S).await;
                cx.update(|cx| {
                    if handle.read_with(cx, |hud, _| current(hud)).unwrap_or(false) {
                        hide(&handle, cx);
                    }
                });
            })
            .detach();
        }
        _ => {}
    }
}

#[cfg(target_os = "macos")]
pub mod native {
    use gpui_kit::Window;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSView, NSWindow, NSWindowCollectionBehavior, NSWorkspace};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    /// The NSView GPUI created for this window (it handles the backing-scale callbacks, not the content view).
    fn gpui_view(window: &Window) -> Option<&NSView> {
        let RawWindowHandle::AppKit(h) = HasWindowHandle::window_handle(window).ok()?.as_raw()
        else {
            return None;
        };
        // SAFETY: the handle points at the live NSView GPUI created for this window; we're on the main thread.
        Some(unsafe { h.ns_view.cast().as_ref() })
    }

    fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
        gpui_view(window)?.window()
    }

    pub fn configure(window: &Window) {
        let Some(w) = ns_window(window) else { return };
        w.setIgnoresMouseEvents(true);
        w.setHasShadow(false);
        w.setHidesOnDeactivate(false);
        w.setCollectionBehavior(
            w.collectionBehavior()
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::Stationary,
        );
    }

    /// Returns whether the window moved (to the screen the user is on), in which case GPUI must re-read its bounds
    /// and scale.
    pub fn set_visible(window: &Window, visible: bool) -> bool {
        let Some(w) = ns_window(window) else {
            return false;
        };
        if visible {
            let appearing = !w.isVisible();
            // Unlike orderFront:, this works while another app is active and never makes the panel key.
            w.orderFrontRegardless();
            // Same run loop turn as the order-in, so it never shows at the old spot.
            if appearing && let Some(v) = gpui_view(window) {
                anchor(&w, v);
                return true;
            }
        } else {
            w.orderOut(None);
        }
        false
    }

    /// Bottom center of the screen with the key window (where the user is typing), set each time the HUD appears.
    /// Cocoa coordinates: origin at the bottom left of the main screen, y up.
    fn anchor(w: &NSWindow, view: &NSView) {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        let Some(screen) = objc2_app_kit::NSScreen::mainScreen(mtm) else {
            return;
        };
        let f = screen.visibleFrame();
        let x = f.origin.x + (f.size.width - super::W as f64) / 2.;
        let y = f.origin.y + (super::LIFT - super::PAD_BOTTOM) as f64;
        w.setFrameOrigin(objc2_foundation::NSPoint::new(x.round(), y.round()));
        // Moving onto a screen with another scale: the layer's contents scale and drawable size follow on this call
        // (GPUI's view handles it); the caller then has GPUI's Window re-read the scale.
        view.viewDidChangeBackingProperties();
    }

    /// Main-thread facts for the activation test.
    #[derive(Debug)]
    pub struct Probe {
        pub frontmost: (i32, String),
        pub app_active: bool,
        pub hud_key: bool,
        pub hud_visible: bool,
    }

    pub fn probe(window: &Window) -> Option<Probe> {
        let mtm = objc2::MainThreadMarker::new()?;
        let front = NSWorkspace::sharedWorkspace().frontmostApplication();
        let frontmost = front.map_or((-1, String::new()), |a| {
            (
                a.processIdentifier(),
                a.localizedName().map(|n| n.to_string()).unwrap_or_default(),
            )
        });
        let w = ns_window(window)?;
        Some(Probe {
            frontmost,
            app_active: NSApplication::sharedApplication(mtm).isActive(),
            hud_key: w.isKeyWindow(),
            hud_visible: w.isVisible(),
        })
    }
}

#[cfg(target_os = "windows")]
pub mod native {
    use gpui_kit::Window;

    pub fn configure(_: &Window) {}
    pub fn set_visible(_: &Window, _: bool) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{Effect, Hud, Msg, State, level};

    #[test]
    fn transitions() {
        let mut h = Hud::new();
        assert_eq!(h.apply(Msg::Listen), Effect::Show(None));
        assert_eq!(h.apply(Msg::Unquiet), Effect::Ignore);
        assert_eq!(h.apply(Msg::Level(3000.)), Effect::Redraw);
        assert_eq!(h.level, 1.);
        h.apply(Msg::Level(60.));
        assert!((h.level - 0.6).abs() < 1e-6, "decays, not drops");
        h.apply(Msg::Quiet("low".into()));
        assert_eq!(h.state, State::Quiet);
        assert_eq!(h.apply(Msg::Unquiet), Effect::Show(None));
        assert_eq!(h.state, State::Listen);
        h.apply(Msg::Text {
            text: "Plaid 天气".into(),
            command: true,
        });
        h.apply(Msg::Finishing);
        assert!(h.command && h.text == "Plaid 天气");
        assert_eq!(h.apply(Msg::Level(100.)), Effect::Ignore);
        h.apply(Msg::Running("天气".into()));
        assert!(!h.command);
        assert_eq!(h.apply(Msg::Done("天气".into())), Effect::Show(Some(0.7)));
        assert!(h.ran);
        h.apply(Msg::Listen);
        assert_eq!(h.apply(Msg::Done("text".into())), Effect::Show(Some(0.7)));
        assert!(!h.ran);
        assert_eq!(h.apply(Msg::Failed("x".into())), Effect::Show(Some(5.)));
        assert_eq!(h.apply(Msg::Hide), Effect::Hide);
    }

    /// The panel keeps the size the utterance reached through finishing, refining and the paste; a new utterance
    /// or an unrelated message starts at one row (render measures and grows it).
    #[test]
    fn size_only_grows_within_an_utterance() {
        let mut h = Hud::new();
        h.apply(Msg::Listen);
        h.rows = 3;
        h.text_w = 400.;
        h.apply(Msg::Text {
            text: "短".into(),
            command: false,
        });
        h.apply(Msg::Finishing);
        h.apply(Msg::Refining("短".into()));
        h.apply(Msg::Done("短".into()));
        assert_eq!((h.rows, h.text_w), (3, 400.));
        h.apply(Msg::Flash("学会".into()));
        assert_eq!((h.rows, h.text_w), (1, 0.));
        h.rows = 2;
        h.apply(Msg::Listen);
        assert_eq!(h.rows, 1);
    }

    #[test]
    fn level_scale() {
        assert_eq!(level(0.), 0.);
        assert_eq!(level(60.), 0.);
        assert_eq!(level(10_000.), 1.);
        assert!((level(424.26) - 0.5).abs() < 0.01);
    }

    #[test]
    fn edge_eases_and_resets() {
        let mut h = Hud::new();
        h.apply(Msg::Listen);
        h.apply(Msg::Level(3000.));
        h.frame(1.);
        assert!(h.shown > 0.2 && h.shown < 0.4, "{}", h.shown);
        // Six frames (~100 ms, one audio buffer) close most of the gap.
        for _ in 0..5 {
            h.frame(1.);
        }
        assert!(h.shown > 0.85, "{}", h.shown);
        h.apply(Msg::Listen);
        assert_eq!(h.shown, 0.);
    }

    #[test]
    fn state_change_morphs_from_what_was_shown() {
        let mut h = Hud::new();
        h.visible = true;
        h.apply(Msg::Listen);
        h.current();
        h.apply(Msg::Text {
            text: "hi".into(),
            command: false,
        });
        assert!(
            h.morph.is_none(),
            "new text in the same state doesn't morph"
        );
        h.apply(Msg::Finishing);
        let sweep = h.current();
        h.apply(Msg::Done("hi".into()));
        let start = h.current();
        assert!((start.w - sweep.w).abs() < 0.05, "{start:?}");
        h.morph = Some(super::Instant::now() - super::Duration::from_secs_f32(super::MORPH_S));
        let end = h.current();
        assert_eq!(
            (end.w, end.rgb),
            (1., super::rgba(super::GREEN << 8 | 0xff))
        );
        assert!(h.morph.is_none());
        // Hidden: a fresh show starts at its target.
        h.visible = false;
        h.apply(Msg::Listen);
        assert!(h.morph.is_none());
    }

    #[test]
    fn new_text_types_on() {
        let text = |t: &str| Msg::Text {
            text: t.into(),
            command: false,
        };
        let mut h = Hud::new();
        h.visible = true;
        h.apply(Msg::Listen);
        h.apply(text("hello"));
        // Laid out whole, all hidden; the first frame starts fading in "h".
        assert_eq!(h.fades()[0].0, 0..5);
        h.frame(1.);
        let fades = h.fades();
        assert_eq!(
            fades.iter().map(|f| f.0.clone()).collect::<Vec<_>>(),
            [0..1, 1..5]
        );
        assert!(fades[0].1.fade_out.unwrap() < 1.);
        let mut frames = 1;
        while h.typing() {
            h.frame(1.);
            frames += 1;
        }
        // Paced to speech: five characters take about a second and a half while listening.
        assert!((40..=150).contains(&frames), "{frames}");
        assert!(h.fades().is_empty());
        // A correction swaps in place: what was shown stays shown, the rest types on.
        h.apply(text("help me"));
        assert_eq!(h.reveal, 5.);
        // "lo" became "p me": all of it counts as replaced (nothing of the old tail reappears).
        assert_eq!(h.refade.as_ref().map(|r| r.0.clone()), Some(3..7));
        assert!(h.fades().iter().any(|f| f.0 == (3..4)));
        h.apply(text("yes"));
        assert!(!h.typing());
        // Not on screen: appears whole.
        h.visible = false;
        h.apply(text("other"));
        assert!(!h.typing());
    }

    #[test]
    fn changed_spans_only_the_rewrite() {
        use super::changed;
        let at = |o: &str, n: &str| changed(o, n);
        // A word recognized again: just those characters, the rest stays put.
        assert_eq!(at("文字出现的这种感觉", "文字粗心的这种感觉"), 2..4);
        // Punctuation swapped while more text arrives.
        assert_eq!(at("流利的，舒服", "流利的、舒服的感觉"), 3..4);
        // A filler removed, or text appended: nothing to fade again.
        assert_eq!(at("效果，嗯，目前", "效果，目前").len(), 0);
        assert_eq!(at("效果", "效果还是"), 2..2);
    }
}
