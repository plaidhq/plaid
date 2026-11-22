//! 声音 (shortcuts, microphone, recognition) and 设置 (API key, data): one view, two pages.
use super::key_field::KeyField;
use super::mav::*;
use super::{
    Live, Model, bool_str, data_dir_label, field, on_if_one, on_unless_zero, pick_list, reveal,
};
use crate::config;
use gpui_kit::base::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::{QUIET_RMS, SILENCE_DEFAULT};

/// Both sides of ⌘ ⇧ ⌥ ⌃, and fn, in menu order.
const MODIFIERS: [(i32, &str); 9] = [
    (61, "右 ⌥"),
    (58, "左 ⌥"),
    (54, "右 ⌘"),
    (55, "左 ⌘"),
    (62, "右 ⌃"),
    (59, "左 ⌃"),
    (60, "右 ⇧"),
    (56, "左 ⇧"),
    (63, "fn"),
];

const NAMED: [(i32, &str); 32] = [
    (122, "F1"),
    (120, "F2"),
    (99, "F3"),
    (118, "F4"),
    (96, "F5"),
    (97, "F6"),
    (98, "F7"),
    (100, "F8"),
    (101, "F9"),
    (109, "F10"),
    (103, "F11"),
    (111, "F12"),
    (105, "F13"),
    (107, "F14"),
    (113, "F15"),
    (106, "F16"),
    (64, "F17"),
    (79, "F18"),
    (80, "F19"),
    (90, "F20"),
    (49, "空格"),
    (48, "Tab"),
    (51, "⌫"),
    (117, "⌦"),
    (115, "Home"),
    (119, "End"),
    (116, "Page Up"),
    (121, "Page Down"),
    (123, "←"),
    (124, "→"),
    (125, "↓"),
    (126, "↑"),
];

/// ANSI layout characters by macOS key code.
const CHARS: [(i32, &str); 47] = [
    (0, "A"),
    (1, "S"),
    (2, "D"),
    (3, "F"),
    (4, "H"),
    (5, "G"),
    (6, "Z"),
    (7, "X"),
    (8, "C"),
    (9, "V"),
    (11, "B"),
    (12, "Q"),
    (13, "W"),
    (14, "E"),
    (15, "R"),
    (16, "Y"),
    (17, "T"),
    (18, "1"),
    (19, "2"),
    (20, "3"),
    (21, "4"),
    (22, "6"),
    (23, "5"),
    (24, "="),
    (25, "9"),
    (26, "7"),
    (27, "-"),
    (28, "8"),
    (29, "0"),
    (30, "]"),
    (31, "O"),
    (32, "U"),
    (33, "["),
    (34, "I"),
    (35, "P"),
    (37, "L"),
    (38, "J"),
    (39, "'"),
    (40, "K"),
    (41, ";"),
    (42, "\\"),
    (43, ","),
    (44, "/"),
    (45, "N"),
    (46, "M"),
    (47, "."),
    (50, "`"),
];

/// Display name of a macOS key code, with Windows names for the modifiers on Windows.
pub fn key_name(code: i32) -> String {
    let find = |t: &[(i32, &str)]| {
        t.iter()
            .find(|(c, _)| *c == code)
            .map(|(_, n)| n.to_string())
    };
    let Some(name) = find(&MODIFIERS)
        .or_else(|| find(&NAMED))
        .or_else(|| find(&CHARS))
    else {
        return format!("Key {code}");
    };
    if cfg!(windows) {
        name.replace('⌥', "Alt")
            .replace('⌘', "Win")
            .replace('⌃', "Ctrl")
            .replace('⇧', "Shift")
    } else {
        name
    }
}

/// Modifiers offered in the shortcut menus. Right ⌥ belongs to the installed Swift Plaid on macOS dev builds.
pub(super) fn offered() -> Vec<i32> {
    let dev_mac = cfg!(target_os = "macos") && !config::release();
    MODIFIERS
        .iter()
        .map(|(c, _)| *c)
        .filter(|&c| !(dev_mac && c == 61))
        .collect()
}

/// Stores `holdKey`/`tapKey` (-1 clears) with its display name and rebinds the live hook.
pub(super) fn set_shortcut(model: &Entity<Model>, key: &str, code: i32, cx: &mut App) {
    model.update(cx, |m, cx| {
        m.set(key, code, cx);
        if code >= 0 {
            m.set(&format!("{key}Name"), key_name(code), cx);
        }
    });
    if let Some(l) = cx.try_global::<Live>() {
        l.keys.rebind();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shortcut {
    Hold,
    Tap,
}

impl Shortcut {
    fn key(self) -> &'static str {
        match self {
            Shortcut::Hold => "holdKey",
            Shortcut::Tap => "tapKey",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Shortcut::Hold => "hold",
            Shortcut::Tap => "tap",
        }
    }

    fn code(self) -> i32 {
        let (hold, tap) = config::hotkeys();
        match self {
            Shortcut::Hold => hold,
            Shortcut::Tap => tap,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Key(Shortcut),
}

/// A numeric setting edited as text with a stepper; stored as a string rounded to 10.
struct Number {
    key: &'static str,
    min: i64,
    max: i64,
    default: i64,
    input: Entity<InputState>,
}

impl Number {
    fn value(&self) -> i64 {
        config::value(self.key)
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| v as i64)
            .unwrap_or(self.default)
    }

    fn clamp(&self, v: i64) -> i64 {
        ((v.clamp(self.min, self.max) + 5) / 10) * 10
    }
}

pub struct General {
    model: Entity<Model>,
    /// The 声音 page; otherwise 设置.
    voice: bool,
    key: Entity<KeyField>,
    menu: Option<Menu>,
    recording: Option<Shortcut>,
    advanced: bool,
    silence: Number,
    tail: Number,
}

impl General {
    pub fn new(
        model: Entity<Model>,
        voice: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> General {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let m = model.clone();
        let key = cx.new(|cx| KeyField::new(m, || "apiKey".into(), window, cx));
        let silence = Self::number("silenceRMS", 0, 1500, SILENCE_DEFAULT as i64, window, cx);
        let tail = Self::number("tailMs", 100, 1000, 300, window, cx);
        General {
            model,
            voice,
            key,
            menu: None,
            recording: None,
            advanced: false,
            silence,
            tail,
        }
    }

    fn number(
        key: &'static str,
        min: i64,
        max: i64,
        default: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Number {
        let n = Number {
            key,
            min,
            max,
            default,
            input: cx.new(|cx| InputState::new(window, cx)),
        };
        let v = n.value().to_string();
        n.input.update(cx, |i, cx| i.set_value(v, window, cx));
        cx.subscribe_in(
            &n.input,
            window,
            move |this, _, e: &InputEvent, window, cx| {
                if matches!(e, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit(key, None, window, cx);
                }
            },
        )
        .detach();
        n
    }

    fn numbers(&self, key: &str) -> &Number {
        if key == self.silence.key {
            &self.silence
        } else {
            &self.tail
        }
    }

    /// Stores the typed value (or the current one moved by `step` × 10), clamped and rounded, and shows it.
    fn commit(
        &mut self,
        key: &str,
        step: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let n = self.numbers(key);
        let typed = n
            .input
            .read(cx)
            .value()
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| v as i64);
        let v = match step {
            Some(s) => n.clamp(n.value() + s * 10),
            None => typed.map(|v| n.clamp(v)).unwrap_or_else(|| n.value()),
        };
        let input = n.input.clone();
        let key = n.key;
        input.update(cx, |i, cx| i.set_value(v.to_string(), window, cx));
        self.model.update(cx, |m, cx| m.set(key, v.to_string(), cx));
    }

    fn set_shortcut(&mut self, s: Shortcut, code: i32, cx: &mut Context<Self>) {
        set_shortcut(&self.model, s.key(), code, cx);
    }

    fn record(&mut self, s: Shortcut, cx: &mut Context<Self>) {
        let Some(keys) = cx.try_global::<Live>().map(|l| l.keys.clone()) else {
            return;
        };
        if self.recording == Some(s) {
            keys.stop_recording();
            self.recording = None;
            cx.notify();
            return;
        }
        let (tx, rx) = async_channel::bounded(1);
        keys.record(tx);
        self.recording = Some(s);
        self.menu = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let got = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                // A newer recorder replaced this one: its own task owns `recording`.
                if this.recording != Some(s) {
                    return;
                }
                this.recording = None;
                if let Ok(code) = got
                    && code >= 0
                {
                    this.set_shortcut(s, code, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn shortcut_row(&self, s: Shortcut, label: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let code = s.code();
        let current = if code < 0 {
            "未绑定".to_string()
        } else {
            key_name(code)
        };
        let mut codes = offered();
        if code >= 0 && !codes.contains(&code) {
            codes.push(code);
        }
        let mut items: Vec<MenuEntry> = codes
            .iter()
            .map(|&c| MenuEntry {
                label: key_name(c).into(),
                checked: c == code,
                rule: false,
            })
            .collect();
        items.push(MenuEntry {
            label: "未绑定".into(),
            checked: code < 0,
            rule: false,
        });
        let open = self.menu == Some(Menu::Key(s));
        let this = cx.entity().downgrade();
        let (t1, t2, t3) = (this.clone(), this.clone(), this);
        let list = pick_list(
            s.id(),
            150.,
            current,
            items,
            open,
            move |_, cx| {
                let _ = t1.update(cx, |this, cx| {
                    this.menu = if open { None } else { Some(Menu::Key(s)) };
                    cx.notify();
                });
            },
            move |_, cx| {
                let _ = t2.update(cx, |this, cx| {
                    this.menu = None;
                    cx.notify();
                });
            },
            move |i, _, cx| {
                let pick = codes.get(i).copied().unwrap_or(-1);
                let _ = t3.update(cx, |this, cx| {
                    this.menu = None;
                    this.set_shortcut(s, pick, cx);
                    cx.notify();
                });
            },
        );
        let rec_id = SharedString::from(format!("{}-record", s.id()));
        let on_rec = cx.listener(move |this, _, _, cx| this.record(s, cx));
        let rec = if self.recording == Some(s) {
            default_button(rec_id, "请按一个键…", on_rec)
        } else {
            button(rec_id, "录制…", on_rec)
        };
        row(
            label,
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(list)
                .child(rec),
        )
        .into_any_element()
    }

    /// Input devices as a radio list: follow the system, each connected device, and a chosen one that is gone.
    fn mic_list(&self, cx: &mut Context<Self>) -> Div {
        let facts = &self.model.read(cx).facts;
        let chosen = config::value("micDevice");
        let mut items: Vec<(Option<String>, String, String, bool)> = vec![(
            None,
            "跟随系统".into(),
            format!("当前 {}", facts.default_mic.as_deref().unwrap_or("无")),
            chosen.is_none(),
        )];
        for (id, name) in &facts.mics {
            let on = chosen.as_deref() == Some(id.as_str());
            items.push((Some(id.clone()), name.clone(), String::new(), on));
        }
        if let Some(c) = chosen
            .as_ref()
            .filter(|c| !facts.mics.iter().any(|(id, _)| id == *c))
        {
            items.push((
                Some(c.clone()),
                "已选设备".into(),
                "未连接，暂用系统默认".into(),
                true,
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .children(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, (id, name, sub, on))| {
                        let model = self.model.clone();
                        radio(
                            SharedString::from(format!("mic-{i}")),
                            on,
                            name,
                            move |_, cx| {
                                let id = id.clone().unwrap_or_default();
                                model.update(cx, |m, cx| m.set("micDevice", id, cx));
                            },
                        )
                        .gap(px(12.))
                        .h(px(36.))
                        .px(px(10.))
                        .rounded(px(10.))
                        .map(|d| {
                            if on {
                                d.bg(hex(RAISED))
                            } else {
                                d.hover(|s| s.bg(hexa(0xffffff08)))
                            }
                        })
                        .child(div().text_size(px(12.)).text_color(hex(TEXT2)).child(sub))
                    }),
            )
    }

    /// The last utterance's speech level on a 48-cell meter with the silence threshold marked, plus warnings.
    /// Returns the card's status note and the meter column.
    fn level(&self, cx: &App) -> (String, Div) {
        const CELLS: usize = 48;
        let m = self.model.read(cx);
        let last = m.store.all.iter().rev().find(|r| r.speech_rms.is_some());
        let silence = self.silence.value();
        let speech = last.and_then(|r| r.speech_rms).unwrap_or(0);
        let lit = (level(speech as f64) * CELLS as f64).round() as usize;
        let mark = ((level(silence as f64) * CELLS as f64).round() as usize).min(CELLS - 1);
        let cells = div()
            .flex()
            .items_end()
            .gap(px(3.))
            .h(px(26.))
            .children((0..CELLS).map(|i| {
                let c = if i >= 44 {
                    RED
                } else if i >= 38 {
                    AMBER
                } else {
                    CYAN
                };
                let cell = div().flex_1().rounded(px(2.));
                if i == mark {
                    cell.h(px(26.)).bg(hex(TEXT2))
                } else if i < lit {
                    cell.h(px(20.)).bg(hex(c))
                } else {
                    cell.h(px(14.)).bg(hex(RAISED2))
                }
            }));
        let scale = div()
            .relative()
            .h(px(16.))
            .font(mono())
            .text_size(px(10.))
            .text_color(hex(TEXT3))
            .child(div().absolute().left_0().child("60"))
            .child(
                div()
                    .absolute()
                    .left(relative(mark as f32 / CELLS as f32))
                    .text_color(hex(TEXT2))
                    .child(format!("↑ 静音阈值 {silence}")),
            )
            .child(div().absolute().right_0().child("3000 RMS"));
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(cells)
            .child(scale);
        let status = match last {
            None => {
                col = col.child(hint("说一句话后显示上一句的电平。"));
                "还没有录音".to_string()
            }
            Some(r) => {
                if speech < QUIET_RMS {
                    let mic = r
                        .mic
                        .clone()
                        .or_else(|| m.facts.default_mic.clone())
                        .unwrap_or_else(|| "当前设备".into());
                    col = col.child(
                        hint(format!(
                            "语音电平过低。{mic}：识别会漏字或整句为空。换一个输入设备，或调高该设备的输入音量。"
                        ))
                        .text_color(hex(AMBER)),
                    );
                } else if speech < silence {
                    col = col.child(
                        hint("上一句的语音低于静音阈值，收尾可能吞字：把阈值放在噪声和语音之间、靠近噪声。")
                            .text_color(hex(AMBER)),
                    );
                }
                match r.noise_rms {
                    Some(n) => format!("上一句 语音 {speech} · 噪声 {n}"),
                    None => format!("上一句 语音 {speech}"),
                }
            }
        };
        (status, col)
    }

    fn number_row(
        &self,
        label: &'static str,
        n: &Number,
        unit: &'static str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = n.key;
        let this = cx.entity().downgrade();
        row(
            label,
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(field(&n.input, 60., window, cx))
                .child(stepper(key, move |dir, w, cx| {
                    let _ = this.update(cx, |this, cx| this.commit(key, Some(dir), w, cx));
                }))
                .child(unit),
        )
        .into_any_element()
    }

    fn toggle(&self, key: &'static str, label: &'static str, sub: &'static str) -> AnyElement {
        let on = on_unless_zero(key);
        let model = self.model.clone();
        toggle_row(key, on, label, sub, move |v, _, cx| {
            model.update(cx, |m, cx| m.set(key, bool_str(v), cx));
        })
        .into_any_element()
    }
}

/// The HUD's four looks, drawn small: label, sample text, and the bottom light in the state's color.
fn hud_states() -> Div {
    let states = [
        ("在听", "我们明天下午四点", TEXT, CYAN, 0.7),
        ("润色中", "我们明天下午四点开会。", TEXT, VIOLET, 0.8),
        ("麦克风太小", "没有听到声音", AMBER, AMBER, 0.4),
        ("出错", "网络不可用，已保存录音", RED, RED, 0.4),
    ];
    div()
        .flex()
        .gap(px(10.))
        .children(states.into_iter().map(|(name, text, ink, light, w)| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().text_size(px(11.)).text_color(hex(TEXT2)).child(name))
                .child(
                    div()
                        .relative()
                        .h(px(64.))
                        .rounded(px(10.))
                        .bg(hex(WELL))
                        .border_1()
                        .border_color(hexa(LINE))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(px(8.))
                        .text_size(px(12.))
                        .text_color(hex(ink))
                        .child(div().truncate().child(text))
                        .child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left(relative((1. - w) / 2.))
                                .w(relative(w))
                                .h(px(3.))
                                .flex()
                                .shadow(vec![shadow(0., -2., 14., 0., (light << 8) | 0x99, false)])
                                .child(
                                    div()
                                        .flex_1()
                                        .h_full()
                                        .bg(hgrada(light << 8, (light << 8) | 0xff)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .h_full()
                                        .bg(hgrada((light << 8) | 0xff, light << 8)),
                                ),
                        ),
                )
        }))
}

/// Swift's level mapping: log scale between RMS 60 and 3000.
fn level(rms: f64) -> f64 {
    let (lo, hi) = (60f64.log10(), 3000f64.log10());
    ((rms.max(1.).log10() - lo) / (hi - lo)).clamp(0., 1.)
}

impl General {
    fn voice_page(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let (status, meter) = self.level(cx);
        let hold = Shortcut::Hold.code();
        let cap = if hold < 0 {
            "未绑定".to_string()
        } else {
            key_name(hold)
        };
        let mut asr = vec![
            card_head("识别", "火山引擎 bigmodel").into_any_element(),
            self.toggle(
                "asrContext",
                "发送上下文",
                "前台应用和输入框里光标前的文字，帮助识别专有名词",
            ),
            self.toggle(
                "nonstream",
                "二遍识别",
                "松键后用整段音频再识别一次，更准，约慢 300 ms",
            ),
            div()
                .pt(px(4.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(disclosure("advanced", self.advanced, "高级设置", {
                    let this = cx.entity().downgrade();
                    move |_, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.advanced = !this.advanced;
                            cx.notify();
                        });
                    }
                }))
                .child(hint("静音阈值、松键收尾"))
                .into_any_element(),
        ];
        if self.advanced {
            asr.push(self.number_row("静音阈值", &self.silence, "RMS", window, cx));
            asr.push(self.number_row("最长收尾", &self.tail, "ms", window, cx));
            asr.push(
                row(
                    "",
                    hint("松键后短暂保留录音，等到静音或时限再结束，避免吞掉最后一个字。太高会丢字，太低会变慢。"),
                )
                .into_any_element(),
            );
        }
        super::page_body("voice")
            .child(
                group_body()
                    .child(
                        card_head("麦克风", "仅影响 Plaid，不更改系统输入设备").child(
                            div()
                                .font(mono())
                                .text_size(px(11.))
                                .text_color(hex(TEXT2))
                                .child(status),
                        ),
                    )
                    .child(self.mic_list(cx))
                    .child(div().pt(px(6.)).child(meter)),
            )
            .child(
                group_body()
                    .child(card_head("说话键", "仅单独按下时触发；两者可以是同一个键"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(28.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        keycap(cap)
                                            .h(px(64.))
                                            .min_w(px(96.))
                                            .px(px(18.))
                                            .rounded(px(12.))
                                            .text_size(px(20.))
                                            .shadow(vec![
                                                shadow(0., 3., 0., 0., 0x08090bff, false),
                                                glow(CYAN, 22., 0x22),
                                            ]),
                                    )
                                    .child(hint("按住说话")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(6.))
                                    .child(self.shortcut_row(Shortcut::Hold, "按住说话", cx))
                                    .child(self.shortcut_row(Shortcut::Tap, "轻点开关", cx))
                                    .child(row(
                                        "",
                                        hint("按住 300 ms 开始、松开粘贴；轻点开始，再点一下或 ⏎ 结束，esc 取消。"),
                                    )),
                            ),
                    ),
            )
            .child(group_body().children(asr))
            .child(
                group_body()
                    .child(card_head("屏幕底部光带", "说话时出现在当前屏幕底部"))
                    .child(hud_states())
                    .child(div().h(px(6.)))
                    .children(self.hud_options()),
            )
    }

    fn hud_options(&self) -> Vec<AnyElement> {
        let opts = crate::hud::Opts::load();
        let flag = |key: &'static str, on: bool, label, sub| {
            let model = self.model.clone();
            toggle_row(key, on, label, sub, move |v, _, cx| {
                model.update(cx, |m, cx| m.set(key, bool_str(v), cx));
            })
            .into_any_element()
        };
        let reveal = {
            let model = self.model.clone();
            use crate::hud::Reveal;
            let picked = match opts.reveal {
                Reveal::Pace => 0,
                Reveal::Fast => 1,
                Reveal::Instant => 2,
            };
            segmented(
                "hudReveal",
                &["跟着说话", "快", "立即"],
                picked,
                move |i, _, cx| {
                    let v = ["pace", "fast", "instant"][i.min(2)];
                    model.update(cx, |m, cx| m.set("hudReveal", v, cx));
                },
            )
        };
        let linger = {
            let model = self.model.clone();
            use crate::hud::LINGER;
            let picked = LINGER.iter().position(|&s| s == opts.linger).unwrap_or(1);
            segmented(
                "hudLinger",
                &["0.4 s", "0.7 s", "1.5 s"],
                picked,
                move |i, _, cx| {
                    let v = LINGER[i.min(LINGER.len() - 1)].to_string();
                    model.update(cx, |m, cx| m.set("hudLinger", v, cx));
                },
            )
        };
        let mut rows = vec![
            flag(
                "hudText",
                opts.text,
                "显示文字",
                "关掉后说话时只留光带；提示和出错仍显示文字",
            ),
            option_row("文字速度", "跟着说话的节奏逐字出现，或者更快", reveal).into_any_element(),
            option_row("松开后停留", "粘贴完成后文字停留多久再淡出", linger).into_any_element(),
            flag(
                "hudSound",
                opts.sound,
                "声音提示",
                "开始说话和粘贴时轻响一下",
            ),
        ];
        if cfg!(target_os = "macos") {
            rows.insert(
                1,
                flag(
                    "hudMouseScreen",
                    on_if_one("hudMouseScreen"),
                    "跟随鼠标所在屏幕",
                    "多屏时出现在鼠标所在的屏幕；默认跟随正在输入的窗口",
                ),
            );
        }
        rows
    }

    fn general_page(&self) -> Stateful<Div> {
        let reveal_label = if cfg!(windows) {
            "在资源管理器中显示"
        } else {
            "在 Finder 中显示"
        };
        super::page_body("general")
            .child(
                group_body()
                    .child(card_head("火山引擎", "语音识别服务；key 只存在本机"))
                    .child(row("API Key", self.key.clone())),
            )
            .child(
                group_body()
                    .child(card_head("数据", "全部只存本机"))
                    .child(row(
                        "数据文件夹",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .font(mono())
                                    .text_size(px(12.))
                                    .child(data_dir_label()),
                            )
                            .child(button("reveal", reveal_label, |_, _, _| {
                                reveal(&config::data_dir().join("records.jsonl"))
                            })),
                    ))
                    .child(self.toggle(
                        "keepAudio",
                        "保留最近 500 条录音",
                        "只存本机，用于回放和反馈问题",
                    ))
                    .child(div().pt(px(6.)).child(hint(
                        "云端识别会发送音频及已启用的上下文；LLM 润色和后台校对可能发送识别文本、热词及上下文。后台校对独立于润色开关，可在「词典」中管理。",
                    ))),
            )
    }
}

impl Render for General {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.voice {
            self.voice_page(window, cx)
        } else {
            self.general_page()
        }
    }
}
