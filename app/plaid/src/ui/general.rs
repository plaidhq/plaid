//! 设置: shortcuts, microphone, Volcengine ASR, data.
use super::key_field::KeyField;
use super::mav::*;
use super::{Live, Model, bool_str, data_dir_label, field, on_unless_zero, pick_list, reveal};
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
fn offered() -> Vec<i32> {
    let dev_mac = cfg!(target_os = "macos") && !config::release();
    MODIFIERS
        .iter()
        .map(|(c, _)| *c)
        .filter(|&c| !(dev_mac && c == 61))
        .collect()
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
    Mic,
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
    key: Entity<KeyField>,
    menu: Option<Menu>,
    recording: Option<Shortcut>,
    advanced: bool,
    silence: Number,
    tail: Number,
}

impl General {
    pub fn new(model: Entity<Model>, window: &mut Window, cx: &mut Context<Self>) -> General {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let m = model.clone();
        let key = cx.new(|cx| KeyField::new(m, || "apiKey".into(), window, cx));
        let silence = Self::number("silenceRMS", 0, 1500, SILENCE_DEFAULT as i64, window, cx);
        let tail = Self::number("tailMs", 100, 1000, 300, window, cx);
        General {
            model,
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
        self.model.update(cx, |m, cx| {
            m.set(s.key(), code, cx);
            if code >= 0 {
                m.set(&format!("{}Name", s.key()), key_name(code), cx);
            }
        });
        if let Some(l) = cx.try_global::<Live>() {
            l.keys.rebind();
        }
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
            })
            .collect();
        items.push(MenuEntry {
            label: "未绑定".into(),
            checked: code < 0,
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

    fn mic_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let facts = &self.model.read(cx).facts;
        let chosen = config::value("micDevice");
        let system = format!(
            "跟随系统（{}）",
            facts.default_mic.as_deref().unwrap_or("无")
        );
        let mut ids: Vec<Option<String>> = vec![None];
        let mut items = vec![MenuEntry {
            label: system.clone().into(),
            checked: chosen.is_none(),
        }];
        for (id, name) in &facts.mics {
            ids.push(Some(id.clone()));
            items.push(MenuEntry {
                label: name.clone().into(),
                checked: chosen.as_deref() == Some(id.as_str()),
            });
        }
        let missing = chosen
            .as_ref()
            .is_some_and(|c| !facts.mics.iter().any(|(id, _)| id == c));
        if missing {
            ids.push(chosen.clone());
            items.push(MenuEntry {
                label: "已选设备未连接，暂用系统默认".into(),
                checked: true,
            });
        }
        let current = items
            .iter()
            .find(|i| i.checked)
            .map(|i| i.label.clone())
            .unwrap_or(system.into());
        let open = self.menu == Some(Menu::Mic);
        let this = cx.entity().downgrade();
        let (t1, t2, t3) = (this.clone(), this.clone(), this);
        row(
            "输入设备：",
            pick_list(
                "mic",
                300.,
                current,
                items,
                open,
                move |_, cx| {
                    let _ = t1.update(cx, |this, cx| {
                        this.menu = if open { None } else { Some(Menu::Mic) };
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
                    let id = ids.get(i).cloned().flatten();
                    let _ = t3.update(cx, |this, cx| {
                        this.menu = None;
                        this.model
                            .update(cx, |m, cx| m.set("micDevice", id.unwrap_or_default(), cx));
                    });
                },
            ),
        )
        .into_any_element()
    }

    /// The last utterance's levels: a 16-cell meter of the speech level and what the numbers mean.
    fn level_rows(&self, cx: &App) -> Vec<AnyElement> {
        let m = self.model.read(cx);
        let last = m.store.all.iter().rev().find(|r| r.speech_rms.is_some());
        let Some(r) = last else {
            return vec![
                row("输入电平：", meter(0, 16)).into_any_element(),
                row("", hint("说一句话后显示上一句的电平。")).into_any_element(),
            ];
        };
        let speech = r.speech_rms.unwrap_or(0);
        let lit = (level(speech as f64) * 16.).round() as usize;
        let silence = self.silence.value();
        let mut rows = vec![row("输入电平：", meter(lit, 16)).into_any_element()];
        if let Some(noise) = r.noise_rms {
            rows.push(
                row(
                    "",
                    hint(format!(
                        "上一句噪声约 {}，语音约 {}（RMS）；静音阈值放在两者之间、靠近噪声。",
                        noise, speech
                    ))
                    .when(speech < silence, |d| d.text_color(hex(0xb35c00))),
                )
                .into_any_element(),
            );
        }
        if speech < QUIET_RMS {
            let mic = r
                .mic
                .clone()
                .or_else(|| m.facts.default_mic.clone())
                .unwrap_or_else(|| "当前设备".into());
            rows.push(
                row(
                    "",
                    hint(format!(
                        "语音电平过低。{mic}：识别会漏字或整句为空。换一个输入设备，或调高该设备的输入音量。"
                    ))
                    .text_color(hex(0xb35c00)),
                )
                .into_any_element(),
            );
        }
        rows
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

    fn check(&self, key: &'static str, label: &'static str) -> AnyElement {
        let on = on_unless_zero(key);
        let model = self.model.clone();
        row(
            "",
            checkbox(key, on, label, move |v, _, cx| {
                model.update(cx, |m, cx| m.set(key, bool_str(v), cx));
            }),
        )
        .into_any_element()
    }
}

/// Swift's level mapping: log scale between RMS 60 and 3000.
fn level(rms: f64) -> f64 {
    let (lo, hi) = (60f64.log10(), 3000f64.log10());
    ((rms.max(1.).log10() - lo) / (hi - lo)).clamp(0., 1.)
}

impl Render for General {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reveal_label = if cfg!(windows) {
            "在资源管理器中显示"
        } else {
            "在 Finder 中显示"
        };
        let mut asr = vec![
            row("API Key：", self.key.clone()).into_any_element(),
            self.check("asrContext", "发送上下文（前台应用和输入框里光标前的文字）"),
            self.check(
                "nonstream",
                "二遍识别（松键后用整段音频再识别一次，约慢 300 ms）",
            ),
            row(
                "",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(disclosure("advanced", self.advanced, "高级设置", {
                        let this = cx.entity().downgrade();
                        move |_, cx| {
                            let _ = this.update(cx, |this, cx| {
                                this.advanced = !this.advanced;
                                cx.notify();
                            });
                        }
                    }))
                    .child(hint("静音阈值、松键收尾")),
            )
            .into_any_element(),
        ];
        if self.advanced {
            asr.push(self.number_row("静音阈值：", &self.silence, "RMS", window, cx));
            asr.push(row("", hint("松键后短暂保留录音，等到静音或时限再结束，避免吞掉最后一个字。太高会丢字，太低会变慢。")).into_any_element());
            asr.push(self.number_row("最长收尾：", &self.tail, "ms", window, cx));
        }
        let mut mic = vec![self.mic_row(cx)];
        mic.extend(self.level_rows(cx));
        mic.push(row("", hint("仅影响 Plaid，不更改系统输入设备。")).into_any_element());

        super::page_body("general")
            .child(section(
                "快捷键",
                [
                    self.shortcut_row(Shortcut::Hold, "按住说话：", cx),
                    self.shortcut_row(Shortcut::Tap, "轻点开关：", cx),
                    row(
                        "",
                        hint("仅单独按键触发。按住 300 ms 开始、松开粘贴；轻点开始，再点一下或 ⏎ 结束，esc 取消。两者可以是同一个键。"),
                    )
                    .into_any_element(),
                ],
            ))
            .child(section("麦克风", mic))
            .child(section("语音识别（火山引擎）", asr))
            .child(section(
                "数据",
                [
                    row(
                        "数据文件夹：",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .min_w_0()
                            .child(div().truncate().text_color(hex(0x333333)).child(data_dir_label()))
                            .child(button("reveal", reveal_label, |_, _, _| {
                                reveal(&config::data_dir().join("records.jsonl"))
                            })),
                    )
                    .into_any_element(),
                    self.check("keepAudio", "保留最近 500 条录音（只存本机，用于回放和反馈问题）"),
                    row(
                        "",
                        hint("云端识别会发送音频及已启用的上下文；LLM 优化和后台校对可能发送识别文本、热词及上下文。后台校对独立于优化开关，可在「词典」中管理。"),
                    )
                    .into_any_element(),
                ],
            ))
    }
}
