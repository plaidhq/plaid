//! Mac OS X 10.9 controls drawn with GPUI primitives. Colors and metrics come from the design canvas
//! (Mavericks fidelity pass). GPUI gradients take two stops, so the four-stop Aqua fills are stacked bands.
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

pub const WINDOW: u32 = 0xededed;
pub const SIDEBAR: u32 = 0xdee4ea;
pub const SIDEBAR_EDGE: u32 = 0xa9adb2;
pub const ROW_SELECTED: u32 = 0x3875d7;
pub const ROW_ALT: u32 = 0xedf3fe;
pub const HINT: u32 = 0x666666;
pub const LINK: u32 = 0x2a62c9;
pub const TITLEBAR_H: f32 = 22.;

const DEFAULT_FILL: [(u32, f32); 4] = [
    (0xd3e7fd, 0.),
    (0xa9cff8, 0.45),
    (0x8cbdf4, 0.55),
    (0xa6cff9, 1.),
];
const CAP_FILL: [(u32, f32); 4] = [
    (0xb4d6fb, 0.),
    (0x73aff3, 0.48),
    (0x5b9fef, 0.55),
    (0x8fc2f7, 1.),
];
const TAB_FILL: [(u32, f32); 4] = [
    (0x9ec8f7, 0.),
    (0x5b9cee, 0.48),
    (0x3f88e9, 0.55),
    (0x6fadf3, 1.),
];
const CHECK_FILL: [(u32, f32); 4] = [
    (0xc3defc, 0.),
    (0x86b9f3, 0.48),
    (0x6aa8f0, 0.55),
    (0x9dccf8, 1.),
];

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

pub fn font() -> Font {
    Font {
        fallbacks: Some(FontFallbacks::from_fonts(
            [
                "Lucida Sans Unicode",
                "Hiragino Sans GB",
                "PingFang SC",
                "Microsoft YaHei",
            ]
            .map(String::from)
            .to_vec(),
        )),
        ..gpui_kit::font("Lucida Grande")
    }
}

pub fn bold() -> FontWeight {
    FontWeight::BOLD
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

/// Top-to-bottom two-stop gradient.
pub fn vgrad(top: u32, bottom: u32) -> Background {
    linear_gradient(
        180.,
        linear_color_stop(hex(top), 0.),
        linear_color_stop(hex(bottom), 1.),
    )
}

fn vgrada(top: u32, bottom: u32) -> Background {
    linear_gradient(
        180.,
        linear_color_stop(hexa(top), 0.),
        linear_color_stop(hexa(bottom), 1.),
    )
}

/// Absolute layers that together paint a multi-stop vertical gradient inside a box with corner radius `r`.
fn bands(stops: &[(u32, f32)], r: f32) -> Vec<Div> {
    let n = stops.len() - 1;
    (0..n)
        .map(|i| {
            let ((a, pa), (b, pb)) = (stops[i], stops[i + 1]);
            let mut d = div()
                .absolute()
                .left_0()
                .right_0()
                .top(relative(pa))
                .h(relative(pb - pa))
                .bg(vgrad(a, b));
            if i == 0 {
                d = d.rounded_t(px(r));
            }
            if i == n - 1 {
                d = d.rounded_b(px(r));
            }
            d
        })
        .collect()
}

/// The window's text defaults: Lucida Grande 13 px, black.
pub fn root() -> Div {
    div()
        .font(font())
        .text_size(px(13.))
        .line_height(px(17.))
        .text_color(hex(0x000000))
        .bg(hex(WINDOW))
}

// --- Title bar -------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Close,
    Min,
    Zoom,
}

/// One traffic light: colored ring, glow rising from the bottom, and a small gloss on top.
pub fn light(kind: Light, active: bool) -> Stateful<Div> {
    let (ring, top, bottom, area, id) = match (kind, active) {
        (_, false) => (0xa9a9a9, 0xd4d4d4, 0xe8e8e8, None, "light-off"),
        (Light::Close, _) => (
            0xa8342b,
            0xc8392f,
            0xffa59a,
            Some(WindowControlArea::Close),
            "light-close",
        ),
        (Light::Min, _) => (
            0xa7731a,
            0xd0911b,
            0xffdd8c,
            Some(WindowControlArea::Min),
            "light-min",
        ),
        (Light::Zoom, _) => (
            0x2e8b2a,
            0x35a12c,
            0xb0eb98,
            Some(WindowControlArea::Max),
            "light-zoom",
        ),
    };
    let mut d = hit(SharedString::from(format!("{id}-{}", kind as u8)))
        .relative()
        .size(px(12.))
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(hex(ring))
        .bg(vgrad(top, bottom))
        .shadow(vec![shadow(0., 1., 0., 0., 0xffffff99, false)])
        .child(
            div()
                .absolute()
                .top(px(0.5))
                .left(px(2.5))
                .w(px(5.))
                .h(px(4.))
                .rounded_full()
                .bg(vgrada(0xfffffff0, 0xffffff00)),
        );
    if let Some(a) = area {
        // Windows hit-tests these areas natively; macOS ignores them, so clicks do the work there.
        d = d.window_control_area(a);
        #[cfg(target_os = "macos")]
        {
            d = d.on_click(move |_, w, _| match kind {
                Light::Close => w.remove_window(),
                Light::Min => w.minimize_window(),
                Light::Zoom => w.zoom_window(),
            });
        }
    }
    d
}

/// 22 px unified title bar with drawn traffic lights and a centered title. The bar is the window's drag area.
pub fn titlebar(title: impl Into<SharedString>, active: bool) -> Div {
    div()
        .relative()
        .h(px(TITLEBAR_H))
        .flex_none()
        .flex()
        .items_center()
        .bg(vgrad(0xe9e9e9, 0xd1d1d1))
        .border_b_1()
        .border_color(hex(0xa2a2a2))
        .shadow(vec![shadow(0., 1., 0., 0., 0xf7f7f7ff, true)])
        .window_control_area(WindowControlArea::Drag)
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top_0()
                .bottom_0()
                .flex()
                .items_center()
                .justify_center()
                .text_color(hex(if active { 0x4d4d4d } else { 0x9a9a9a }))
                .child(title.into()),
        )
        .child(
            div()
                .flex()
                .gap(px(8.))
                .pl(px(8.))
                .child(light(Light::Close, active))
                .child(light(Light::Min, active))
                .child(light(Light::Zoom, active)),
        )
}

// --- Source list -----------------------------------------------------------------------------------------------

pub fn source_header(label: impl Into<SharedString>) -> Div {
    div()
        .px(px(12.))
        .pt(px(9.))
        .pb(px(3.))
        .text_size(px(11.))
        .font_weight(bold())
        .text_color(hex(0x707e8c))
        .child(label.into())
}

pub fn icon(path: &'static str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(path)
        .size(px(size))
        .flex_none()
        .text_color(color)
}

pub fn source_row(
    id: impl Into<ElementId>,
    glyph: &'static str,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    let row = hit(id)
        .h(px(22.))
        .flex()
        .items_center()
        .gap(px(7.))
        .pl(px(16.))
        .pr(px(10.))
        .border_t_1()
        .border_color(transparent_black());
    if selected {
        row.bg(vgrad(0x64a0e3, 0x2a6ccc))
            .border_color(hex(0x4a89d8))
            .border_b_1()
            .text_color(hex(0xffffff))
            .font_weight(bold())
            .child(icon(glyph, 16., hex(0xffffff)))
            .child(label.into())
    } else {
        row.text_color(hex(0x1c1c1c))
            .child(icon(glyph, 16., hex(0x6d7f93)))
            .child(label.into())
    }
}

// --- Boxes and rows --------------------------------------------------------------------------------------------

/// Titled group box (NSBox).
pub fn group(title: impl Into<SharedString>) -> Div {
    div().flex().flex_col().child(
        div()
            .text_size(px(11.))
            .text_color(hex(0x3a3a3a))
            .ml(px(6.))
            .mb(px(4.))
            .child(title.into()),
    )
}

pub fn group_body() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(16.))
        .py(px(12.))
        .rounded(px(5.))
        .bg(hexa(0x0000000a))
        .border_1()
        .border_color(hexa(0x00000024))
}

/// A titled group box with its rows.
pub fn section(title: impl Into<SharedString>, rows: impl IntoIterator<Item = AnyElement>) -> Div {
    group(title).child(group_body().children(rows))
}

pub const LABEL_W: f32 = 150.;

/// Form row: right-aligned label column, then the control.
pub fn row(label: impl Into<SharedString>, control: impl IntoElement) -> Div {
    let label: SharedString = label.into();
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .min_h(px(22.))
        .child(
            div()
                .w(px(LABEL_W))
                .flex_none()
                .flex()
                .justify_end()
                .text_color(hex(0x1a1a1a))
                .when(!label.is_empty(), |d| d.child(label)),
        )
        .child(div().flex_1().min_w_0().child(control))
}

pub fn hint(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(11.))
        .line_height(px(15.))
        .text_color(hex(HINT))
        .child(text.into())
}

pub fn small(text: impl Into<SharedString>) -> Div {
    div().text_size(px(11.)).child(text.into())
}

// --- Buttons ---------------------------------------------------------------------------------------------------

fn button_frame(id: impl Into<ElementId>, small: bool) -> Stateful<Div> {
    hit(id)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(if small { 18. } else { 21. }))
        .px(px(if small { 9. } else { 13. }))
        .rounded(px(4.))
        .text_size(px(if small { 11. } else { 13. }))
        .whitespace_nowrap()
        .shadow(vec![shadow(0., 1., 1., 0., 0x00000012, false)])
}

/// Rounded push button (NSRoundedBezelStyle).
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    button_frame(id, false)
        .bg(vgrad(0xffffff, 0xf0f0f0))
        .border_1()
        .border_color(hex(0xa5a5a5))
        .active(|s| s.bg(vgrad(0xd9d9d9, 0xc9c9c9)))
        .child(label.into())
        .on_click(on_click)
}

pub fn small_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    button_frame(id, true)
        .bg(vgrad(0xffffff, 0xf0f0f0))
        .border_1()
        .border_color(hex(0xa5a5a5))
        .active(|s| s.bg(vgrad(0xd9d9d9, 0xc9c9c9)))
        .child(label.into())
        .on_click(on_click)
}

/// The default (blue, pulsing in 10.9; static here) push button.
pub fn default_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    button_frame(id, false)
        .border_1()
        .border_color(hex(0x5a87c8))
        .children(bands(&DEFAULT_FILL, 3.))
        .child(div().relative().child(label.into()))
        .on_click(on_click)
}

pub fn disabled(el: Stateful<Div>) -> Stateful<Div> {
    el.opacity(0.5)
}

/// NSPopUpButton face: current value and the blue double-arrow cap. The menu is drawn by the caller.
pub fn popup(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    width: f32,
) -> Stateful<Div> {
    hit(id)
        .flex()
        .flex_none()
        .w(px(width))
        .h(px(21.))
        .rounded(px(4.))
        .bg(vgrad(0xffffff, 0xf0f0f0))
        .border_1()
        .border_color(hex(0xa5a5a5))
        .shadow(vec![shadow(0., 1., 1., 0., 0x00000012, false)])
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .pl(px(9.))
                .pr(px(6.))
                .child(div().truncate().child(label.into())),
        )
        .child(
            div()
                .relative()
                .w(px(17.))
                .flex_none()
                .border_l_1()
                .border_color(hex(0x5a86c4))
                .children(bands(&CAP_FILL, 0.).into_iter().enumerate().map(|(i, b)| {
                    if i == 2 {
                        b.rounded_br(px(3.))
                    } else if i == 0 {
                        b.rounded_tr(px(3.))
                    } else {
                        b
                    }
                }))
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon("plaid/updown.svg", 9., hex(0xffffff))),
                ),
        )
}

/// One entry in a drawn pop-up menu.
pub struct MenuEntry {
    pub label: SharedString,
    pub checked: bool,
}

/// The open menu of a pop-up: white rounded panel, blue highlight under the pointer.
pub fn menu(
    id: impl Into<SharedString>,
    width: f32,
    items: Vec<MenuEntry>,
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let on_pick = std::rc::Rc::new(on_pick);
    hit(id.clone())
        .min_w(px(width))
        .py(px(4.))
        .rounded(px(5.))
        .bg(hexa(0xfffffff5))
        .border_1()
        .border_color(hexa(0x00000033))
        .shadow(vec![shadow(0., 6., 18., 0., 0x00000059, false)])
        .children(items.into_iter().enumerate().map(move |(i, it)| {
            let pick = on_pick.clone();
            hit(SharedString::from(format!("{id}-{i}")))
                .h(px(19.))
                .flex()
                .items_center()
                .pl(px(6.))
                .pr(px(18.))
                .hover(|s| s.bg(hex(0x3875d7)).text_color(hex(0xffffff)))
                .child(
                    div()
                        .w(px(14.))
                        .flex_none()
                        .when(it.checked, |d| d.child("✓")),
                )
                .child(div().whitespace_nowrap().child(it.label))
                .on_click(move |_, w, cx| pick(i, w, cx))
        }))
}

// --- Check boxes, radios, segmented ----------------------------------------------------------------------------

/// Check box with its label; the whole row is the hit target.
pub fn checkbox(
    id: impl Into<ElementId>,
    on: bool,
    label: impl Into<SharedString>,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let mark = div()
        .relative()
        .size(px(14.))
        .flex_none()
        .rounded(px(3.))
        .border_1()
        .shadow(vec![shadow(0., 1., 0., 0., 0xffffffff, true)]);
    let mark = if on {
        mark.border_color(hex(0x4a76b5))
            .children(bands(&CHECK_FILL, 2.))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("plaid/check.svg", 10., hex(0x111111))),
            )
    } else {
        mark.border_color(hex(0x8e8e8e))
            .bg(vgrad(0xffffff, 0xe9e9e9))
    };
    hit(id)
        .flex()
        .items_center()
        .gap(px(6.))
        .child(mark)
        .child(label.into())
        .on_click(move |_, w, cx| on_toggle(!on, w, cx))
}

pub fn radio(
    id: impl Into<ElementId>,
    on: bool,
    label: impl Into<SharedString>,
    on_pick: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let dot = div()
        .relative()
        .size(px(14.))
        .flex_none()
        .rounded_full()
        .border_1();
    let dot = if on {
        dot.border_color(hex(0x4a76b5))
            .bg(vgrad(0x9dc7f6, 0x5a9eef))
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(5.)).rounded_full().bg(hex(0x111111)))
    } else {
        dot.border_color(hex(0x8e8e8e))
            .bg(vgrad(0xffffff, 0xe9e9e9))
    };
    hit(id)
        .flex()
        .items_center()
        .gap(px(6.))
        .child(dot)
        .child(label.into())
        .on_click(move |_, w, cx| on_pick(w, cx))
}

/// Segmented control (NSSegmentedControl, rounded style).
pub fn segmented(
    id: impl Into<SharedString>,
    items: &[&str],
    selected: usize,
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let on_pick = std::rc::Rc::new(on_pick);
    let n = items.len();
    hit(id.clone())
        .flex()
        .flex_none()
        .h(px(21.))
        .rounded(px(4.))
        .border_1()
        .border_color(hex(0xa5a5a5))
        .shadow(vec![shadow(0., 1., 1., 0., 0x00000012, false)])
        .children(items.iter().enumerate().map(|(i, label)| {
            let pick = on_pick.clone();
            let mut s = hit(SharedString::from(format!("{id}-{i}")))
                .flex()
                .items_center()
                .px(px(11.))
                .whitespace_nowrap()
                .when(i > 0, |d| d.border_l_1().border_color(hex(0xa5a5a5)));
            if i == 0 {
                s = s.rounded_l(px(3.));
            }
            if i == n - 1 {
                s = s.rounded_r(px(3.));
            }
            if i == selected {
                s = s
                    .bg(vgrad(0xb4b4b4, 0xcdcdcd))
                    .shadow(vec![shadow(0., 1., 2., 0., 0x00000040, true)]);
            } else {
                s = s.bg(vgrad(0xffffff, 0xf0f0f0));
            }
            s.child(label.to_string())
                .on_click(move |_, w, cx| pick(i, w, cx))
        }))
}

/// Tab strip of an NSTabView: segmented look with a blue selected tab, centered over the box.
pub fn tabs(
    id: impl Into<SharedString>,
    items: &[&str],
    selected: usize,
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let on_pick = std::rc::Rc::new(on_pick);
    let n = items.len();
    hit(id.clone())
        .flex()
        .flex_none()
        .h(px(22.))
        .rounded(px(4.))
        .border_1()
        .border_color(hex(0x9a9a9a))
        .shadow(vec![shadow(0., 1., 1., 0., 0x00000012, false)])
        .children(items.iter().enumerate().map(|(i, label)| {
            let pick = on_pick.clone();
            let r = |d: Stateful<Div>| {
                let d = if i == 0 { d.rounded_l(px(3.)) } else { d };
                if i == n - 1 { d.rounded_r(px(3.)) } else { d }
            };
            let s = r(hit(SharedString::from(format!("{id}-{i}")))
                .relative()
                .flex()
                .items_center()
                .px(px(12.))
                .whitespace_nowrap()
                .when(i > 0, |d| d.border_l_1().border_color(hex(0x9a9a9a))));
            let s = if i == selected {
                s.text_color(hex(0xffffff)).children(
                    bands(&TAB_FILL, 0.).into_iter().enumerate().map(|(k, b)| {
                        let b = if i == 0 && k == 0 {
                            b.rounded_tl(px(3.))
                        } else {
                            b
                        };
                        let b = if i == 0 && k == 2 {
                            b.rounded_bl(px(3.))
                        } else {
                            b
                        };
                        let b = if i == n - 1 && k == 0 {
                            b.rounded_tr(px(3.))
                        } else {
                            b
                        };
                        if i == n - 1 && k == 2 {
                            b.rounded_br(px(3.))
                        } else {
                            b
                        }
                    }),
                )
            } else {
                s.bg(vgrad(0xffffff, 0xf0f0f0))
            };
            s.child(div().relative().child(label.to_string()))
                .on_click(move |_, w, cx| pick(i, w, cx))
        }))
}

/// Box under a tab strip; the strip overlaps its top edge by half its height.
pub fn tab_box() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(16.))
        .pt(px(20.))
        .pb(px(14.))
        .rounded(px(5.))
        .bg(hexa(0x0000000a))
        .border_1()
        .border_color(hexa(0x00000024))
}

/// Disclosure triangle with a label.
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
        .text_color(hex(0x1a1a1a))
        .child(icon(
            if open {
                "plaid/tri-down.svg"
            } else {
                "plaid/tri-right.svg"
            },
            9.,
            hex(0x6b6b6b),
        ))
        .child(label.into())
        .on_click(move |_, w, cx| on_toggle(w, cx))
}

// --- Text fields -----------------------------------------------------------------------------------------------

/// Square bezel around an editor (NSTextField); the editor itself draws no frame.
pub fn field_frame(focused: bool) -> Div {
    div()
        .flex()
        .items_center()
        .min_h(px(22.))
        .bg(hex(0xffffff))
        .border_1()
        .border_color(hex(0x9c9c9c))
        .shadow(if focused {
            vec![
                shadow(0., 0., 2., 2., 0x579bf0bf, false),
                shadow(0., 1., 1., 0., 0x0000001f, true),
            ]
        } else {
            vec![shadow(0., 1., 1., 0., 0x0000001f, true)]
        })
}

/// Arrow pair of an NSStepper.
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
            .active(|s| s.bg(hex(0xd0d0d0)))
            .child(icon(glyph, 7., hex(0x333333)))
            .on_click(move |_, w, cx| step(dir, w, cx))
    };
    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(13.))
        .h(px(21.))
        .rounded(px(4.))
        .border_1()
        .border_color(hex(0xa0a0a0))
        .bg(vgrad(0xffffff, 0xececec))
        .child(half(1, "plaid/up.svg"))
        .child(div().h(px(1.)).bg(hex(0xb8b8b8)))
        .child(half(-1, "plaid/down.svg"))
}

// --- Indicators ------------------------------------------------------------------------------------------------

/// Discrete level indicator (NSLevelIndicator).
pub fn meter(lit: usize, total: usize) -> Div {
    div().flex().gap(px(3.)).children((0..total).map(|i| {
        let cell = div().w(px(8.)).h(px(9.)).rounded(px(2.));
        if i < lit {
            cell.bg(vgrad(0x9cc6f5, 0x3f86e1))
                .shadow(vec![shadow(0., 0., 0., 1., 0x1e468c73, true)])
        } else {
            cell.bg(vgrad(0xf4f4f4, 0xd6d6d6))
                .shadow(vec![shadow(0., 0., 0., 1., 0x00000038, true)])
        }
    }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Err,
    Off,
}

/// Small status jewel.
pub fn dot(s: Status, size: f32) -> Div {
    let (top, bottom) = match s {
        Status::Ok => (0xc9f3a6, 0x2f8a1b),
        Status::Warn => (0xffe08a, 0xb57b12),
        Status::Err => (0xff9a92, 0xb8302a),
        Status::Off => (0xe2e2e2, 0xa8a8a8),
    };
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .bg(vgrad(top, bottom))
        .shadow(vec![shadow(0., 0., 0., 0.5, 0x00000059, true)])
}

/// Status line: jewel and short text.
pub fn badge(s: Status, text: impl Into<SharedString>) -> Div {
    let color = match s {
        Status::Ok => 0x3c6a2b,
        Status::Warn => 0x8a5a00,
        Status::Err => 0xa3241d,
        Status::Off => 0x666666,
    };
    div()
        .flex()
        .items_center()
        .gap(px(5.))
        .text_size(px(11.))
        .text_color(hex(color))
        .child(dot(s, 9.))
        .child(text.into())
}

/// Keyboard key as a light cap (in windows).
pub fn keycap(text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(22.))
        .h(px(18.))
        .px(px(5.))
        .rounded(px(4.))
        .bg(vgrad(0xffffff, 0xe6e6e6))
        .border_1()
        .border_color(hex(0xa8a8a8))
        .text_size(px(11.))
        .shadow(vec![shadow(0., 1., 0., 0., 0x0000001a, false)])
        .child(text.into())
}

// --- Tables ----------------------------------------------------------------------------------------------------

/// NSTableView header cell strip.
pub fn table_header(cols: &[(&str, f32)]) -> Div {
    div()
        .flex()
        .flex_none()
        .h(px(17.))
        .bg(vgrad(0xffffff, 0xe9e9e9))
        .border_b_1()
        .border_color(hex(0xb1b1b1))
        .text_size(px(11.))
        .children(cols.iter().enumerate().map(|(i, (label, w))| {
            let cell = div()
                .flex()
                .items_center()
                .px(px(6.))
                .when(i > 0, |d| d.border_l_1().border_color(hex(0xd0d0d0)))
                .child(label.to_string());
            if *w > 0. {
                cell.w(px(*w)).flex_none()
            } else {
                cell.flex_1().min_w_0()
            }
        }))
}

/// One table row: alternating fill, blue when selected.
pub fn table_row(id: impl Into<ElementId>, index: usize, selected: bool) -> Stateful<Div> {
    let r = hit(id).flex().items_center().min_h(px(20.));
    if selected {
        r.bg(hex(ROW_SELECTED)).text_color(hex(0xffffff))
    } else if index % 2 == 1 {
        r.bg(hex(ROW_ALT))
    } else {
        r.bg(hex(0xffffff))
    }
}

/// Cell of `table_row`; width 0 means flexible.
pub fn cell(w: f32) -> Div {
    let c = div().px(px(6.)).overflow_hidden();
    if w > 0. {
        c.w(px(w)).flex_none()
    } else {
        c.flex_1().min_w_0()
    }
}

/// Bordered white table well.
pub fn table_well() -> Div {
    div()
        .flex()
        .flex_col()
        .bg(hex(0xffffff))
        .border_1()
        .border_color(hex(0xa8a8a8))
        .overflow_hidden()
}

/// Bottom bar under a table: gradient strip with small square buttons.
pub fn bottom_bar() -> Div {
    div()
        .flex()
        .items_center()
        .h(px(22.))
        .flex_none()
        .bg(vgrad(0xd9d9d9, 0xc3c3c3))
        .border_t_1()
        .border_color(hex(0xa8a8a8))
}

pub fn bar_button(
    id: impl Into<ElementId>,
    glyph: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    hit(id)
        .w(px(24.))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .border_r_1()
        .border_color(hex(0xa8a8a8))
        .active(|s| s.bg(hex(0xb0b0b0)))
        .child(icon(glyph, 10., hex(0x333333)))
        .on_click(on_click)
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
