// Menu bar / notification area icon. tray-icon must be created on the main thread once the platform
// event loop runs, which is the case inside GPUI's `run` callback. Menu clicks arrive on muda's handler
// and are forwarded to the GPUI foreground executor through a channel.
use gpui_kit::*;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    History,
    Settings,
    Quit,
}

struct Tray(#[allow(dead_code)] TrayIcon);
impl Global for Tray {}

pub fn install(cx: &mut App, on_command: impl Fn(Command, &mut App) + 'static) -> Result<()> {
    let items = [
        ("history", "历史…", Command::History),
        ("settings", "设置…", Command::Settings),
        ("quit", "退出", Command::Quit),
    ];
    let menu = Menu::new();
    for (i, (id, title, _)) in items.iter().enumerate() {
        if i == 2 {
            menu.append(&PredefinedMenuItem::separator())?;
        }
        menu.append(&MenuItem::with_id(*id, *title, true, None))?;
    }
    // macOS tints the template image itself. Windows shows it as is: white on a dark taskbar.
    #[cfg(windows)]
    let ink = if crate::win::light_taskbar() { 0 } else { 255 };
    #[cfg(not(windows))]
    let ink = 0;
    let glyph = Icon::from_rgba(mic_glyph(ink), GLYPH, GLYPH)?;
    #[cfg(target_os = "macos")]
    let builder = TrayIconBuilder::new().with_icon_templated(glyph);
    #[cfg(not(target_os = "macos"))]
    let builder = TrayIconBuilder::new().with_icon(glyph);
    let icon = builder
        .with_tooltip("Plaid")
        .with_menu(Box::new(menu))
        .build()?;
    cx.set_global(Tray(icon));

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

/// Microphone outline as an alpha mask in gray level `ink` (black is the template image on macOS):
/// a capsule, a U-shaped holder around its lower half, and a stem with a base.
fn mic_glyph(ink: u8) -> Vec<u8> {
    let n = GLYPH as f32;
    let mut out = Vec::with_capacity((GLYPH * GLYPH * 4) as usize);
    // Signed distance to a vertical segment from (cx, y0) to (cx, y1), minus radius.
    let capsule = |x: f32, y: f32, cx: f32, y0: f32, y1: f32, r: f32| {
        let dy = y - y.clamp(y0, y1);
        ((x - cx).powi(2) + dy.powi(2)).sqrt() - r
    };
    let c = n / 2.;
    for py in 0..GLYPH {
        for px in 0..GLYPH {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            // Filled capsule body.
            let body = capsule(x, y, c, 8.5, 15.5, 5.5);
            // U holder: ring of radius 10 around (c, 15.5), lower half only, 2 px stroke.
            let ring = (((x - c).powi(2) + (y - 15.5).powi(2)).sqrt() - 10.).abs() - 1.25;
            let holder = if y >= 15.5 { ring } else { f32::MAX };
            let stem = capsule(x, y, c, 25.5, 30.5, 1.25);
            let base = if (x - c).abs() <= 6. {
                (y - 31.).abs() - 1.25
            } else {
                f32::MAX
            };
            let d = body.min(holder).min(stem).min(base);
            let a = (0.5 - d).clamp(0., 1.);
            out.extend_from_slice(&[ink, ink, ink, (a * 255.) as u8]);
        }
    }
    out
}
