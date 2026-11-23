//! First-run window: API key, permissions, hotkey. Opened at launch while the Volc key or (macOS) Accessibility
//! is missing; the Accessibility prompt is asked from here instead of at launch.
use super::general::{key_name, offered, set_shortcut};
use super::key_field::KeyField;
use super::mav::*;
use super::open_url;
use super::{Facts, Model, field, os_activate, poll_facts};
use crate::{config, secret};
use gpui_kit::base::input::InputState;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

const STEPS: [&str; 5] = ["介绍", "API Key", "权限", "快捷键", "完成"];
pub const W: f32 = 860.;
pub const H: f32 = 600.;

pub struct Onboarding {
    step: usize,
    model: Entity<Model>,
    key: Entity<KeyField>,
    trial: Entity<InputState>,
    /// The Accessibility prompt was shown once; later clicks open the settings pane instead.
    asked: bool,
    /// macOS microphone permission (see `mac::mic_allowed`).
    mic: Option<bool>,
}

/// Whether to show the window at launch.
pub fn needed() -> bool {
    secret::get("apiKey").ok().flatten().is_none() || !super::trusted()
}

impl Onboarding {
    pub fn new(
        step: usize,
        model: Entity<Model>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Onboarding {
        let m = model.clone();
        let key = cx.new(|cx| KeyField::new(m, || "apiKey".into(), window, cx));
        let trial = cx.new(|cx| InputState::new(window, cx).placeholder("文字会出现在这里"));
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        cx.observe_window_activation(window, |_, _, cx| cx.notify())
            .detach();
        let fixture = model.read(cx).facts.fixture;
        Onboarding {
            step,
            model,
            key,
            trial,
            asked: false,
            mic: if fixture { Some(true) } else { mic_allowed() },
        }
    }

    /// Whether 继续 is allowed: the key is stored, and on macOS Accessibility is granted.
    fn can_continue(&self, cx: &App) -> bool {
        let f = &self.model.read(cx).facts;
        match self.step {
            1 => f.volc_key,
            2 => f.trusted,
            _ => true,
        }
    }

    fn intro(&self) -> Div {
        let bars = [6., 16., 24., 12., 20., 4.];
        let mac = cfg!(target_os = "macos");
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .items_start()
            .child(
                div()
                    .size(px(72.))
                    .rounded(px(16.))
                    .bg(hex(RAISED2))
                    .shadow(vec![
                        shadow(0., 0., 0., 1., LINE2, true),
                        glow(CYAN, 18., 0x26),
                    ])
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(4.))
                    .children(
                        bars.map(|h| div().w(px(3.5)).h(px(h + 3.)).rounded(px(2.)).bg(hex(CYAN))),
                    ),
            )
            .child(h2("欢迎使用 Plaid"))
            .child(para(
                "按住一个键说话，松开后文字出现在光标处。识别用火山引擎的流式语音识别。",
            ))
            .child(para("接下来需要三样东西："))
            .child(
                div().flex().flex_col().pl(px(8.)).gap(px(4.)).children(
                    [
                        "火山引擎 API Key",
                        if mac {
                            "辅助功能和麦克风权限"
                        } else {
                            "确认麦克风可用"
                        },
                        "选一个快捷键",
                    ]
                    .map(|t| div().child(format!("•  {t}"))),
                ),
            )
    }

    fn api_key(&self) -> Div {
        let store = if cfg!(windows) {
            "Windows 凭据管理器"
        } else {
            "钥匙串"
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(h2("火山引擎 API Key"))
            .child(para(
                "在火山引擎控制台的语音技术里创建应用，复制 API Key 粘贴到这里。",
            ))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .items_start()
                    .child(
                        div()
                            .w(px(64.))
                            .h(px(22.))
                            .flex()
                            .items_center()
                            .justify_end()
                            .child("API Key："),
                    )
                    .child(self.key.clone()),
            )
            .child(hint(format!("Key 只存在本机的{store}里。")).pl(px(72.)))
    }

    fn permissions(&self, cx: &mut Context<Self>) -> Div {
        let facts = self.model.read(cx).facts.clone();
        let body = div().flex().flex_col().gap(px(6.)).child(h2("权限"));
        if cfg!(target_os = "macos") {
            let ax = if facts.trusted {
                (
                    disabled(button("ax", "已允许", |_, _, _| {})),
                    badge(Status::Ok, "已授权"),
                )
            } else {
                (
                    button(
                        "ax",
                        "打开系统偏好设置…",
                        cx.listener(|this, _, _, cx| {
                            if this.asked {
                                open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
                            } else {
                                this.asked = true;
                                request_trust();
                            }
                            cx.notify();
                        }),
                    ),
                    badge(Status::Warn, "尚未授权"),
                )
            };
            let mic = match self.mic {
                Some(true) => (
                    disabled(button("mic", "已允许", |_, _, _| {})),
                    badge(Status::Ok, "已授权"),
                ),
                Some(false) => (
                    button("mic", "打开系统偏好设置…", |_, _, _| {
                        open_url(
                            "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
                        )
                    }),
                    badge(Status::Warn, "已拒绝"),
                ),
                None => (
                    disabled(button("mic", "允许", |_, _, _| {})),
                    badge(Status::Off, "第一次录音时系统会询问"),
                ),
            };
            body.child(para("macOS 需要两项授权，授权后这里会自动更新。").mb(px(8.)))
                .child(perm(
                    "plaid/accessibility.svg",
                    "辅助功能",
                    "监听快捷键，并把文字粘贴到当前应用",
                    ax,
                    false,
                ))
                .child(perm(
                    "plaid/mic.svg",
                    "麦克风",
                    "录音，只在按住快捷键时打开",
                    mic,
                    true,
                ))
        } else {
            let status = if facts.mics.is_empty() {
                badge(Status::Warn, "没有找到麦克风")
            } else {
                badge(Status::Ok, "可用")
            };
            body.gap(px(10.))
                .child(para("Windows 不需要额外授权快捷键和粘贴，只需确认麦克风可用。"))
                .child(perm(
                    "plaid/mic.svg",
                    "麦克风",
                    "设置 → 隐私和安全性 → 麦克风 → 允许桌面应用访问麦克风",
                    (
                        button("mic", "打开设置…", |_, _, _| open_url("ms-settings:privacy-microphone")),
                        status,
                    ),
                    true,
                ))
                .child(
                    div()
                        .bg(tint(AMBER, 0x14))
                        .border_1()
                        .border_color(tint(AMBER, 0x40))
                        .rounded(px(4.))
                        .px(px(12.))
                        .py(px(9.))
                        .text_size(px(12.))
                        .line_height(px(19.))
                        .text_color(hex(AMBER))
                        .child("以管理员身份运行的窗口（例如任务管理器、注册表编辑器）处于前台时，快捷键和粘贴都不起作用。这是 Windows 的权限隔离，不是故障。"),
                )
        }
    }

    fn hotkey(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let (hold, _) = config::hotkeys();
        let choices: &[(i32, &str)] = if cfg!(windows) {
            &[(61, "推荐；带 AltGr 的键盘布局请换一个"), (62, "")]
        } else {
            &[
                (61, "推荐，很少和其他快捷键冲突"),
                (54, ""),
                (63, "部分键盘没有"),
            ]
        };
        let offered = offered();
        let model = self.model.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(h2("快捷键"))
            .child(para("按住说话，松开粘贴；轻点一下进入开关模式。"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(9.))
                    .pl(px(4.))
                    .children(choices.iter().filter(|(c, _)| offered.contains(c)).map(
                        |&(code, note)| {
                            let model = model.clone();
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(radio(
                                    SharedString::from(format!("hk-{code}")),
                                    hold == code,
                                    key_name(code),
                                    move |_, cx| set_shortcut(&model, "holdKey", code, cx),
                                ))
                                .when(!note.is_empty(), |d| d.child(hint(note)))
                        },
                    ))
                    .child(hint("其他按键可以稍后在设置里选。")),
            )
            .child(
                div()
                    .mt(px(6.))
                    .border_1()
                    .border_dashed()
                    .border_color(hexa(LINE3))
                    .rounded(px(5.))
                    .px(px(16.))
                    .py(px(14.))
                    .bg(hexa(GLASS_WELL))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child("试一下：点进下面的框，按住")
                            .child(keycap(key_name(hold)))
                            .child("说一句话。"),
                    )
                    .child(field(&self.trial, 520., window, cx)),
            )
    }

    fn done(&self) -> Div {
        let (hold, _) = config::hotkeys();
        let tray = if cfg!(windows) {
            "任务栏右侧的通知区域"
        } else {
            "菜单栏"
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .items_start()
            .child(
                dot(Status::Ok, 56.)
                    .flex()
                    .items_center()
                    .justify_center()
                    .shadow(vec![glow(GREEN, 18., 0x40)])
                    .child(icon("plaid/check.svg", 26., hex(BG))),
            )
            .child(h2("准备好了"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .line_height(px(21.))
                    .child(format!("Plaid 在{tray}。在任何输入框里按住"))
                    .child(keycap(key_name(hold)))
                    .child("说话即可。"),
            )
            .child(para("热词、LLM 润色和语音命令可以稍后在设置里打开。"))
    }
}

impl Render for Onboarding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = match self.step {
            0 => self.intro(),
            1 => self.api_key(),
            2 => self.permissions(cx),
            3 => self.hotkey(window, cx),
            _ => self.done(),
        };
        let step = self.step;
        let last = step == STEPS.len() - 1;
        let back = button(
            "back",
            "返回",
            cx.listener(|this, _, _, cx| {
                this.step = this.step.saturating_sub(1);
                cx.notify();
            }),
        );
        let next = default_button(
            "next",
            if last { "开始使用" } else { "继续" },
            cx.listener(move |this, _, window, cx| {
                if !this.can_continue(cx) {
                    return;
                }
                if last {
                    window.remove_window();
                } else {
                    this.step += 1;
                    cx.notify();
                }
            }),
        );
        root()
            .id("onboarding")
            .size_full()
            .flex()
            .flex_col()
            .child(titlebar("欢迎使用 Plaid", window.is_window_active()))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .w(px(190.))
                            .flex_none()
                            .pt(px(26.))
                            .pl(px(26.))
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .children(STEPS.iter().enumerate().map(|(i, name)| {
                                let d = div()
                                    .size(px(11.))
                                    .flex_none()
                                    .rounded_full()
                                    .shadow(vec![shadow(0., 0., 0., 1., LINE3, true)]);
                                let d = match i.cmp(&step) {
                                    std::cmp::Ordering::Equal => {
                                        d.bg(hex(CYAN)).shadow(vec![glow(CYAN, 8., 0x66)])
                                    }
                                    std::cmp::Ordering::Less => d.bg(hex(TEXT3)),
                                    std::cmp::Ordering::Greater => d.bg(hex(RAISED2)),
                                };
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(9.))
                                    .text_color(hex(if i == step { TEXT } else { TEXT2 }))
                                    .when(i == step, |d| d.font_weight(bold()))
                                    .child(d)
                                    .child(*name)
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .mt(px(20.))
                            .mr(px(20.))
                            .bg(hexa(GLASS))
                            .border_1()
                            .border_color(hexa(LINE))
                            .px(px(32.))
                            .py(px(28.))
                            .overflow_hidden()
                            .child(panel),
                    ),
            )
            .child(
                div()
                    .h(px(52.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(10.))
                    .px(px(20.))
                    .child(if step == 0 { disabled(back) } else { back })
                    .child(if self.can_continue(cx) {
                        next
                    } else {
                        disabled(next)
                    }),
            )
    }
}

fn h2(text: &'static str) -> Div {
    div()
        .text_size(px(17.))
        .line_height(px(22.))
        .font_weight(bold())
        .text_color(hex(TEXT))
        .child(text)
}

fn para(text: &'static str) -> Div {
    div().line_height(px(21.)).text_color(hex(TEXT)).child(text)
}

/// Permission row: icon, name with explanation, and the action with its status underneath.
fn perm(
    glyph: &'static str,
    name: &'static str,
    note: &'static str,
    (action, status): (Stateful<Div>, Div),
    last: bool,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .py(px(12.))
        .border_t_1()
        .border_color(hexa(LINE))
        .when(last, |d| d.border_b_1())
        .child(icon(glyph, 32., hex(CYAN)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().font_weight(bold()).child(name))
                .child(hint(note)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(4.))
                .child(action)
                .child(status),
        )
}

fn request_trust() {
    #[cfg(target_os = "macos")]
    crate::mac::request_trust();
}

fn mic_allowed() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        crate::mac::mic_allowed()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(true)
    }
}

pub fn window_options(bounds: Option<Bounds<Pixels>>, show: bool) -> WindowOptions {
    let mut o = super::window_options(bounds, show);
    o.window_bounds = Some(WindowBounds::Windowed(bounds.unwrap_or(Bounds {
        origin: Default::default(),
        size: size(px(W), px(H)),
    })));
    o.window_min_size = Some(size(px(W), px(H)));
    o.is_resizable = false;
    if let Some(t) = o.titlebar.as_mut() {
        t.title = Some("欢迎使用 Plaid".into());
    }
    o
}

/// Opens the first-run window centered on the primary display.
pub fn open(cx: &mut App) {
    let bounds = cx.primary_display().map(|d| {
        let b = d.bounds();
        Bounds::centered_at(b.center(), size(px(W), px(H)))
    });
    os_activate(true);
    let model = cx.new(|_| Model::new(Facts::load()));
    let poll = model.downgrade();
    match open_window(window_options(bounds, true), cx, |window, cx| {
        super::native_chrome(window);
        cx.new(|cx| Onboarding::new(0, model, window, cx))
    }) {
        Ok((handle, view)) => {
            cx.on_window_closed(move |_, id| {
                if id == handle.window_id() {
                    os_activate(false);
                }
            })
            .detach();
            poll_facts(poll, cx);
            // The microphone permission changes in System Settings while the window is open.
            let view = view.downgrade();
            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(1))
                        .await;
                    let alive = cx.update(|cx| {
                        view.update(cx, |o, cx| {
                            let m = mic_allowed();
                            if m != o.mic {
                                o.mic = m;
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
        Err(e) => crate::trace::log(&format!("onboarding window: {e}")),
    }
}
