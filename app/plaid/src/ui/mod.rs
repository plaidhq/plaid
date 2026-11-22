//! Settings window ("edge light", dark): a header with the page pills, one page, a status line. Pages read settings live
//! from settings.json and the dictation history from disk (refreshed when records.jsonl changes); anything that
//! changes records goes to the controller as a `Msg`, which owns the store while the app runs.
pub mod assets;
mod commands;
mod dictionary;
mod general;
mod history;
mod key_field;
pub mod mav;
pub mod onboarding;
mod overview;
mod perf;
mod refine;
#[cfg(all(feature = "snapshot", any(target_os = "macos", target_os = "linux")))]
pub mod snapshot;

use crate::{config, hotkey, secret};
use gpui_kit::base::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use mav::*;
use plaid_core::dictation::Msg;
use plaid_core::store::Store;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

actions!(plaid, [CloseWindow, Minimize, Quit]);

/// Standard window shortcuts for the settings and onboarding windows. The app has no main menu (it is a menu bar
/// app), so AppKit supplies none of these. Text fields keep their own bindings (gpui-base input).
pub fn bind_keys(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new("ctrl-w", CloseWindow, None)]);
    // The global handlers only mark the action handled; the keystroke observer then acts on the window that
    // received the key (App alone does not know it, and active_window is the platform's key window).
    cx.on_action(|_: &CloseWindow, _| {});
    cx.on_action(|_: &Minimize, _| {});
    cx.observe_keystrokes(|e, window, _| match e.action.as_deref() {
        Some(a) if a.as_any().is::<CloseWindow>() => window.remove_window(),
        Some(a) if a.as_any().is::<Minimize>() => window.minimize_window(),
        _ => {}
    })
    .detach();
    cx.on_action(|_: &Quit, cx| cx.quit());
}

/// Registers the bundled fonts and the dark palette the kit's text input draws with. Call after `gpui_kit::init`.
pub fn init(cx: &mut App) {
    let fonts: [&'static [u8]; 5] = [
        include_bytes!("../../resources/fonts/SpaceGrotesk-Regular.ttf"),
        include_bytes!("../../resources/fonts/SpaceGrotesk-Medium.ttf"),
        include_bytes!("../../resources/fonts/SpaceGrotesk-Bold.ttf"),
        include_bytes!("../../resources/fonts/JetBrainsMono-Regular.ttf"),
        include_bytes!("../../resources/fonts/JetBrainsMono-SemiBold.ttf"),
    ];
    let fonts = fonts.into_iter().map(std::borrow::Cow::Borrowed).collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        crate::trace::log(&format!("fonts: {e}"));
    }
    let theme = gpui_kit::base::Theme::global_mut(cx);
    theme.appearance = gpui_kit::base::ThemeAppearance::Dark;
    let c = &mut theme.tokens.colors;
    c.foreground = hex(TEXT);
    c.muted_foreground = hex(TEXT3);
    c.surface = hex(WELL);
    c.background = hex(BG);
    c.border = hexa(LINE2);
    c.accent = hex(0x2f6f8c);
    c.selection = tint(CYAN, 0x55);
}

/// What the running app hands the settings window: the controller's inbox and the hotkey hook's state.
pub struct Live {
    pub tx: async_channel::Sender<Msg>,
    pub keys: Arc<hotkey::State>,
}

impl Global for Live {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Overview,
    Perf,
    History,
    Voice,
    Refine,
    Dictionary,
    Commands,
    General,
}

impl Page {
    /// Header order; a rule goes before 声音 and 设置.
    pub const ALL: [Page; 8] = [
        Page::Overview,
        Page::Perf,
        Page::History,
        Page::Voice,
        Page::Refine,
        Page::Dictionary,
        Page::Commands,
        Page::General,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::Overview => "今天",
            Page::Perf => "报告",
            Page::History => "历史",
            Page::Voice => "声音",
            Page::Refine => "文字",
            Page::Dictionary => "词典",
            Page::Commands => "命令",
            Page::General => "设置",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Page::Overview => "overview",
            Page::Perf => "perf",
            Page::History => "history",
            Page::Voice => "voice",
            Page::Refine => "refine",
            Page::Dictionary => "dictionary",
            Page::Commands => "commands",
            Page::General => "general",
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
                    !n.starts_with("CADefaultDeviceAggregate")
                        && !n.starts_with("VPAUAggregateAudioDevice")
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
    let r = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
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
        .h(px(30.))
        .px(px(10.))
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
    drop_menu(
        id,
        popup(id, current, width),
        width,
        items,
        open,
        toggle,
        close,
        pick,
    )
}

/// `pick_list` with any face: the menu named `{id}-menu` opens over `face`.
#[allow(clippy::too_many_arguments)]
pub fn drop_menu(
    id: impl Into<SharedString>,
    face: Stateful<Div>,
    width: f32,
    items: Vec<MenuEntry>,
    open: bool,
    toggle: impl Fn(&mut Window, &mut App) + 'static,
    close: impl Fn(&mut Window, &mut App) + 'static,
    pick: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Div {
    let id: SharedString = id.into();
    div()
        .flex()
        .flex_col()
        .child(face.on_click(move |_, w, cx| toggle(w, cx)))
        .when(open, |d| {
            // The menu opens with the checked item over the button face.
            let lift = items.iter().position(|i| i.checked).unwrap_or(0) as f32 * MENU_ROW + 5.;
            d.child(deferred(
                anchored().snap_to_window_with_margin(px(8.)).child(
                    menu(SharedString::from(format!("{id}-menu")), width, items, pick)
                        .mt(px(-lift))
                        .on_mouse_down_out(move |_, w, cx| close(w, cx)),
                ),
            ))
        })
}

pub struct Settings {
    page: Page,
    model: Entity<Model>,
    pages: Vec<(Page, AnyView)>,
    /// 词典 suggestions waiting for a decision (sidebar badge); recounted when the model or hotwords.txt changes.
    pending: usize,
    /// The page shown before the last switch: the header pill slides from its tab.
    from: Option<Page>,
    /// Each tab's (left, width) inside the nav row, measured while painting; the pill slides between them.
    tabs: Rc<RefCell<Vec<(Page, f32, f32)>>>,
    nav_x: Rc<Cell<f32>>,
}

impl Settings {
    pub fn new(
        page: Page,
        model: Entity<Model>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Settings {
        cx.observe(&model, |this, _, cx| this.count_pending(cx))
            .detach();
        cx.observe_window_activation(window, |_, window, cx| {
            // Shortcut recording only while the window is frontmost: otherwise the hook would swallow and bind the
            // next key typed in another app. Closing the recorder's channel ends the page's recording state.
            if !window.is_window_active()
                && let Some(l) = cx.try_global::<Live>()
            {
                l.keys.stop_recording();
            }
            cx.notify();
        })
        .detach();
        let pending = dictionary::pending(model.read(cx));
        Settings {
            page,
            model,
            pages: vec![],
            pending,
            from: None,
            tabs: Rc::default(),
            nav_x: Rc::default(),
        }
    }

    fn count_pending(&mut self, cx: &mut Context<Self>) {
        self.pending = dictionary::pending(self.model.read(cx));
        cx.notify();
    }

    pub fn show(&mut self, page: Page, cx: &mut Context<Self>) {
        if page != self.page {
            self.from = Some(self.page);
        }
        self.page = page;
        cx.notify();
    }

    fn view(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) -> AnyView {
        if let Some((_, v)) = self.pages.iter().find(|(p, _)| *p == page) {
            return v.clone();
        }
        let m = self.model.clone();
        let v: AnyView = match page {
            Page::General => cx
                .new(|cx| general::General::new(m, false, window, cx))
                .into(),
            Page::Voice => cx
                .new(|cx| general::General::new(m, true, window, cx))
                .into(),
            Page::History => {
                let s = cx.entity().downgrade();
                let go = move |p: Page, _: &mut Window, cx: &mut App| {
                    let _ = s.update(cx, |s, cx| s.show(p, cx));
                };
                cx.new(|cx| history::History::new(m, go, window, cx)).into()
            }
            Page::Refine => cx.new(|cx| refine::Refine::new(m, window, cx)).into(),
            Page::Commands => cx.new(|cx| commands::Commands::new(m, window, cx)).into(),
            Page::Overview => cx.new(|cx| overview::Overview::new(m, window, cx)).into(),
            Page::Perf => cx.new(|cx| perf::Perf::new(m, window, cx)).into(),
            Page::Dictionary => {
                let nav = cx.entity().downgrade();
                cx.new(|cx| dictionary::Dictionary::new(m, nav, window, cx))
                    .into()
            }
        };
        self.pages.push((page, v.clone()));
        v
    }

    /// The page area. In the app a page switch fades the new page in while it settles 6 px up; snapshot runs draw
    /// it at rest so shots never catch the fade.
    fn page_frame(&self, page: AnyView, cx: &App) -> AnyElement {
        let frame = div().flex_1().min_h_0().relative().child(page);
        if cx.try_global::<Live>().is_none() {
            return frame.into_any_element();
        }
        frame
            .with_animation(
                ElementId::Name(format!("page-{}", self.page.slug()).into()),
                Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                |d, t| d.opacity(t).top(px((1. - t) * 6.)),
            )
            .into_any_element()
    }

    /// 56 px header: window lights, wordmark, page pills. Only the wordmark and the empty space drag the window,
    /// so the pills keep their clicks (Windows hit-tests drag areas natively).
    fn header(&self, active: bool, cx: &mut Context<Self>) -> Div {
        let pending = self.pending;
        let tabs = self.tabs.clone();
        let nav_x = self.nav_x.clone();
        let spot = |p: Page| {
            let x0 = nav_x.get();
            tabs.borrow()
                .iter()
                .find(|t| t.0 == p)
                .map(|t| (t.1 - x0, t.2))
        };
        // Live only (snapshots draw the pill on the tab): slide from the previous tab once both are measured.
        let slide = cx
            .try_global::<Live>()
            .and(self.from)
            .and_then(|f| Some((f, spot(f)?, spot(self.page)?)));
        let mut nav = div()
            .relative()
            .flex()
            .items_center()
            .gap(px(2.))
            .ml(px(10.))
            .child(
                canvas(move |b, _, _| nav_x.set(b.origin.x.into()), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            );
        if let Some((from, a, b)) = slide {
            nav = nav.child(
                div()
                    .absolute()
                    .top_0()
                    .h(px(30.))
                    .rounded_full()
                    .bg(hexa(0xffffff17))
                    .with_animation(
                        ElementId::Name(
                            format!("tab-pill-{}-{}", from.slug(), self.page.slug()).into(),
                        ),
                        Animation::new(Duration::from_millis(220)).with_easing(ease_out_quint()),
                        move |d, t| {
                            d.left(px(a.0 + (b.0 - a.0) * t))
                                .w(px(a.1 + (b.1 - a.1) * t))
                        },
                    ),
            );
        }
        for p in Page::ALL {
            if matches!(p, Page::Voice | Page::General) {
                nav = nav.child(div().w(px(1.)).h(px(14.)).mx(px(8.)).bg(hexa(LINE3)));
            }
            let on = p == self.page;
            let measure = {
                let tabs = tabs.clone();
                canvas(
                    move |b, _, _| {
                        let mut t = tabs.borrow_mut();
                        t.retain(|x| x.0 != p);
                        t.push((p, b.origin.x.into(), b.size.width.into()));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
            };
            let count = (p == Page::Dictionary && pending > 0).then(|| {
                div()
                    .ml(px(6.))
                    .h(px(16.))
                    .min_w(px(16.))
                    .px(px(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(hex(CYAN))
                    .text_color(hex(BG))
                    .font(mono())
                    .text_size(px(10.))
                    .font_weight(bold())
                    .child(pending.to_string())
            });
            nav = nav.child(
                hit(p.slug())
                    .flex()
                    .items_center()
                    .h(px(30.))
                    .px(px(12.))
                    .rounded_full()
                    .map(|d| {
                        if on && slide.is_some() {
                            d.text_color(hex(TEXT))
                        } else if on {
                            d.bg(hexa(0xffffff17)).text_color(hex(TEXT))
                        } else {
                            d.text_color(hex(TEXT2))
                                .hover(|s| s.text_color(hex(TEXT)).bg(hexa(0xffffff0a)))
                        }
                    })
                    .child(measure)
                    .child(p.title())
                    .children(count)
                    .on_click(cx.listener(move |this, _, _, cx| this.show(p, cx))),
            );
        }
        div()
            .h(px(56.))
            .flex_none()
            .flex()
            .items_center()
            .child(div().pl(px(20.)).pr(px(22.)).child(lights(active)))
            .child(
                drag_area()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h_full()
                    .text_size(px(19.))
                    .line_height(px(24.))
                    .font_weight(bold())
                    .child("plaid")
                    .child(
                        div()
                            .size(px(6.))
                            .rounded_full()
                            .bg(hex(CYAN))
                            .shadow(vec![glow(CYAN, 10., 0xff)]),
                    ),
            )
            .child(nav)
            .child(drag_area().flex_1().h_full())
    }

    /// 40 px mono status line, then the 2 px cyan→violet edge light along the window's bottom.
    fn footer(&self, cx: &mut Context<Self>) -> Div {
        let facts = &self.model.read(cx).facts;
        let (status, text) = if !facts.volc_key {
            (Status::Warn, "未设置 API Key")
        } else if !facts.trusted {
            (Status::Warn, "需要辅助功能权限")
        } else {
            (Status::Ok, "就绪 · 火山引擎")
        };
        let words = self.model.read(cx).store.all.len();
        div()
            .relative()
            .h(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(18.))
            .px(px(22.))
            .font(mono())
            .text_size(px(11.))
            .text_color(hex(TEXT2))
            .child(badge(status, text))
            .child(format!("{words} 条记录"))
            .child(
                div()
                    .ml_auto()
                    .child(format!("Plaid {}", env!("CARGO_PKG_VERSION"))),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(2.))
                    .flex()
                    .shadow(vec![shadow(0., -4., 18., 0., 0x7fd8ff59, false)])
                    .child(div().flex_1().h_full().bg(hgrada(0x7fd8ff00, 0x7fd8ffff)))
                    .child(div().flex_1().h_full().bg(hgrada(0x7fd8ffff, 0xb59cffff)))
                    .child(div().flex_1().h_full().bg(hgrada(0xb59cffff, 0xb59cff00))),
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
            .child(gpui_kit::base::TextSelectionLayer)
            .child(self.header(window.is_window_active(), cx))
            .child(self.page_frame(page, cx))
            .child(self.footer(cx))
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
        .gap(px(14.))
        .pt(px(6.))
        .px(px(28.))
        .pb(px(20.))
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
            poll_facts(poll, cx);
        }
        Err(e) => crate::trace::log(&format!("settings window: {e}")),
    }
}

/// Picks up new dictations and facts that change while a window is open, until the model is dropped.
fn poll_facts(model: WeakEntity<Model>, cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            let alive = cx.update(|cx| {
                model
                    .update(cx, |m, cx| {
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
        let Ok(h) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::AppKit(h) = h.as_raw() else {
            return;
        };
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
