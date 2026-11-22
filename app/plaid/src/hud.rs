// Recording HUD: a small always-on-top window that never takes focus and lets the mouse through.
// GPUI's PopUp kind gives a non-activating NSPanel at NSPopUpWindowLevel on macOS and a
// WS_EX_TOOLWINDOW | WS_EX_TOPMOST window on Windows; the rest is patched through the native handle.
use crate::ui::mav;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::Hud as Msg;
use std::time::Instant;

/// The panel per the design; the window adds room around it for the drop shadow. The panel sits at the bottom
/// center of the window and fits the text: it widens up to `PANEL_MAX_W`, then grows upward up to `MAX_LINES`.
/// The window is sized for the largest panel. The panel holds only the text; the light along its bottom edge carries
/// the input level and the state.
const PANEL_MIN_W: f32 = 280.;
const PANEL_MAX_W: f32 = 560.;
const PANEL_PAD_X: f32 = 16.;
const PANEL_PAD_Y: f32 = 11.;
const RADIUS: f32 = 12.;
/// Label and key hints above the text, only for states that ask something of the user.
const HEADER_H: f32 = 16.;
const HEADER_GAP: f32 = 4.;
const TEXT_SIZE: f32 = 15.;
const LINE_H: f32 = 21.;
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
/// Animation frame interval while the edge moves.
const FRAME_MS: u64 = 16;

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
    /// Last measured text, so animation frames don't reshape it.
    fit: Option<(SharedString, (f32, usize))>,
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
            fit: None,
            visible: false,
            epoch: 0,
        }
    }

    /// Updates the state for one controller message.
    pub fn apply(&mut self, m: Msg) -> Effect {
        let prev = self.state;
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
                self.with(t, State::Done, Some(0.5))
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
        self.state = state;
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

    /// The edge moves on its own: eases toward the level, or sweeps.
    fn animates(&self) -> bool {
        self.visible
            && matches!(
                self.state,
                State::Listen | State::Quiet | State::Finishing | State::Running | State::Refining
            )
    }

    /// One animation frame: about 60 ms to close most of the gap to the latest level.
    /// False when nothing on screen would change, so a steady level costs no redraw.
    pub(crate) fn frame(&mut self) -> bool {
        let gap = self.level - self.shown;
        if gap.abs() < 0.002 {
            self.shown = self.level;
            return !matches!(self.edge(), Edge::Fixed(..) | Edge::Level(..));
        }
        self.shown += gap * 0.3;
        true
    }

    fn edge(&self) -> Edge {
        match self.state {
            State::Listen => Edge::Level(if self.command { CYAN } else { WHITE }, self.shown),
            State::Quiet => Edge::Fixed(AMBER, 0.3),
            State::Finishing | State::Running | State::Refining => Edge::Sweep(WHITE),
            State::Done | State::Note => Edge::Fixed(GREEN, 1.),
            State::Saved | State::Flash => Edge::Fixed(AMBER, 1.),
            State::Failed => Edge::Fixed(RED, 1.),
            State::Ask => Edge::Fixed(CYAN, 1.),
        }
    }
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

/// A light segment `w` wide starting at `x`, fading out toward both ends, with a soft glow from its middle.
fn light(x: f32, w: f32, color: u32, alpha: f32) -> Div {
    let a = (alpha.clamp(0., 1.) * 255.) as u32;
    let half = |from: u32, to: u32| {
        div().w(px(w / 2.)).h_full().bg(linear_gradient(
            90.,
            linear_color_stop(mav::hexa(color << 8 | from), 0.),
            linear_color_stop(mav::hexa(color << 8 | to), 1.),
        ))
    };
    div()
        .absolute()
        .bottom_0()
        .left(px(x))
        .w(px(w))
        .h(px(EDGE_H))
        .flex()
        .child(half(0, a))
        .child(half(a, 0))
        .child(
            // Glow: a short core whose blurred shadow lights the panel edge around the middle.
            div()
                .absolute()
                .bottom_0()
                .left(px(w * 0.35))
                .w(px(w * 0.3))
                .h(px(EDGE_H))
                .shadow(vec![mav::shadow(
                    0.,
                    0.,
                    10.,
                    0.,
                    color << 8 | (a as f32 * 0.6) as u32,
                    false,
                )]),
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
        // Only states that ask something of the user get a header; everything else is the text alone.
        let header: Option<(&str, Vec<Hint>)> = match self.state {
            State::Refining => Some(("优化中", vec![Key("esc"), Text("用原文")])),
            State::Saved => Some(("未粘贴", vec![Text("在历史中查看")])),
            State::Failed => Some(("失败", vec![])),
            State::Ask => Some((
                "执行命令？",
                vec![Key("⏎"), Text("执行"), Key("esc"), Text("取消")],
            )),
            _ => None,
        };
        let live = matches!(self.state, State::Listen | State::Finishing);
        let dim = matches!(
            self.state,
            State::Finishing | State::Refining | State::Running
        );
        let (text, placeholder) = if self.text.is_empty() {
            let p = match self.state {
                State::Listen => "请说话…",
                _ => "等待最终结果…",
            };
            (SharedString::from(p), true)
        } else {
            (self.text.clone(), false)
        };
        let text_color = if placeholder {
            mav::hexa(0xffffff73)
        } else if dim {
            mav::hexa(0xffffff99)
        } else if self.state == State::Quiet {
            mav::hex(AMBER)
        } else if self.command && live {
            mav::hex(CYAN)
        } else {
            mav::hex(WHITE)
        };
        let (w, rows) = match &self.fit {
            Some((t, m)) if *t == text => *m,
            _ => {
                let m = fit(window, &text);
                self.fit = Some((text.clone(), m));
                m
            }
        };
        self.text_w = self.text_w.max(w);
        if !placeholder {
            self.rows = self.rows.max(rows);
        }
        let panel_w = (self.text_w + CHROME_W).clamp(PANEL_MIN_W, PANEL_MAX_W);
        // The light runs between the corner curves, so it never spills past the rounded edge.
        let track = panel_w - 2. * RADIUS;
        let edge = match self.edge() {
            Edge::Level(c, l) => {
                let w = track * (EDGE_MIN + (EDGE_MAX - EDGE_MIN) * l);
                light(RADIUS + (track - w) / 2., w, c, 0.35 + 0.65 * l)
            }
            Edge::Fixed(c, f) => {
                let w = track * f;
                light(RADIUS + (track - w) / 2., w, c, 0.9)
            }
            Edge::Sweep(c) => {
                let t = self.clock.elapsed().as_secs_f32() / SWEEP_S;
                // Starts centered, where the level light was, then swings side to side.
                let p = 0.5 + 0.5 * (t * std::f32::consts::TAU).sin();
                let w = track * SWEEP_W;
                light(RADIUS + (track - w) * p, w, c, 0.75)
            }
        };
        let line = div()
            .min_h(px(self.rows as f32 * LINE_H))
            .text_size(px(TEXT_SIZE))
            .line_height(px(LINE_H))
            .text_color(text_color)
            .line_clamp(MAX_LINES);
        // Live partials keep their newest words; everything else keeps its start.
        let line = if live {
            line.text_ellipsis_start()
        } else {
            line.text_ellipsis()
        };
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
                    .child(line.child(text))
                    .child(edge),
            )
    }
}

/// Width and wrapped rows (capped at `MAX_LINES`) `text` takes when wrapped at `TEXT_MAX_W`.
fn fit(window: &Window, text: &SharedString) -> (f32, usize) {
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
        return (TEXT_MAX_W, 1);
    };
    let w = lines
        .iter()
        .map(|l| f32::from(l.width()))
        .fold(0., f32::max);
    let rows: usize = lines.iter().map(|l| l.wrap_boundaries().len() + 1).sum();
    // A pixel of slack so the laid-out text never wraps where the measurement didn't.
    ((w.ceil() + 1.).min(TEXT_MAX_W), rows.clamp(1, MAX_LINES))
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
        ..Default::default()
    }
}

pub fn hide(handle: &WindowHandle<Hud>, cx: &mut App) {
    let _ = handle.update(cx, |hud, window, _| {
        hud.epoch += 1;
        hud.visible = false;
        native::set_visible(window, false);
    });
}

/// Applies one controller HUD change to the window: show, hide, schedule the auto-hide, animate the edge light.
pub fn apply(handle: &WindowHandle<Hud>, m: Msg, cx: &mut App) {
    let Ok((effect, anim_started, epoch)) = handle.update(cx, |hud, window, cx| {
        let was_animating = hud.animates();
        let effect = hud.apply(m);
        match effect {
            Effect::Ignore | Effect::Hide => {}
            Effect::Redraw => cx.notify(),
            Effect::Show(_) => {
                hud.epoch += 1;
                hud.visible = true;
                if native::set_visible(window, true) {
                    // GPUI's own move/resize callbacks can't update the window from inside this update.
                    window.bounds_changed(cx);
                }
                cx.notify();
            }
        }
        (effect, !was_animating && hud.animates(), hud.epoch)
    }) else {
        return;
    };
    let handle = *handle;
    match effect {
        Effect::Hide => hide(&handle, cx),
        Effect::Show(Some(secs)) => {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs_f32(secs))
                    .await;
                cx.update(|cx| {
                    if handle
                        .read_with(cx, |hud, _| hud.epoch == epoch)
                        .unwrap_or(false)
                    {
                        hide(&handle, cx);
                    }
                });
            })
            .detach();
        }
        _ => {}
    }
    if anim_started {
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(FRAME_MS))
                    .await;
                let go = cx.update(|cx| {
                    handle
                        .update(cx, |hud, _, cx| {
                            let go = hud.animates();
                            if go && hud.frame() {
                                cx.notify();
                            }
                            go
                        })
                        .unwrap_or(false)
                });
                if !go {
                    break;
                }
            }
        })
        .detach();
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
        assert_eq!(h.apply(Msg::Done("天气".into())), Effect::Show(Some(0.5)));
        assert!(h.ran);
        h.apply(Msg::Listen);
        assert_eq!(h.apply(Msg::Done("text".into())), Effect::Show(Some(0.5)));
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
        h.frame();
        assert!(h.shown > 0.2 && h.shown < 0.4, "{}", h.shown);
        // Six frames (~100 ms, one audio buffer) close most of the gap.
        for _ in 0..5 {
            h.frame();
        }
        assert!(h.shown > 0.85, "{}", h.shown);
        h.apply(Msg::Listen);
        assert_eq!(h.shown, 0.);
    }
}
