// Menu bar / notification area icon. tray-icon must be created on the main thread once the platform
// event loop runs, which is the case inside GPUI's `run` callback. Menu clicks arrive on muda's handler
// and are forwarded to the GPUI foreground executor through a channel.
use gpui_kit::*;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Overview,
    History,
    Settings,
    Update,
    Quit,
}

struct Tray {
    _icon: TrayIcon,
    menu: Menu,
    /// "重启以更新到 x" / "下载新版本 x…" and its separator, at the top while an update is offered.
    update: MenuItem,
    update_sep: PredefinedMenuItem,
    shown: bool,
}
impl Global for Tray {}

/// Shows or hides the update item. `ready`: the update is staged and applying it restarts Plaid.
pub fn offer_update(version: Option<&str>, ready: bool, cx: &mut App) {
    if !cx.has_global::<Tray>() {
        return;
    }
    let t = cx.global_mut::<Tray>();
    match version {
        Some(v) => {
            t.update.set_text(if ready {
                format!("重启以更新到 {v}")
            } else {
                format!("下载新版本 {v}…")
            });
            if !t.shown {
                let _ = t.menu.insert_items(&[&t.update, &t.update_sep], 0);
                t.shown = true;
            }
        }
        None if t.shown => {
            let _ = t.menu.remove(&t.update);
            let _ = t.menu.remove(&t.update_sep);
            t.shown = false;
        }
        None => {}
    }
}

pub fn install(cx: &mut App, on_command: impl Fn(Command, &mut App) + 'static) -> Result<()> {
    let items = [
        ("overview", "概览…", Command::Overview),
        ("history", "历史…", Command::History),
        ("settings", "设置…", Command::Settings),
        ("quit", "退出", Command::Quit),
        ("update", "", Command::Update),
    ];
    let menu = Menu::new();
    for (i, (id, title, _)) in items[..4].iter().enumerate() {
        if i == 3 {
            menu.append(&PredefinedMenuItem::separator())?;
        }
        menu.append(&MenuItem::with_id(*id, *title, true, None))?;
    }
    // macOS tints the template image itself. Windows shows it as is: white on a dark taskbar.
    #[cfg(windows)]
    let ink = if crate::win::light_taskbar() { 0 } else { 255 };
    #[cfg(not(windows))]
    let ink = 0;
    let glyph = Icon::from_rgba(grid_glyph(ink), GLYPH, GLYPH)?;
    #[cfg(target_os = "macos")]
    let builder = TrayIconBuilder::new().with_icon_templated(glyph);
    #[cfg(not(target_os = "macos"))]
    let builder = TrayIconBuilder::new().with_icon(glyph);
    let icon = builder
        .with_tooltip("Plaid")
        .with_menu(Box::new(menu.clone()))
        .build()?;
    cx.set_global(Tray {
        _icon: icon,
        menu,
        update: MenuItem::with_id("update", "", true, None),
        update_sep: PredefinedMenuItem::separator(),
        shown: false,
    });

    let (tx, rx) = async_channel::unbounded();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if let Some(c) = items.iter().find(|i| e.id == i.0).map(|i| i.2) {
            let _ = tx.send_blocking(c);
        }
    }));
    cx.spawn(async move |cx| {
        while let Ok(c) = rx.recv().await {
            cx.update(|cx| on_command(c, cx));
        }
    })
    .detach();
    Ok(())
}

const GLYPH: u32 = 36; // 18 pt at 2x

/// The app icon's `#` as an alpha mask in gray level `ink` (black is the template image on macOS): two
/// vertical and two horizontal rounded strokes.
fn grid_glyph(ink: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity((GLYPH * GLYPH * 4) as usize);
    // Distance to the stroke at `at` (across), spanning 7..29 along it, minus its radius.
    let bar = |along: f32, across: f32, at: f32| {
        let d = along - along.clamp(7., 29.);
        (d.powi(2) + (across - at).powi(2)).sqrt() - 1.7
    };
    for py in 0..GLYPH {
        for px in 0..GLYPH {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let d = [13.5, 22.5]
                .iter()
                .map(|&at| bar(y, x, at).min(bar(x, y, at)))
                .fold(f32::MAX, f32::min);
            let a = (0.5 - d).clamp(0., 1.);
            out.extend_from_slice(&[ink, ink, ink, (a * 255.) as u8]);
        }
    }
    out
}
