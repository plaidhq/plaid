//! 优化: the LLM pass between the ASR result and the paste (RefinePage.swift).
use super::key_field::KeyField;
use super::mav::*;
use super::{Model, bool_str, field, on_if_one, on_unless_zero, pick_list};
use crate::{config, secret};
use gpui_kit::base::input::{
    InputBaseState, InputEvent, InputModeKind, InputState, Textarea, TextareaState,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::pct;
use plaid_core::llm;
use std::time::{Duration, Instant};

/// Sets format and base URL only.
const PRESETS: [(&str, &str, &str); 6] = [
    (
        "Volcengine Ark",
        "openai",
        "https://ark.cn-beijing.volces.com/api/v3",
    ),
    ("OpenAI", "openai", "https://api.openai.com/v1"),
    ("Anthropic", "anthropic", "https://api.anthropic.com/v1"),
    ("DeepSeek", "openai", "https://api.deepseek.com"),
    ("OpenRouter", "openai", "https://openrouter.ai/api/v1"),
    ("Ollama (local)", "openai", "http://localhost:11434/v1"),
];

const FORMATS: [(&str, &str); 2] = [("openai", "OpenAI 兼容"), ("anthropic", "Anthropic")];

const SAMPLE: &str =
    "嗯那个我们周三，不对，周四下午开个会，就是说讨论一下 keep a life 和那个 CI 的问题";

const ORANGE: u32 = 0xb35c00;
const RED: u32 = 0xa3241d;

pub(super) fn mono() -> &'static str {
    if cfg!(windows) { "Consolas" } else { "Menlo" }
}

/// Form row for tall controls: the label sits on the control's first line instead of its middle.
pub(super) fn top_row(label: impl Into<SharedString>, control: impl IntoElement) -> Div {
    let label: SharedString = label.into();
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .child(
            div()
                .w(px(LABEL_W))
                .flex_none()
                .flex()
                .justify_end()
                .pt(px(3.))
                .text_color(hex(0x1a1a1a))
                .when(!label.is_empty(), |d| d.child(label)),
        )
        .child(div().flex_1().min_w_0().child(control))
}

/// Multi-line editor in the square text bezel.
pub(super) fn editor(
    state: &Entity<TextareaState>,
    mono_font: bool,
    window: &Window,
    cx: &App,
) -> Div {
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    field_frame(focused)
        .items_start()
        .px(px(5.))
        .py(px(3.))
        .when(mono_font, |d| d.font_family(mono()).text_size(px(12.)))
        .child(div().flex_1().min_w_0().child(Textarea::new(state)))
}

/// Text field that takes the row's remaining width.
pub(super) fn wide_field(state: &Entity<InputState>, window: &Window, cx: &App) -> Div {
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    field_frame(focused)
        .flex_1()
        .min_w_0()
        .h(px(22.))
        .px(px(5.))
        .child(div().flex_1().min_w_0().child(Input::new(state)))
}

use gpui_kit::base::input::Input;

/// Orange "已修改" capsule.
fn pill(text: &'static str) -> Div {
    div()
        .px(px(6.))
        .rounded_full()
        .text_size(px(10.))
        .line_height(px(15.))
        .text_color(hex(ORANGE))
        .bg(hexa(0xff950026))
        .child(text)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Preset,
    Format,
}

/// A prompt editor with its default (system prompt, user message template).
struct Prompt {
    key: &'static str,
    id: &'static str,
    title: &'static str,
    default: &'static str,
    input: Entity<TextareaState>,
    open: bool,
}

#[derive(Default)]
struct Test {
    running: bool,
    ms: Option<u128>,
    /// (text, failed)
    result: Option<(String, bool)>,
}

pub struct Refine {
    model: Entity<Model>,
    key: Entity<KeyField>,
    menu: Option<Menu>,
    base: Entity<InputState>,
    llm_model: Entity<InputState>,
    extra: Entity<TextareaState>,
    timeout: Entity<InputState>,
    context: Entity<TextareaState>,
    prompts: [Prompt; 2],
    sample: Entity<TextareaState>,
    test: Test,
}

impl Refine {
    pub fn new(model: Entity<Model>, window: &mut Window, cx: &mut Context<Self>) -> Refine {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let m = model.clone();
        let key = cx.new(|cx| {
            KeyField::new(
                m,
                || llm::key_account(&config::value("llmBaseUrl").unwrap_or_default()),
                window,
                cx,
            )
        });
        let raw = |k: &str| config::raw(k).unwrap_or_default();
        let base = Self::line("llmBaseUrl", &raw("llmBaseUrl"), PRESETS[0].2, window, cx);
        let llm_model = Self::line("llmModel", &raw("llmModel"), "模型或接入点 ID", window, cx);
        let extra = Self::area(
            "llmExtraBody",
            &raw("llmExtraBody"),
            r#"{"thinking":{"type":"disabled"}}"#,
            (1, 6),
            window,
            cx,
        );
        let context = Self::area(
            "llmContext",
            &raw("llmContext"),
            "例如：后端工程师，主要写 Go 和 TypeScript；常提到的项目有……",
            (4, 12),
            window,
            cx,
        );
        let timeout = cx.new(|cx| InputState::new(window, cx));
        let ms = timeout_ms().to_string();
        timeout.update(cx, |i, cx| i.set_value(ms, window, cx));
        cx.subscribe_in(&timeout, window, |this, _, e: &InputEvent, window, cx| {
            if matches!(e, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.commit_timeout(window, cx);
            }
        })
        .detach();
        let prompt =
            |key, id, title, default, rows, window: &mut Window, cx: &mut Context<Self>| {
                let v = config::raw(key).unwrap_or_else(|| String::from(default));
                Prompt {
                    key,
                    id,
                    title,
                    default,
                    input: Self::area(key, &v, "", rows, window, cx),
                    open: false,
                }
            };
        let prompts = [
            prompt(
                "llmPrompt",
                "prompt",
                "系统提示词",
                llm::DEFAULT_PROMPT,
                (12, 30),
                window,
                cx,
            ),
            prompt(
                "llmTemplate",
                "template",
                "用户消息",
                llm::DEFAULT_TEMPLATE,
                (8, 20),
                window,
                cx,
            ),
        ];
        let sample = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6));
        sample.update(cx, |i, cx| i.set_value(SAMPLE, window, cx));
        Refine {
            model,
            key,
            menu: None,
            base,
            llm_model,
            extra,
            timeout,
            context,
            prompts,
            sample,
            test: Test::default(),
        }
    }

    /// Single-line field written to `key` on every edit (as the Swift page's bindings).
    fn line(
        key: &'static str,
        value: &str,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |i, cx| i.set_value(value.to_string(), window, cx));
        Self::live(&input, key, window, cx);
        input
    }

    fn area(
        key: &'static str,
        value: &str,
        placeholder: &'static str,
        (min, max): (usize, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextareaState> {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(min, max)
                .placeholder(placeholder)
        });
        input.update(cx, |i, cx| i.set_value(value.to_string(), window, cx));
        Self::live(&input, key, window, cx);
        input
    }

    /// User edits go straight to settings.json; programmatic `set_value` emits no Change.
    fn live<M: InputModeKind>(
        input: &Entity<InputBaseState<M>>,
        key: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(input, window, move |this, input, e: &InputEvent, _, cx| {
            if let InputEvent::Change = e {
                let v = input.read(cx).value().to_string();
                this.model.update(cx, |m, cx| m.set(key, v, cx));
            }
            cx.notify();
        })
        .detach();
    }

    fn commit_timeout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.timeout.read(cx).value().trim().parse::<u64>().ok();
        let v = typed.unwrap_or_else(timeout_ms);
        self.timeout
            .update(cx, |i, cx| i.set_value(v.to_string(), window, cx));
        if typed.is_some() {
            self.model
                .update(cx, |m, cx| m.set("llmTimeoutMs", v.to_string(), cx));
        }
    }

    fn pick_preset(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(&(_, format, url)) = PRESETS.get(i) else {
            return;
        };
        self.menu = None;
        self.base.update(cx, |b, cx| b.set_value(url, window, cx));
        self.model.update(cx, |m, cx| {
            m.set("llmFormat", format, cx);
            m.set("llmBaseUrl", url, cx);
        });
    }

    fn reset_prompt(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let p = &self.prompts[i];
        if p.input.read(cx).value() == p.default {
            return;
        }
        let (key, default, input) = (p.key, p.default, p.input.clone());
        input.update(cx, |t, cx| t.set_value(default, window, cx));
        self.model
            .update(cx, |m, cx| m.set(key, serde_json::Value::Null, cx));
    }

    /// One request through the configured endpoint with the sample text (Refine.run with app "Plaid 设置" and no
    /// history). Snapshot runs stop before the keychain and the network.
    fn run_test(&mut self, cx: &mut Context<Self>) {
        if self.test.running {
            return;
        }
        let t0 = Instant::now();
        let text = self.sample.read(cx).value().to_string();
        let fixture = self.model.read(cx).facts.fixture;
        let req = self.request(&text, fixture, cx);
        self.test = Test {
            running: true,
            ..Test::default()
        };
        cx.notify();
        let req = match req {
            Ok(r) if !fixture => r,
            Ok(_) => return self.finish(Err("snapshot run: not sent".into()), t0, cx),
            Err(e) => return self.finish(Err(e), t0, cx),
        };
        #[cfg(any(target_os = "macos", windows))]
        let proxy = crate::os::proxy();
        #[cfg(not(any(target_os = "macos", windows)))]
        let proxy: Option<llm::SystemProxy> = None;
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move { llm::send(&req, proxy.as_ref()).map_err(|f| f.message) })
                .await;
            let _ = this.update(cx, |this, cx| this.finish(r, t0, cx));
        })
        .detach();
    }

    fn finish(&mut self, r: Result<String, String>, t0: Instant, cx: &mut Context<Self>) {
        self.test = Test {
            running: false,
            ms: Some(t0.elapsed().as_millis()),
            result: Some(match r {
                Ok(t) => (t, false),
                Err(e) => (format!("失败：{e}"), true),
            }),
        };
        cx.notify();
    }

    /// The controller's request (`llm::refine_request`), built from the current settings.
    fn request(&self, text: &str, fixture: bool, cx: &App) -> Result<llm::Request, String> {
        let ep = llm::endpoint(config::value);
        let key = (!fixture && !ep.base.is_empty())
            .then(|| secret::llm_key(&ep.base))
            .flatten();
        let fill = llm::Fill {
            text,
            app: "Plaid 设置",
            history: &[],
            hotwords: &config::terms(true).join(", "),
            fixes: &self.model.read(cx).store.fixes(),
        };
        llm::refine_request(
            config::value,
            &ep,
            key.as_deref(),
            &fill,
            Duration::from_millis(timeout_ms()),
        )
    }

    fn status(&self, cx: &App) -> String {
        if !on_if_one("llmEnabled") {
            return "关闭时识别结果原样粘贴".into();
        }
        let s = "出错或超时会直接粘贴原文；优化中按 esc 跳过";
        let all = &self.model.read(cx).store.all;
        let ms: Vec<i64> = all[all.len().saturating_sub(50)..]
            .iter()
            .filter(|r| r.llm == "ok")
            .filter_map(|r| r.llm_ms)
            .collect();
        match pct(&ms, 0.5) {
            Some(p) => format!("{s} · 最近 50 句中位 {p} ms"),
            None => s.into(),
        }
    }

    fn menu_toggle(
        &self,
        which: Menu,
        open: bool,
        cx: &mut Context<Self>,
    ) -> impl Fn(&mut Window, &mut App) + 'static {
        let this = cx.entity().downgrade();
        move |_, cx| {
            let _ = this.update(cx, |this, cx| {
                this.menu = if open { None } else { Some(which) };
                cx.notify();
            });
        }
    }

    fn menu_close(&self, cx: &mut Context<Self>) -> impl Fn(&mut Window, &mut App) + 'static {
        let this = cx.entity().downgrade();
        move |_, cx| {
            let _ = this.update(cx, |this, cx| {
                this.menu = None;
                cx.notify();
            });
        }
    }

    fn provider_rows(&self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let format = config::value("llmFormat").unwrap_or_else(|| "openai".into());
        let open = self.menu == Some(Menu::Preset);
        let this = cx.entity().downgrade();
        let presets = pick_list(
            "preset",
            170.,
            "选择预设…",
            PRESETS
                .iter()
                .map(|(name, _, _)| MenuEntry {
                    label: (*name).into(),
                    checked: false,
                    rule: false,
                })
                .collect(),
            open,
            self.menu_toggle(Menu::Preset, open, cx),
            self.menu_close(cx),
            move |i, w, cx| {
                let _ = this.update(cx, |this, cx| this.pick_preset(i, w, cx));
            },
        );
        let open = self.menu == Some(Menu::Format);
        let this = cx.entity().downgrade();
        let formats = pick_list(
            "llmFormat",
            170.,
            FORMATS
                .iter()
                .find(|(v, _)| *v == format)
                .map_or(format.clone(), |(_, l)| (*l).into()),
            FORMATS
                .iter()
                .map(|(v, l)| MenuEntry {
                    label: (*l).into(),
                    checked: *v == format,
                    rule: false,
                })
                .collect(),
            open,
            self.menu_toggle(Menu::Format, open, cx),
            self.menu_close(cx),
            move |i, _, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.menu = None;
                    this.model
                        .update(cx, |m, cx| m.set("llmFormat", FORMATS[i].0, cx));
                });
            },
        );
        let extra = self.extra.read(cx).value();
        let extra_ok = extra.trim().is_empty()
            || matches!(
                serde_json::from_str::<serde_json::Value>(&extra),
                Ok(serde_json::Value::Object(_))
            );
        vec![
            row("预设：", presets).into_any_element(),
            row("接口格式：", formats).into_any_element(),
            row(
                "接口地址：",
                hit("llmBaseUrl")
                    .w(px(300.))
                    .child(field(&self.base, 300., window, cx)),
            )
            .into_any_element(),
            row(
                "模型：",
                hit("llmModel")
                    .w(px(300.))
                    .child(field(&self.llm_model, 300., window, cx)),
            )
            .into_any_element(),
            row("API key：", self.key.clone()).into_any_element(),
            row("", hint("按接口地址的域名分别保存")).into_any_element(),
            top_row(
                "额外请求体：",
                hit("llmExtraBody")
                    .w(px(300.))
                    .child(editor(&self.extra, true, window, cx)),
            )
            .into_any_element(),
            row(
                "",
                if extra_ok {
                    hint("合并进每次请求的 JSON 对象，例如关闭思考")
                } else {
                    hint("不是 JSON 对象").text_color(hex(RED))
                },
            )
            .into_any_element(),
            row(
                "超时：",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(hit("llmTimeoutMs").child(field(&self.timeout, 70., window, cx)))
                    .child("ms"),
            )
            .into_any_element(),
            row("", hint("超过即粘贴原文")).into_any_element(),
        ]
    }

    fn context_rows(&self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let history = history();
        let model = self.model.clone();
        let app_context = on_unless_zero("llmAppContext");
        vec![
            top_row(
                "关于你：",
                hit("llmContext").child(editor(&self.context, false, window, cx)),
            )
            .into_any_element(),
            row(
                "",
                hint("角色、项目、人名、偏好的文风。热词会另外作为词表发送，不用重复。"),
            )
            .into_any_element(),
            row(
                "",
                checkbox(
                    "llmAppContext",
                    app_context,
                    "发送前台应用和窗口标题",
                    move |v, _, cx| {
                        model.update(cx, |m, cx| m.set("llmAppContext", bool_str(v), cx))
                    },
                ),
            )
            .into_any_element(),
            row(
                "",
                hint("让模型按应用调整格式，例如聊天保持口语、邮件用书面句子"),
            )
            .into_any_element(),
            row(
                "历史句数：",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().min_w(px(34.)).child(format!("{history} 句")))
                    .child(stepper("llmHistory", {
                        let model = self.model.clone();
                        move |dir, _, cx| {
                            let v = (history as i64 + dir).clamp(0, 10);
                            model.update(cx, |m, cx| m.set("llmHistory", v.to_string(), cx));
                        }
                    })),
            )
            .into_any_element(),
            row("", hint("10 分钟内的最近几句作为前文，只用来理解语境")).into_any_element(),
        ]
    }

    fn prompt_rows(&self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = vec![];
        for (i, p) in self.prompts.iter().enumerate() {
            let modified = p.input.read(cx).value() != p.default;
            let this = cx.entity().downgrade();
            rows.push(
                row(
                    "",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(disclosure(
                            SharedString::from(format!("{}-open", p.id)),
                            p.open,
                            p.title,
                            move |_, cx| {
                                let _ = this.update(cx, |this, cx| {
                                    this.prompts[i].open = !this.prompts[i].open;
                                    cx.notify();
                                });
                            },
                        ))
                        .when(modified, |d| d.child(pill("已修改"))),
                )
                .into_any_element(),
            );
            if p.open {
                rows.push(
                    top_row(
                        "",
                        hit(SharedString::from(p.id)).child(editor(&p.input, true, window, cx)),
                    )
                    .into_any_element(),
                );
                let reset = button(
                    SharedString::from(format!("{}-reset", p.id)),
                    "恢复默认",
                    cx.listener(move |this, _, w, cx| this.reset_prompt(i, w, cx)),
                );
                rows.push(
                    row("", if modified { reset } else { disabled(reset) }).into_any_element(),
                );
            }
        }
        rows.push(
            row(
                "",
                hint(
                    "占位符：{{text}} {{context}} {{hotwords}} {{corrections}} {{app}} {{history}}",
                ),
            )
            .into_any_element(),
        );
        rows
    }

    fn test_rows(&self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let run = button(
            "refine-run",
            if self.test.running {
                "运行中…"
            } else {
                "运行"
            },
            cx.listener(|this, _, _, cx| this.run_test(cx)),
        );
        let mut rows = vec![
            top_row(
                "示例文本：",
                hit("refine-sample").child(editor(&self.sample, false, window, cx)),
            )
            .into_any_element(),
            row(
                "",
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(if self.test.running {
                        disabled(run)
                    } else {
                        run
                    })
                    .when_some(self.test.ms, |d, ms| d.child(hint(format!("{ms} ms")))),
            )
            .into_any_element(),
        ];
        if let Some((text, failed)) = &self.test.result {
            rows.push(
                top_row(
                    "",
                    hit("refine-result")
                        .px(px(6.))
                        .py(px(4.))
                        .bg(hex(0xffffff))
                        .border_1()
                        .border_color(hex(0xc4c4c4))
                        .text_color(hex(if *failed { RED } else { 0x1a1a1a }))
                        .child(text.clone()),
                )
                .into_any_element(),
            );
        }
        rows
    }
}

fn timeout_ms() -> u64 {
    config::value("llmTimeoutMs")
        .and_then(|s| s.parse().ok())
        .unwrap_or(4000)
}

fn history() -> usize {
    config::value("llmHistory")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(3)
        .min(10)
}

impl Render for Refine {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = on_if_one("llmEnabled");
        let model = self.model.clone();
        super::page_body("refine")
            .child(hint(
                "粘贴前用 LLM 整理识别结果：去口癖、修正自我纠正、统一术语",
            ))
            .child(section(
                "文本处理",
                [
                    row(
                        "",
                        checkbox("llmEnabled", enabled, "粘贴前用 LLM 优化", move |v, _, cx| {
                            model.update(cx, |m, cx| m.set("llmEnabled", bool_str(v), cx))
                        }),
                    )
                    .into_any_element(),
                    row("", hint(self.status(cx))).into_any_element(),
                    row(
                        "",
                        hint("启用后，识别文本及已配置的上下文会发送给所选 LLM 服务；使用远程接口时会离开本机。「词典 → 后台校对」独立运行，关闭此开关不会关闭后台校对。"),
                    )
                    .into_any_element(),
                ],
            ))
            .child(section("服务商", self.provider_rows(window, cx)))
            .child(section("上下文", self.context_rows(window, cx)))
            .child(section("提示词", self.prompt_rows(window, cx)))
            .child(section("测试", self.test_rows(window, cx)))
    }
}

#[cfg(all(feature = "snapshot", any(target_os = "macos", target_os = "linux")))]
pub(super) fn check(ui: &mut super::snapshot::Ui) -> Result<(), String> {
    use super::Page;
    use super::snapshot::{expect, facts};
    ui.open(Page::Refine, facts())?;
    ui.shot("refine")?;

    let before = config::raw("llmEnabled");
    ui.click("llmEnabled")?;
    expect(
        "llmEnabled off",
        config::raw("llmEnabled").as_deref() == Some("0"),
        &before,
    )?;
    ui.shot("refine-off")?;
    ui.click("llmEnabled")?;
    expect(
        "llmEnabled on",
        config::raw("llmEnabled").as_deref() == Some("1"),
        &None,
    )?;

    ui.click("llmFormat")?;
    ui.click("llmFormat-menu-1")?;
    expect(
        "format picked",
        config::raw("llmFormat").as_deref() == Some("anthropic"),
        &None,
    )?;

    ui.click("preset")?;
    ui.shot("refine-presets")?;
    ui.click("preset-menu-3")?;
    expect(
        "preset sets format and url",
        config::raw("llmFormat").as_deref() == Some("openai")
            && config::raw("llmBaseUrl").as_deref() == Some("https://api.deepseek.com"),
        &None,
    )?;
    expect("preset menu closed", !ui.has("preset-menu-0"), &None)?;

    // Typing goes through window.dispatch_keystroke into the focused input.
    ui.click("llmModel")?;
    ui.key("cmd-a")?;
    ui.type_text("deepseek-v4")?;
    expect(
        "model typed",
        config::raw("llmModel").as_deref() == Some("deepseek-v4"),
        &None,
    )?;
    ui.click("llmTimeoutMs")?;
    ui.key("cmd-a")?;
    ui.type_text("2500")?;
    expect(
        "timeout not stored before enter",
        config::raw("llmTimeoutMs").is_none(),
        &None,
    )?;
    ui.key("enter")?;
    expect(
        "timeout stored on enter",
        config::raw("llmTimeoutMs").as_deref() == Some("2500"),
        &None,
    )?;
    ui.click("llmExtraBody")?;
    ui.type_text("[1]")?;
    expect(
        "extra body typed",
        config::raw("llmExtraBody").as_deref() == Some("[1]"),
        &None,
    )?;
    ui.shot("refine-typed")?;

    ui.scroll("preset", -500.)?;
    ui.click("llmHistory-1")?;
    expect(
        "history up",
        config::raw("llmHistory").as_deref() == Some("4"),
        &None,
    )?;
    ui.click("llmAppContext")?;
    expect(
        "app context off",
        config::raw("llmAppContext").as_deref() == Some("0"),
        &None,
    )?;

    ui.click("prompt-open")?;
    expect("prompt editor shown", ui.has("prompt-reset"), &None)?;
    ui.click("prompt")?;
    ui.type_text("X")?;
    let edited = config::raw("llmPrompt");
    expect(
        "prompt edited",
        edited
            .as_deref()
            .is_some_and(|p| p != llm::DEFAULT_PROMPT && p.contains('X')),
        &None,
    )?;
    ui.shot("refine-prompt")?;
    ui.scroll("prompt-open", -600.)?;
    ui.click("prompt-reset")?;
    expect(
        "prompt restored",
        config::raw("llmPrompt").is_none(),
        &edited,
    )?;

    // The test call: empty base URL fails before any keychain or network access (and fixture runs never send).
    config::set("llmBaseUrl", serde_json::Value::Null).map_err(|e| e.to_string())?;
    ui.scroll("prompt-reset", -2000.)?;
    ui.click("refine-run")?;
    expect("test failure shown", ui.has("refine-result"), &None)?;
    ui.shot("refine-test")?;
    ui.close()
}
