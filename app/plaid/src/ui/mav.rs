//! The settings window's controls, drawn with GPUI primitives in the dark "edge light" style: near-black
//! surfaces, hairline borders, pill controls and the HUD's cyan as the accent. Fonts are Space Grotesk and
//! JetBrains Mono (bundled, see `ui::init`); CJK falls back to the system fonts.
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

pub const BG: u32 = 0x0c0d10;
pub const SURFACE: u32 = 0x14161b;
pub const WELL: u32 = 0x0f1014;
pub const RAISED: u32 = 0x1a1c22;
pub const RAISED2: u32 = 0x1e2128;
pub const HOVER: u32 = 0x262a32;
pub const TEXT: u32 = 0xecedef;
pub const TEXT2: u32 = 0x8b8f98;
pub const TEXT3: u32 = 0x5c6069;
pub const CYAN: u32 = 0x7fd8ff;
pub const VIOLET: u32 = 0xb59cff;
pub const GREEN: u32 = 0x7ee08a;
pub const AMBER: u32 = 0xffc65a;
pub const RED: u32 = 0xff7a70;
/// Hairlines, `0xRRGGBBAA` (use with `hexa`).
pub const LINE: u32 = 0xffffff0f;
pub const LINE2: u32 = 0xffffff1a;
pub const LINE3: u32 = 0xffffff24;
/// Selected-text background.
pub const SELECTION: u32 = 0x7fd8ff4d;

pub const HINT: u32 = TEXT2;
pub const LINK: u32 = CYAN;
pub const TITLEBAR_H: f32 = 38.;

/// `div().id(..)` for anything clickable. Snapshot builds also record the element's bounds under its id, so offscreen
/// checks can click controls by id (see `snapshot::click`).
pub fn hit(id: impl Into<ElementId>) -> Stateful<Div> {
    let id: ElementId = id.into();
    #[cfg(feature = "snapshot")]
    let key = id.to_string();
    let d = div().id(id).relative();
    #[cfg(feature = "snapshot")]
    let d = d.child(
        canvas(
            move |b, _, _| PROBES.with(|p| p.borrow_mut().insert(key, b)),
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0(),
    );
    d
}

#[cfg(feature = "snapshot")]
thread_local! {
    /// Bounds of every `hit` element painted in the last frame, by id.
    pub static PROBES: std::cell::RefCell<std::collections::HashMap<String, Bounds<Pixels>>> = Default::default();
}

pub fn hex(c: u32) -> Hsla {
    rgb(c).into()
}

/// `0xRRGGBBAA`.
pub fn hexa(c: u32) -> Hsla {
    rgba(c).into()
}

/// `c` (0xRRGGBB) at `alpha` (0–255).
pub fn tint(c: u32, alpha: u32) -> Hsla {
    hexa((c << 8) | alpha.min(255))
}

const CJK: [&str; 4] = [
    "PingFang SC",
    "Hiragino Sans GB",
    "Microsoft YaHei UI",
    "Microsoft YaHei",
];

fn family(name: &str, lead: &[&str]) -> Font {
    Font {
        fallbacks: Some(FontFallbacks::from_fonts(
            lead.iter()
                .chain(CJK.iter())
                .map(|s| s.to_string())
                .collect(),
        )),
        ..gpui_kit::font(name.to_string())
    }
}

/// The HUD's font (Lucida Grande); its capacity and reveal are tuned to these metrics.
pub fn font() -> Font {
    family("Lucida Grande", &["Lucida Sans Unicode"])
}

pub const SANS: &str = "Space Grotesk";
pub const MONO: &str = "JetBrains Mono";

/// Space Grotesk, the settings window's text face.
pub fn sans() -> Font {
    family(SANS, &[])
}

/// JetBrains Mono for numbers, keys and status lines.
pub fn mono() -> Font {
    family(MONO, &["Menlo", "Consolas"])
}

pub fn bold() -> FontWeight {
    FontWeight::BOLD
}

pub fn medium() -> FontWeight {
    FontWeight::MEDIUM
}

pub fn shadow(x: f32, y: f32, blur: f32, spread: f32, color: u32, inset: bool) -> BoxShadow {
    BoxShadow {
        color: hexa(color),
        offset: point(px(x), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset,
    }
}

/// A soft glow of `c` (0xRRGGBB).
pub fn glow(c: u32, blur: f32, alpha: u32) -> BoxShadow {
    shadow(0., 0., blur, 0., (c << 8) | alpha.min(255), false)
}

/// Left-to-right gradient between two `0xRRGGBBAA` colors.
pub fn hgrada(left: u32, right: u32) -> Background {
    linear_gradient(
        90.,
        linear_color_stop(hexa(left), 0.),
        linear_color_stop(hexa(right), 1.),
    )
}

/// The window's text defaults: Space Grotesk 13 px on the near-black ground.
pub fn root() -> Div {
    div()
        .font(sans())
        .text_size(px(13.))
        .line_height(px(18.))
        .text_color(hex(TEXT))
        .bg(hex(BG))
}

// --- Title bar -------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Close,
    Min,
    Zoom,
}

/// One window light: a grey dot that takes its color while the pointer is over the group `lights`.
pub fn light(kind: Light, active: bool) -> Stateful<Div> {
    let (color, area, id) = match kind {
        Light::Close => (0xff5f57, WindowControlArea::Close, "light-close"),
        Light::Min => (0xfebc2e, WindowControlArea::Min, "light-min"),
        Light::Zoom => (0x28c840, WindowControlArea::Max, "light-zoom"),
    };
    let id = if active { id } else { "light-off" };
    let d = hit(SharedString::from(format!("{id}-{}", kind as u8)))
        .size(px(12.))
        .flex_none()
        .rounded_full()
        .bg(hex(0x3a3d45))
        .when(active, |d| d.group_hover("lights", |s| s.bg(hex(color))));
    if !active {
        return d;
    }
    // Windows hit-tests these areas natively; macOS ignores them, so clicks do the work there.
    let d = d.window_control_area(area);
    #[cfg(target_os = "macos")]
    let d = d.on_click(move |_, w, _| match kind {
        Light::Close => w.remove_window(),
        Light::Min => w.minimize_window(),
        Light::Zoom => w.zoom_window(),
    });
    d
}

pub fn lights(active: bool) -> Div {
    div()
        .group("lights")
        .flex()
        .flex_none()
        .gap(px(8.))
        .child(light(Light::Close, active))
        .child(light(Light::Min, active))
        .child(light(Light::Zoom, active))
}

/// A strip that moves the window when dragged (and is the native drag area on Windows).
pub fn drag_area() -> Div {
    div()
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |e, w, _| {
            if e.click_count == 2 {
                w.zoom_window();
            } else {
                w.start_window_move();
            }
        })
}

/// Plain title bar for secondary windows: lights and a centered title over a drag strip.
pub fn titlebar(title: impl Into<SharedString>, active: bool) -> Div {
    div()
        .relative()
        .h(px(TITLEBAR_H))
        .flex_none()
        .flex()
        .items_center()
        .child(
            drag_area()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .text_color(hex(if active { TEXT2 } else { TEXT3 }))
                .child(title.into()),
        )
        .child(div().pl(px(14.)).child(lights(active)))
}

pub fn icon(path: &'static str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(path)
        .size(px(size))
        .flex_none()
        .text_color(color)
}

// --- Cards and rows --------------------------------------------------------------------------------------------

/// Section label above a `group_body` card.
pub fn group(title: impl Into<SharedString>) -> Div {
    div().flex().flex_col().child(
        div()
            .text_size(px(12.))
            .font_weight(medium())
            .text_color(hex(TEXT2))
            .ml(px(4.))
            .mb(px(8.))
            .child(title.into()),
    )
}

/// The card: surface fill, hairline border, 16 px corners.
pub fn group_body() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(20.))
        .py(px(16.))
        .rounded(px(16.))
        .bg(hex(SURFACE))
        .border_1()
        .border_color(hexa(LINE))
}

/// A card with its title inside, then its rows.
pub fn section(title: impl Into<SharedString>, rows: impl IntoIterator<Item = AnyElement>) -> Div {
    group_body()
        .child(
            div()
                .text_size(px(15.))
                .font_weight(medium())
                .pb(px(2.))
                .child(title.into()),
        )
        .children(rows)
}

/// Card title line: the title and a muted note after it; callers may add a child at the far end.
pub fn card_head(title: impl Into<SharedString>, note: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_baseline()
        .gap(px(10.))
        .child(
            div()
                .text_size(px(15.))
                .font_weight(medium())
                .child(title.into()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(12.))
                .text_color(hex(TEXT2))
                .child(note.into()),
        )
}

/// Full-width setting: label and a line of explanation on the left, the switch on the right. The row is the hit
/// target.
pub fn toggle_row(
    id: impl Into<ElementId>,
    on: bool,
    label: impl Into<SharedString>,
    sub: impl Into<SharedString>,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let sub: SharedString = sub.into();
    hit(id)
        .flex()
        .items_center()
        .gap(px(16.))
        .py(px(10.))
        .border_t_1()
        .border_color(hexa(LINE))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(label.into())
                .when(!sub.is_empty(), |d| {
                    d.child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(17.))
                            .text_color(hex(TEXT2))
                            .child(sub),
                    )
                }),
        )
        .child(switch(on))
        .on_click(move |_, w, cx| on_toggle(!on, w, cx))
}

/// `toggle_row`'s layout with any control on the right (a segmented choice, a button).
pub fn option_row(
    label: impl Into<SharedString>,
    sub: impl Into<SharedString>,
    control: impl IntoElement,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .py(px(10.))
        .border_t_1()
        .border_color(hexa(LINE))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(label.into())
                .child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(17.))
                        .text_color(hex(TEXT2))
                        .child(sub.into()),
                ),
        )
        .child(control)
}

pub const LABEL_W: f32 = 150.;

/// Form row: label column, then the control.
pub fn row(label: impl Into<SharedString>, control: impl IntoElement) -> Div {
    let label: SharedString = label.into();
    let label: SharedString = label
        .strip_suffix('：')
        .map(|s| SharedString::from(s.to_string()))
        .unwrap_or(label);
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .min_h(px(30.))
        .child(
            div()
                .w(px(LABEL_W))
                .flex_none()
                .text_color(hex(TEXT2))
                .when(!label.is_empty(), |d| d.child(label)),
        )
        .child(div().flex_1().min_w_0().child(control))
}

pub fn hint(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .line_height(px(17.))
        .text_color(hex(TEXT2))
        .child(text.into())
}

// --- Buttons ---------------------------------------------------------------------------------------------------

fn button_frame(id: impl Into<ElementId>, small: bool) -> Stateful<Div> {
    hit(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(if small { 24. } else { 30. }))
        .px(px(if small { 10. } else { 14. }))
        .rounded_full()
        .text_size(px(if small { 12. } else { 13. }))
        .whitespace_nowrap()
}

fn plain(b: Stateful<Div>) -> Stateful<Div> {
    b.bg(hex(RAISED2))
        .border_1()
        .border_color(hexa(LINE2))
        .hover(|s| s.bg(hex(HOVER)))
        .active(|s| s.bg(hex(0x2c3039)))
}

/// Pill button on the raised fill.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    plain(button_frame(id, false))
        .child(label.into())
        .on_click(on_click)
}

pub fn small_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    plain(button_frame(id, true))
        .child(label.into())
        .on_click(on_click)
}

/// The primary action: solid cyan with dark text.
pub fn default_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    button_frame(id, false)
        .bg(hex(CYAN))
        .text_color(hex(BG))
        .font_weight(medium())
        .shadow(vec![glow(CYAN, 14., 0x40)])
        .hover(|s| s.bg(hex(0x9be2ff)))
        .child(label.into())
        .on_click(on_click)
}

/// Dims a control and lays an occluding cover over it, so its own click handlers never fire.
pub fn disabled(el: Stateful<Div>) -> Stateful<Div> {
    el.opacity(0.4)
        .relative()
        .child(div().absolute().inset_0().cursor_default().occlude())
}

/// Pop-up button face: current value and a chevron. The menu is drawn by the caller.
pub fn popup(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    width: f32,
) -> Stateful<Div> {
    hit(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .w(px(width))
        .h(px(30.))
        .pl(px(12.))
        .pr(px(9.))
        .rounded(px(8.))
        .bg(hex(RAISED2))
        .border_1()
        .border_color(hexa(LINE2))
        .hover(|s| s.bg(hex(HOVER)))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .child(icon("plaid/chev-down.svg", 12., hex(TEXT2)))
}

/// One entry in a drawn pop-up menu.
pub struct MenuEntry {
    pub label: SharedString,
    pub checked: bool,
    /// Draw a separator line below this entry.
    pub rule: bool,
}

/// Height of one `menu` row; callers that line the checked row up with the button use it.
pub const MENU_ROW: f32 = 28.;

/// The open menu of a pop-up: raised dark panel, cyan wash under the pointer.
pub fn menu(
    id: impl Into<SharedString>,
    width: f32,
    items: Vec<MenuEntry>,
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let on_pick = std::rc::Rc::new(on_pick);
    // Occlude: a click on an entry must not also reach the control drawn under the menu.
    hit(id.clone())
        .occlude()
        .min_w(px(width))
        .p(px(4.))
        .rounded(px(10.))
        .bg(hex(RAISED2))
        .border_1()
        .border_color(hexa(LINE3))
        .shadow(vec![shadow(0., 12., 32., 0., 0x000000a0, false)])
        .children(items.into_iter().enumerate().map(move |(i, it)| {
            let pick = on_pick.clone();
            let row = hit(SharedString::from(format!("{id}-{i}")))
                .h(px(MENU_ROW))
                .flex()
                .items_center()
                .pl(px(6.))
                .pr(px(16.))
                .rounded(px(6.))
                .hover(|s| s.bg(tint(CYAN, 0x26)))
                .child(div().w(px(18.)).flex_none().when(it.checked, |d| {
                    d.child(icon("plaid/check.svg", 11., hex(CYAN)))
                }))
                .child(div().whitespace_nowrap().child(it.label))
                .on_click(move |_, w, cx| pick(i, w, cx));
            let rule = it
                .rule
                .then(|| div().my(px(4.)).mx(px(6.)).h(px(1.)).bg(hexa(LINE2)));
            div().child(row).children(rule)
        }))
}

// --- Switches, radios, segmented -------------------------------------------------------------------------------

/// The bare switch: cyan with a glow when on.
pub fn switch(on: bool) -> Div {
    let track = div()
        .flex()
        .flex_none()
        .items_center()
        .w(px(32.))
        .h(px(18.))
        .p(px(2.))
        .rounded_full();
    let knob = div().size(px(14.)).rounded_full();
    if on {
        track
            .justify_end()
            .bg(hex(CYAN))
            .shadow(vec![glow(CYAN, 10., 0x55)])
            .child(knob.bg(hex(BG)))
    } else {
        track
            .bg(hex(RAISED2))
            .border_1()
            .border_color(hexa(LINE3))
            .child(knob.bg(hex(TEXT2)))
    }
}

/// Switch with its label; the whole row is the hit target.
pub fn checkbox(
    id: impl Into<ElementId>,
    on: bool,
    label: impl Into<SharedString>,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    hit(id)
        .flex()
        .items_center()
        .gap(px(10.))
        .min_h(px(26.))
        .child(switch(on))
        .when(!label.is_empty(), |d| d.child(label))
        .on_click(move |_, w, cx| on_toggle(!on, w, cx))
}

pub fn radio(
    id: impl Into<ElementId>,
    on: bool,
    label: impl Into<SharedString>,
    on_pick: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let dot = div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1();
    let dot = if on {
        dot.border_color(hex(CYAN))
            .shadow(vec![glow(CYAN, 8., 0x44)])
            .child(div().size(px(8.)).rounded_full().bg(hex(CYAN)))
    } else {
        dot.border_color(hexa(LINE3)).bg(hex(WELL))
    };
    hit(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .child(dot)
        .child(label.into())
        .on_click(move |_, w, cx| on_pick(w, cx))
}

/// Pill group: the selected item sits on a raised pill inside a dark well.
pub fn segmented(
    id: impl Into<SharedString>,
    items: &[&str],
    selected: usize,
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let on_pick = std::rc::Rc::new(on_pick);
    hit(id.clone())
        .flex()
        .flex_none()
        .h(px(30.))
        .p(px(3.))
        .gap(px(2.))
        .rounded_full()
        .bg(hex(WELL))
        .border_1()
        .border_color(hexa(LINE))
        .children(items.iter().enumerate().map(|(i, label)| {
            let pick = on_pick.clone();
            hit(SharedString::from(format!("{id}-{i}")))
                .flex()
                .items_center()
                .px(px(12.))
                .rounded_full()
                .text_size(px(12.))
                .whitespace_nowrap()
                .map(|s| {
                    if i == selected {
                        s.bg(hex(RAISED2)).text_color(hex(TEXT))
                    } else {
                        s.text_color(hex(TEXT2)).hover(|s| s.text_color(hex(TEXT)))
                    }
                })
                .child(label.to_string())
                .on_click(move |_, w, cx| pick(i, w, cx))
        }))
}

/// Disclosure chevron with a label.
pub fn disclosure(
    id: impl Into<ElementId>,
    open: bool,
    label: impl Into<SharedString>,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    hit(id)
        .flex()
        .items_center()
        .gap(px(6.))
        .text_color(hex(TEXT2))
        .hover(|s| s.text_color(hex(TEXT)))
        .child(icon(
            if open {
                "plaid/chev-down.svg"
            } else {
                "plaid/chev-right.svg"
            },
            12.,
            hex(TEXT2),
        ))
        .child(label.into())
        .on_click(move |_, w, cx| on_toggle(w, cx))
}

// --- Text fields -----------------------------------------------------------------------------------------------

/// Well around an editor; the editor itself draws no frame. Focus adds a cyan ring.
pub fn field_frame(focused: bool) -> Div {
    div()
        .flex()
        .items_center()
        .min_h(px(30.))
        .rounded(px(8.))
        .bg(hex(WELL))
        .border_1()
        .map(|d| {
            if focused {
                d.border_color(tint(CYAN, 0x99))
                    .shadow(vec![shadow(0., 0., 0., 3., 0x7fd8ff26, false)])
            } else {
                d.border_color(hexa(LINE2))
            }
        })
}

/// Up/down pair next to a number field.
pub fn stepper(
    id: impl Into<SharedString>,
    on_step: impl Fn(i64, &mut Window, &mut App) + 'static,
) -> Div {
    let id: SharedString = id.into();
    let on_step = std::rc::Rc::new(on_step);
    let half = |dir: i64, glyph: &'static str| {
        let step = on_step.clone();
        hit(SharedString::from(format!("{id}-{dir}")))
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(hex(HOVER)))
            .child(icon(glyph, 7., hex(TEXT2)))
            .on_click(move |_, w, cx| step(dir, w, cx))
    };
    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(18.))
        .h(px(30.))
        .rounded(px(6.))
        .overflow_hidden()
        .bg(hex(RAISED2))
        .border_1()
        .border_color(hexa(LINE2))
        .child(half(1, "plaid/up.svg"))
        .child(div().h(px(1.)).bg(hexa(LINE2)))
        .child(half(-1, "plaid/down.svg"))
}

// --- Indicators ------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Off,
}

impl Status {
    pub fn color(self) -> u32 {
        match self {
            Status::Ok => GREEN,
            Status::Warn => AMBER,
            Status::Off => TEXT3,
        }
    }
}

/// Status light.
pub fn dot(s: Status, size: f32) -> Div {
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .bg(hex(s.color()))
        .when(s != Status::Off, |d| {
            d.shadow(vec![glow(s.color(), 8., 0x99)])
        })
}

/// Status line: light and short mono text.
pub fn badge(s: Status, text: impl Into<SharedString>) -> Div {
    let color = match s {
        Status::Ok => TEXT,
        Status::Warn => AMBER,
        Status::Off => TEXT2,
    };
    div()
        .flex()
        .items_center()
        .gap(px(7.))
        .font(mono())
        .text_size(px(11.))
        .text_color(hex(color))
        .child(dot(s, 7.))
        .child(text.into())
}

/// Keyboard key cap.
pub fn keycap(text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(24.))
        .h(px(22.))
        .px(px(8.))
        .rounded(px(6.))
        .bg(hex(RAISED2))
        .border_1()
        .border_color(hexa(LINE2))
        .font(mono())
        .text_size(px(11.))
        .shadow(vec![shadow(0., 2., 0., 0., 0x08090bff, false)])
        .child(text.into())
}

// --- Tables ----------------------------------------------------------------------------------------------------

/// One table row: hairline between rows, cyan wash when selected.
pub fn table_row(id: impl Into<ElementId>, index: usize, selected: bool) -> Stateful<Div> {
    let r = hit(id)
        .flex()
        .items_center()
        .min_h(px(30.))
        .when(index > 0, |d| d.border_t_1().border_color(hexa(LINE)));
    if selected {
        r.bg(tint(CYAN, 0x1f))
    } else {
        r.hover(|s| s.bg(hexa(0xffffff08)))
    }
}

/// Cell of `table_row`; width 0 means flexible.
pub fn cell(w: f32) -> Div {
    let c = div().px(px(10.)).overflow_hidden();
    if w > 0. {
        c.w(px(w)).flex_none()
    } else {
        c.flex_1().min_w_0()
    }
}

/// The card a table sits in.
pub fn table_well() -> Div {
    div()
        .flex()
        .flex_col()
        .rounded(px(12.))
        .bg(hex(SURFACE))
        .border_1()
        .border_color(hexa(LINE))
        .overflow_hidden()
}

// --- Spinner ---------------------------------------------------------------------------------------------------

/// Twelve-spoke progress indicator; `phase` rotates the bright spoke.
pub fn spinner(size: f32, color: u32, phase: usize) -> Div {
    div()
        .relative()
        .size(px(size))
        .flex_none()
        .children((0..12).map(|i| {
            let o = 0.2 + 0.8 * (((12 + phase - i) % 12) as f32 / 11.);
            svg()
                .path("plaid/spoke.svg")
                .absolute()
                .inset_0()
                .size(px(size))
                .text_color(hexa((color << 8) | ((o * 255.) as u32)))
                .with_transformation(Transformation::rotate(radians(
                    i as f32 * std::f32::consts::TAU / 12.,
                )))
        }))
}
