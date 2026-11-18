// Recording HUD: a small always-on-top window that never takes focus and lets the mouse through.
// GPUI's PopUp kind gives a non-activating NSPanel at NSPopUpWindowLevel on macOS and a
// WS_EX_TOOLWINDOW | WS_EX_TOPMOST window on Windows; the rest is patched through the native handle.
use gpui_kit::*;

const W: f32 = 420.;
const H: f32 = 56.;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Listening,
    Finishing,
    Done,
    Error,
}

pub struct Hud {
    phase: Phase,
    text: SharedString,
    /// The text is a low-input note that the next loud buffer clears.
    quiet: bool,
    /// Bumped on every change; a pending auto-hide only fires if nothing changed since.
    epoch: u64,
}

impl Hud {
    pub fn set(&mut self, phase: Phase, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.phase = phase;
        self.text = text.into();
        cx.notify();
    }
}

impl Render for Hud {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let dot = match self.phase {
            Phase::Listening => rgb(0xff453a),
            Phase::Finishing => rgb(0xffd60a),
            Phase::Done => rgb(0x30d158),
            Phase::Error => rgb(0xff9f0a),
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .max_w_full()
                    .rounded_full()
                    .bg(rgba(0x1c1c1ee6))
                    .text_color(rgb(0xf5f5f7))
                    .text_sm()
                    .child(div().size_2().flex_none().rounded_full().bg(dot))
                    .child(div().truncate().child(self.text.clone())),
            )
    }
}

/// Opens the HUD hidden. Show and hide go through `show`/`hide`, which order the native window
/// without activating the app.
pub fn open(cx: &mut App) -> Result<WindowHandle<Hud>> {
    let display = cx.primary_display().map(|d| d.bounds());
    let origin = display.map_or(point(px(200.), px(200.)), |b| {
        point(
            b.origin.x + (b.size.width - px(W)) / 2.,
            b.origin.y + b.size.height - px(H + 96.),
        )
    });
    let opts = WindowOptions {
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
    };
    let handle = cx.open_window(opts, |_, cx| {
        cx.new(|_| Hud {
            phase: Phase::Listening,
            text: "".into(),
            quiet: false,
            epoch: 0,
        })
    })?;
    handle.update(cx, |_, window, _| native::configure(window))?;
    Ok(handle)
}

/// Only the HUD test sets a phase directly; the dictation loop goes through `apply`.
#[cfg(target_os = "macos")]
pub fn show(handle: &WindowHandle<Hud>, phase: Phase, text: impl Into<SharedString>, cx: &mut App) {
    let text = text.into();
    let _ = handle.update(cx, |hud, window, cx| {
        hud.set(phase, text, cx);
        native::set_visible(window, true);
    });
}

pub fn hide(handle: &WindowHandle<Hud>, cx: &mut App) {
    let _ = handle.update(cx, |hud, window, _| {
        hud.epoch += 1;
        native::set_visible(window, false);
    });
}

const LISTENING: &str = "正在听…";

/// Applies one controller HUD change. Placeholder presentation until the HUD is redesigned.
pub fn apply(handle: &WindowHandle<Hud>, h: plaid_core::dictation::Hud, cx: &mut App) {
    use plaid_core::dictation::Hud as H;
    let Ok((text, quiet)) = handle.read_with(cx, |hud, _| (hud.text.clone(), hud.quiet)) else {
        return;
    };
    let (next, hide_after): ((Phase, SharedString), Option<f32>) = match h {
        H::Level(_) => return,
        H::Hide => return hide(handle, cx),
        H::Unquiet if !quiet => return,
        H::Listen | H::Unquiet => ((Phase::Listening, LISTENING.into()), None),
        H::Quiet(s) => ((Phase::Error, s.into()), None),
        H::Text { text, command } => (
            (
                Phase::Listening,
                if command {
                    format!("命令 · {text}")
                } else {
                    text
                }
                .into(),
            ),
            None,
        ),
        H::Continuous => (
            (
                Phase::Listening,
                if text == LISTENING {
                    "连续听写 · 再按一次或 ⏎ 结束".into()
                } else {
                    text
                },
            ),
            None,
        ),
        H::Finishing => ((Phase::Finishing, text), None),
        H::Refining(t) => ((Phase::Finishing, t.into()), None),
        H::Done(t) => ((Phase::Done, t.into()), Some(0.5)),
        H::Saved(s) => ((Phase::Error, s.into()), Some(8.)),
        H::Flash(s) => ((Phase::Error, s.into()), Some(5.)),
        H::Ask(s) => ((Phase::Finishing, s.into()), Some(8.)),
        H::Running(s) => ((Phase::Finishing, s.into()), None),
        H::Note(s) => ((Phase::Done, s.into()), Some(4.)),
    };
    let quiet = matches!(next.0, Phase::Error) && hide_after.is_none();
    let mut epoch = 0;
    let _ = handle.update(cx, |hud, window, cx| {
        hud.quiet = quiet;
        hud.epoch += 1;
        epoch = hud.epoch;
        hud.set(next.0, next.1, cx);
        native::set_visible(window, true);
    });
    if let Some(secs) = hide_after {
        let handle = *handle;
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs_f32(secs))
                .await;
            let _ = cx.update(|cx| {
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
}

#[cfg(target_os = "macos")]
pub mod native {
    use gpui_kit::Window;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSView, NSWindow, NSWindowCollectionBehavior, NSWorkspace};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
        let RawWindowHandle::AppKit(h) = HasWindowHandle::window_handle(window).ok()?.as_raw()
        else {
            return None;
        };
        // SAFETY: the handle points at the live NSView GPUI created for this window; we're on the main thread.
        let view: &NSView = unsafe { h.ns_view.cast().as_ref() };
        view.window()
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

    pub fn set_visible(window: &Window, visible: bool) {
        let Some(w) = ns_window(window) else { return };
        if visible {
            // Unlike orderFront:, this works while another app is active and never makes the panel key.
            w.orderFrontRegardless();
        } else {
            w.orderOut(None);
        }
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
    pub fn set_visible(_: &Window, _: bool) {}
}
