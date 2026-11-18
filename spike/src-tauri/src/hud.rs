// Non-activating HUD: an NSPanel that never becomes key, ignores the mouse and floats over fullscreen apps.
use tauri::{AppHandle, Emitter, LogicalPosition, Position, WebviewUrl};
use tauri_nspanel::{CollectionBehavior, ManagerExt, PanelBuilder, PanelLevel, StyleMask, tauri_panel};

tauri_panel! {
    panel!(HudPanel {
        config: {
            can_become_key_window: false,
            can_become_main_window: false,
            is_floating_panel: true
        }
    })
}

pub const LABEL: &str = "hud";
const W: f64 = 420.0;
const H: f64 = 56.0;

/// Create the panel hidden. The window is built with `visible(false)` and converted before the first show,
/// so it never exists as an activating NSWindow on screen.
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let (x, y) = match app.primary_monitor()? {
        Some(m) => {
            let s = m.scale_factor();
            let (w, h) = (m.size().width as f64 / s, m.size().height as f64 / s);
            ((w - W) / 2.0, h - H - 96.0)
        }
        None => (200.0, 200.0),
    };
    PanelBuilder::<_, HudPanel>::new(app, LABEL)
        .url(WebviewUrl::App("index.html".into()))
        .size(tauri::Size::Logical(tauri::LogicalSize::new(W, H)))
        .position(Position::Logical(LogicalPosition::new(x, y)))
        .no_activate(true)
        .transparent(true)
        .has_shadow(false)
        .ignores_mouse_events(true)
        .hides_on_deactivate(false)
        .level(PanelLevel::Status)
        .style_mask(StyleMask::empty().nonactivating_panel())
        .collection_behavior(CollectionBehavior::new().can_join_all_spaces().full_screen_auxiliary().ignores_cycle())
        .with_window(|w| w.visible(false).focused(false).decorations(false).transparent(true).shadow(false).skip_taskbar(true).resizable(false))
        .build()?;
    Ok(())
}

/// state: "listening" | "finishing" | "error"
pub fn show(app: &AppHandle, state: &str, text: &str) {
    let _ = app.emit_to(LABEL, "hud", serde_json::json!({ "state": state, "text": text }));
    let h = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Ok(p) = h.get_webview_panel(LABEL) {
            if !p.is_visible() {
                p.show(); // orderFrontRegardless
            }
        }
    });
}

pub fn hide(app: &AppHandle) {
    let h = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Ok(p) = h.get_webview_panel(LABEL) {
            p.hide();
        }
    });
}

/// Main-thread facts for the activation test.
#[derive(Debug)]
pub struct Probe {
    pub frontmost: (i32, String),
    pub app_active: bool,
    pub panel_key: bool,
    pub panel_visible: bool,
}

pub fn probe(app: &AppHandle) -> Option<Probe> {
    use objc2_app_kit::{NSApplication, NSWorkspace};
    let mtm = objc2::MainThreadMarker::new()?;
    let front = NSWorkspace::sharedWorkspace().frontmostApplication();
    let frontmost = front.map_or((-1, String::new()), |a| (a.processIdentifier(), a.localizedName().map(|n| n.to_string()).unwrap_or_default()));
    let p = app.get_webview_panel(LABEL).ok()?;
    let ns = p.as_panel();
    Some(Probe {
        frontmost,
        app_active: NSApplication::sharedApplication(mtm).isActive(),
        panel_key: ns.isKeyWindow(),
        panel_visible: ns.isVisible(),
    })
}

/// Run `f` on the main thread and wait for its result.
pub fn on_main<T: Send + 'static>(app: &AppHandle, f: impl FnOnce(&AppHandle) -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    let h = app.clone();
    app.run_on_main_thread(move || {
        let _ = tx.send(f(&h));
    })
    .expect("main thread");
    rx.recv().expect("main thread result")
}
