//! Settings window (Mac OS X 10.9 look): source list on the left, one page on the right. Pages read settings live
//! from settings.json and the dictation history from disk (refreshed when records.jsonl changes); anything that
//! changes records goes to the controller as a `Msg`, which owns the store while the app runs.
pub mod assets;
mod general;
mod key_field;
pub mod mav;
#[cfg(all(feature = "snapshot", any(target_os = "macos", target_os = "linux")))]
pub mod snapshot;

use crate::{config, hotkey, secret};
use gpui_kit::base::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use mav::*;
use plaid_core::dictation::Msg;
use plaid_core::store::Store;
use std::sync::Arc;
use std::time::SystemTime;

/// What the running app hands the settings window: the controller's inbox and the hotkey hook's state.
pub struct Live {
    pub tx: async_channel::Sender<Msg>,
    pub keys: Arc<hotkey::State>,
}

impl Global for Live {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Overview,
    History,
    Dictionary,
    Refine,
    Commands,
    General,
    Perf,
}

impl Page {
    pub const ALL: [Page; 7] = [
        Page::Overview,
        Page::History,
        Page::Dictionary,
        Page::Refine,
        Page::Commands,
        Page::General,
        Page::Perf,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::Overview => "概览",
            Page::History => "历史",
            Page::Dictionary => "词典",
            Page::Refine => "优化",
            Page::Commands => "命令",
            Page::General => "设置",
            Page::Perf => "性能",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Page::Overview => "overview",
            Page::History => "history",
            Page::Dictionary => "dictionary",
            Page::Refine => "refine",
            Page::Commands => "commands",
            Page::General => "general",
            Page::Perf => "perf",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Page::Overview => "plaid/overview.svg",
            Page::History => "plaid/history.svg",
            Page::Dictionary => "plaid/dictionary.svg",
            Page::Refine => "plaid/refine.svg",
            Page::Commands => "plaid/commands.svg",
            Page::General => "plaid/settings.svg",
            Page::Perf => "plaid/perf.svg",
        }
    }
}

/// A connected input device: (cpal id, name).
pub type Mic = (String, String);

/// Facts from the OS that pages show. Snapshot runs use a fixture instead of touching the keychain or audio.
#[derive(Clone)]
pub struct Facts {
    pub volc_key: bool,
    pub mics: Vec<Mic>,
    pub default_mic: Option<String>,
    pub trusted: bool,
    /// Snapshot runs: never touch the keychain.
    pub fixture: bool,
}

impl Facts {
    pub fn load() -> Facts {
        let (mics, default_mic) = mics();
        Facts {
            volc_key: secret::get("apiKey").ok().flatten().is_some(),
            mics,
            default_mic,
            trusted: trusted(),
            fixture: false,
        }
    }
}

pub fn trusted() -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::mac::trusted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Input devices without opening any stream.
fn mics() -> (Vec<Mic>, Option<String>) {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let name = |d: &cpal::Device| d.description().map(|d| d.to_string()).ok();
    let list = host
        .input_devices()
        .map(|ds| {
            ds.filter_map(|d| Some((d.id().ok()?.id().to_string(), name(&d)?)))
                .filter(|(_, n)| {
                    !n.starts_with("CADefaultDeviceAggregate") && !n.starts_with("VPAUAggregateAudioDevice")
                })
                .collect()
        })
        .unwrap_or_default();
    (list, host.default_input_device().as_ref().and_then(name))
}

/// Shared state of the settings window: history from disk, OS facts. Pages observe it; `notify` re-renders all.
pub struct Model {
    pub store: Store,
    stamp: Option<SystemTime>,
    pub facts: Facts,
}

impl Model {
    pub fn new(facts: Facts) -> Model {
        let dir = config::data_dir();
        Model {
            stamp: stamp(),
            store: Store::load(&dir),
            facts,
        }
    }

    /// Reloads the history when records.jsonl changed on disk.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let s = stamp();
        if s != self.stamp {
            self.stamp = s;
            self.store = Store::load(&config::data_dir());
            cx.notify();
        }
    }

    /// Writes a setting and re-renders the pages.
    pub fn set(&mut self, key: &str, v: impl Into<serde_json::Value>, cx: &mut Context<Self>) {
        if let Err(e) = config::set(key, v.into()) {
            crate::trace::log(&format!("settings: {key}: {e}"));
        }
        cx.notify();
    }
}

fn stamp() -> Option<SystemTime> {
    std::fs::metadata(config::data_dir().join("records.jsonl"))
        .and_then(|m| m.modified())
        .ok()
}

/// Bool settings the Swift app reads as `!= "0"` (unset = on).
pub fn on_unless_zero(key: &str) -> bool {
    config::value(key).as_deref() != Some("0")
}

/// Bool settings the Swift app reads as `== "1"` (unset = off).
pub fn on_if_one(key: &str) -> bool {
    config::value(key).as_deref() == Some("1")
}

pub fn bool_str(on: bool) -> &'static str {
    if on { "1" } else { "0" }
}

/// Posts to the controller when the app is running (not in snapshot runs).
pub fn send(m: Msg, cx: &App) {
    if let Some(l) = cx.try_global::<Live>() {
        let _ = l.tx.try_send(m);
    }
}

/// Reveals a path in Finder / Explorer.
pub fn reveal(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(windows)]
    let r = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let r: std::io::Result<()> = Ok(());
    if let Err(e) = r {
        crate::trace::log(&format!("reveal: {e}"));
    }
}

/// The data folder as the user knows it.
pub fn data_dir_label() -> String {
    let d = config::data_dir().display().to_string();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && d.starts_with(&h) => format!("~{}", &d[h.len()..]),
        _ => d,
    }
}

/// A text field in the square bezel; the editor is the kit's unstyled input.
pub fn field(state: &Entity<InputState>, width: f32, window: &Window, cx: &App) -> Div {
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    field_frame(focused)
        .w(px(width))
        .h(px(22.))
        .px(px(5.))
        .child(div().flex_1().min_w_0().child(Input::new(state)))
}

/// A pop-up button with its drawn menu. The page keeps which menu is open (`open`), toggles it on click, and
/// gets `pick(index)`; clicking outside closes it through `close`.
#[allow(clippy::too_many_arguments)]
pub fn pick_list(
    id: &'static str,
    width: f32,
    current: impl Into<SharedString>,
    items: Vec<MenuEntry>,
    open: bool,
    toggle: impl Fn(&mut Window, &mut App) + 'static,
    close: impl Fn(&mut Window, &mut App) + 'static,
    pick: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex()
        .flex_col()
        .child(popup(id, current, width).on_click(move |_, w, cx| toggle(w, cx)))
        .when(open, |d| {
            d.child(deferred(
                anchored().snap_to_window_with_margin(px(8.)).child(
                    menu(SharedString::from(format!("{id}-menu")), width, items, pick)
                        .mt(px(1.))
                        .on_mouse_down_out(move |_, w, cx| close(w, cx)),
                ),
            ))
        })
}

pub struct Settings {
    page: Page,
    model: Entity<Model>,
    pages: Vec<(Page, AnyView)>,
}

impl Settings {
    pub fn new(page: Page, model: Entity<Model>, window: &mut Window, cx: &mut Context<Self>) -> Settings {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        cx.observe_window_activation(window, |_, _, cx| cx.notify()).detach();
        Settings {
            page,
            model,
            pages: vec![],
        }
    }

    pub fn show(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        cx.notify();
    }

    fn view(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) -> AnyView {
        if let Some((_, v)) = self.pages.iter().find(|(p, _)| *p == page) {
            return v.clone();
        }
        let m = self.model.clone();
        let v: AnyView = match page {
            Page::General => cx.new(|cx| general::General::new(m, window, cx)).into(),
            _ => cx.new(|_| Todo(page)).into(),
        };
        self.pages.push((page, v.clone()));
        v
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let group = |label: &'static str, pages: &[Page], cx: &mut Context<Self>| {
            div().child(source_header(label)).children(pages.iter().map(|&p| {
                source_row(p.slug(), p.icon(), p.title(), p == self.page)
                    .on_click(cx.listener(move |this, _, _, cx| this.show(p, cx)))
            }))
        };
        let facts = &self.model.read(cx).facts;
        let (status, text) = if !facts.volc_key {
            (Status::Warn, "未设置 API Key")
        } else if !facts.trusted {
            (Status::Warn, "需要辅助功能权限")
        } else {
            (Status::Ok, "就绪 · 火山引擎")
        };
        div()
            .w(px(188.))
            .flex_none()
            .flex()
            .flex_col()
            .py(px(6.))
            .bg(hex(SIDEBAR))
            .border_r_1()
            .border_color(hex(SIDEBAR_EDGE))
            .child(group(
                "听写",
                &[Page::Overview, Page::History],
                cx,
            ))
            .child(group(
                "个性化",
                &[Page::Dictionary, Page::Refine, Page::Commands],
                cx,
            ))
            .child(group("应用", &[Page::General, Page::Perf], cx))
            .child(
                div()
                    .mt_auto()
                    .px(px(14.))
                    .pb(px(4.))
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(badge(status, text))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(hex(0x7b8590))
                            .child(format!("Plaid {}", env!("CARGO_PKG_VERSION"))),
                    ),
            )
    }
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = self.view(self.page, window, cx);
        root()
            .id("settings")
            .size_full()
            .flex()
            .flex_col()
            .child(titlebar(self.page.title(), window.is_window_active()))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.sidebar(cx))
                    .child(div().flex_1().min_w_0().h_full().child(page)),
            )
    }
}

/// Placeholder for pages not built yet.
struct Todo(Page);

impl Render for Todo {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().p(px(22.)).child(hint(format!("{}：尚未完成", self.0.title())))
    }
}

/// Scrolling page body with the canvas paddings.
pub fn page_body(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(12.))
        .pt(px(16.))
        .px(px(22.))
        .pb(px(18.))
}

pub const W: f32 = 980.;
pub const H: f32 = 720.;

pub fn window_options(bounds: Option<Bounds<Pixels>>, show: bool) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds.unwrap_or(Bounds {
            origin: Default::default(),
            size: size(px(W), px(H)),
        }))),
        titlebar: Some(TitlebarOptions {
            title: Some("Plaid 设置".into()),
            appears_transparent: true,
            traffic_light_position: None,
        }),
        focus: show,
        show,
        window_min_size: Some(size(px(860.), px(600.))),
        ..Default::default()
    }
}

struct Open(AnyWindowHandle, Entity<Settings>);

impl Global for Open {}

/// Opens the settings window on `page`, or brings the open one forward.
pub fn open(page: Page, cx: &mut App) {
    if let Some(Open(handle, view)) = cx.try_global::<Open>() {
        let (handle, view) = (*handle, view.clone());
        let alive = handle
            .update(cx, |_, window, cx| {
                view.update(cx, |s, cx| s.show(page, cx));
                window.activate_window();
            })
            .is_ok();
        if alive {
            os_activate(true);
            return;
        }
    }
    let bounds = cx.primary_display().map(|d| {
        let b = d.bounds();
        Bounds::centered_at(b.center(), size(px(W), px(H)))
    });
    os_activate(true);
    let model = cx.new(|_| Model::new(Facts::load()));
    let poll = model.downgrade();
    let opened = open_window(window_options(bounds, true), cx, |window, cx| {
        native_chrome(window);
        cx.new(|cx| Settings::new(page, model, window, cx))
    });
    match opened {
        Ok((handle, view)) => {
            cx.set_global(Open(handle, view));
            cx.on_window_closed(move |cx, id| {
                if id != handle.window_id() {
                    return;
                }
                cx.remove_global::<Open>();
                os_activate(false);
                if let Some(l) = cx.try_global::<Live>() {
                    l.keys.stop_recording();
                }
            })
            .detach();
            // Pick up new dictations and facts that change while the window is open.
            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(1))
                        .await;
                    let alive = cx.update(|cx| {
                        poll.update(cx, |m, cx| {
                            m.refresh(cx);
                            let t = trusted();
                            if t != m.facts.trusted {
                                m.facts.trusted = t;
                                cx.notify();
                            }
                        })
                        .is_ok()
                    });
                    if !alive {
                        break;
                    }
                }
            })
            .detach();
        }
        Err(e) => crate::trace::log(&format!("settings window: {e}")),
    }
}

/// Regular app (Dock icon, ⌘-tab) while the window is open; accessory again after it closes.
fn os_activate(regular: bool) {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        if let Some(mtm) = objc2::MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(if regular {
                NSApplicationActivationPolicy::Regular
            } else {
                NSApplicationActivationPolicy::Accessory
            });
            if regular {
                #[allow(deprecated)]
                app.activateIgnoringOtherApps(true);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = regular;
}

/// macOS: hide the native traffic lights; the title bar draws its own.
fn native_chrome(window: &Window) {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSView, NSWindowButton};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(h) = HasWindowHandle::window_handle(window) else { return };
        let RawWindowHandle::AppKit(h) = h.as_raw() else { return };
        // SAFETY: GPUI's live NSView for this window; main thread.
        let view: &NSView = unsafe { h.ns_view.cast().as_ref() };
        let Some(w) = view.window() else { return };
        for b in [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ] {
            if let Some(b) = w.standardWindowButton(b) {
                b.setHidden(true);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}
