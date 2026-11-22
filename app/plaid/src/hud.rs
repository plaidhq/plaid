// Recording HUD: a small always-on-top window that never takes focus and lets the mouse through.
// GPUI's PopUp kind gives a non-activating NSPanel at NSPopUpWindowLevel on macOS and a
// WS_EX_TOOLWINDOW | WS_EX_TOPMOST window on Windows; the rest is patched through the native handle.
use crate::ui::mav::{self, Status};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::Hud as Msg;
use std::time::Instant;

/// The panel per the design; the window adds room around it for the drop shadow. The panel sits at the bottom
/// center of the window and fits the text: it widens up to `PANEL_MAX_W`, then grows upward up to `MAX_LINES`.
/// The window is sized for the largest panel.
const PANEL_MIN_W: f32 = 320.;
const PANEL_MAX_W: f32 = 560.;
const PANEL_PAD_X: f32 = 14.;
const PANEL_PAD_Y: f32 = 10.;
/// Status dot, label, elapsed time and hints above the text.
const HEADER_H: f32 = 16.;
const HEADER_GAP: f32 = 2.;
const TEXT_SIZE: f32 = 14.;
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
    /// Smoothed input level in 0...1: rises with the input, decays over a few buffers.
    level: f32,
    started: Option<Instant>,
    /// Text width and rows the panel shows; both only grow during an utterance so a rewrite of the partial doesn't
    /// make the panel jump.
    text_w: f32,
    rows: usize,
    spin: usize,
    visible: bool,
    /// Bumped on every shown change; a pending auto-hide only fires if nothing changed since.
    epoch: u64,
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
            started: None,
            text_w: 0.,
            rows: 1,
            spin: 0,
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
                self.started = Some(Instant::now());
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

    fn spins(&self) -> bool {
        self.visible && matches!(self.state, State::Finishing | State::Running)
    }

    fn elapsed(&self) -> Option<String> {
        let s = self.started?.elapsed().as_secs();
        Some(format!("{}:{:02}", s / 60, s % 60))
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

/// Box the header's status indicator sits in.
const DOT_BOX: f32 = 12.;

/// Live input dot: grows from 6 to 10 px and brightens with the level.
fn pulse(level: f32, color: u32) -> Div {
    let alpha = (0x8c as f32 + level * 0x73 as f32) as u32;
    div()
        .size(px(6. + 4. * level))
        .rounded_full()
        .bg(mav::hexa(color << 8 | alpha))
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
        let glyph = |path, color| mav::icon(path, 12., mav::hex(color)).into_any_element();
        let mark = |s| mav::dot(s, 8.).into_any_element();
        let (dot, label, label_color, hints, meta): (AnyElement, &str, u32, Vec<Hint>, bool) =
            match self.state {
                State::Listen => (
                    pulse(self.level, if self.command { CYAN } else { WHITE }).into_any_element(),
                    match (self.command, self.continuous) {
                        (true, _) => "命令",
                        (false, true) => "连续听写",
                        (false, false) => "",
                    },
                    0,
                    if self.continuous {
                        vec![Key("⏎"), Text("结束")]
                    } else {
                        vec![]
                    },
                    true,
                ),
                State::Quiet => (
                    // Low input by definition, so a steady dot rather than one that tracks the level.
                    div()
                        .size(px(7.))
                        .rounded_full()
                        .bg(mav::hex(AMBER))
                        .into_any_element(),
                    "电平过低",
                    AMBER,
                    vec![],
                    true,
                ),
                State::Finishing | State::Running => (
                    mav::spinner(11., WHITE, self.spin).into_any_element(),
                    if self.state == State::Finishing {
                        "收尾"
                    } else {
                        "处理中"
                    },
                    0,
                    vec![],
                    false,
                ),
                State::Refining => (
                    glyph("plaid/refine.svg", WHITE),
                    "优化中",
                    0,
                    vec![Key("esc"), Text("用原文")],
                    false,
                ),
                State::Done => (
                    mark(Status::Ok),
                    if self.ran { "已执行" } else { "已粘贴" },
                    0,
                    vec![],
                    false,
                ),
                State::Note => (mark(Status::Ok), "完成", 0, vec![], false),
                State::Saved => (
                    mark(Status::Warn),
                    "已保存，未粘贴",
                    0,
                    vec![Text("在历史中查看")],
                    false,
                ),
                State::Flash => (mark(Status::Warn), "提示", 0, vec![], false),
                State::Failed => (mark(Status::Err), "失败", 0, vec![], false),
                State::Ask => (
                    glyph("plaid/commands.svg", WHITE),
                    "执行命令？",
                    0,
                    vec![Key("⏎"), Text("执行"), Key("esc"), Text("取消")],
                    false,
                ),
            };
        let live = matches!(self.state, State::Listen | State::Finishing);
        let dim = matches!(self.state, State::Finishing | State::Refining);
        let (text, placeholder) = if self.text.is_empty() {
            let p = match self.state {
                State::Listen if self.continuous => "再按一次快捷键或 ⏎ 结束",
                State::Listen => "请说话…",
                _ => "等待最终结果…",
            };
            (SharedString::from(p), true)
        } else {
            (self.text.clone(), false)
        };
        let text_color = if self.command && live && !dim {
            mav::hex(CYAN)
        } else if dim || placeholder {
            mav::hexa(0xffffff9e)
        } else {
            mav::hex(WHITE)
        };
        let (w, rows) = fit(window, &text);
        self.text_w = self.text_w.max(w);
        if !placeholder {
            self.rows = self.rows.max(rows);
        }
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
                    .w(px((self.text_w + CHROME_W).clamp(PANEL_MIN_W, PANEL_MAX_W)))
                    .px(px(PANEL_PAD_X))
                    .py(px(PANEL_PAD_Y))
                    .flex()
                    .flex_col()
                    .gap(px(HEADER_GAP))
                    .rounded(px(9.))
                    .bg(mav::hexa(0x181818d1))
                    .shadow(vec![
                        mav::shadow(0., 0., 0., 1., 0x0000008c, false),
                        mav::shadow(0., 1., 0., 0., 0xffffff24, true),
                        mav::shadow(0., 4., 12., 0., 0x00000033, false),
                    ])
                    .font(mav::font())
                    .text_color(mav::hex(WHITE))
                    .child(
                        div()
                            .h(px(HEADER_H))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(11.))
                            .child(
                                div()
                                    .size(px(DOT_BOX))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(dot),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(10.))
                                    .font_weight(mav::bold())
                                    .text_color(if label_color == 0 {
                                        mav::hexa(0xffffff94)
                                    } else {
                                        mav::hex(label_color)
                                    })
                                    .child(SharedString::from(label)),
                            )
                            .when_some(meta.then(|| self.elapsed()).flatten(), |d, m| {
                                d.child(div().mr(px(4.)).text_color(mav::hexa(0xffffff8c)).child(m))
                            })
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
                    .child(line.child(text)),
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

/// Applies one controller HUD change to the window: show, hide, schedule the auto-hide, run the spinner.
pub fn apply(handle: &WindowHandle<Hud>, m: Msg, cx: &mut App) {
    let Ok((effect, spin_started, epoch)) = handle.update(cx, |hud, window, cx| {
        let was_spinning = hud.spins();
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
        (effect, !was_spinning && hud.spins(), hud.epoch)
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
    if spin_started {
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(83))
                    .await;
                let go = cx.update(|cx| {
                    handle
                        .update(cx, |hud, _, cx| {
                            let go = hud.spins();
                            if go {
                                hud.spin += 1;
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
}
