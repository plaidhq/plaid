//! 文字: local text rules with a live preview, then the LLM pass between the ASR result and the paste.
use super::key_field::KeyField;
use super::mav::*;
use super::{Model, bool_str, drop_menu, field, on_if_one, on_unless_zero, pick_list};
use crate::{config, secret};
use gpui_kit::base::input::{
    InputBaseState, InputEvent, InputModeKind, InputState, Textarea, TextareaState,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::pct;
use plaid_core::llm;
use plaid_core::rules::{KEYS, Rules};
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

/// What the preview runs the rules over: one filler, a restart, a correction, a Latin term, a number, a period.
const RULE_SAMPLE: &str =
    "嗯，那个，我们明天明天下午三点，不对，四点开会讨论一下Q4的预算，大概要两百万。";

/// (title, example) per rule, in `KEYS` order.
pub(super) const RULE_TILES: [(&str, &str); 6] = [
    ("去掉语气词", "嗯，那个，我们 → 我们"),
    ("去掉重复", "明天明天 → 明天"),
    ("处理改口", "三点，不对，四点 → 四点"),
    ("句末不加句号", "…两百万。 → …两百万"),
    ("中英文加空格", "讨论Q4的 → 讨论 Q4 的"),
    ("数字写成阿拉伯", "两百万 → 200万"),
];

/// Form row for tall controls: the label sits on the control's first line instead of its middle.
pub(super) fn top_row(label: impl Into<SharedString>, control: impl IntoElement) -> Div {
    let label: SharedString = label.into();
    let label: SharedString = label
        .strip_suffix('：')
        .map(|s| SharedString::from(s.to_string()))
        .unwrap_or(label);
    div()
        .flex()
        .items_start()
        .gap(px(12.))
        .child(
            div()
                .w(px(LABEL_W))
                .flex_none()
                .pt(px(6.))
                .text_color(hex(TEXT2))
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
        .px(px(10.))
        .py(px(6.))
        .when(mono_font, |d| d.font(mono()).text_size(px(12.)))
        .child(div().flex_1().min_w_0().child(Textarea::new(state)))
}

/// Text field that takes the row's remaining width.
pub(super) fn wide_field(state: &Entity<InputState>, window: &Window, cx: &App) -> Div {
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    field_frame(focused)
        .flex_1()
        .min_w_0()
        .h(px(30.))
        .px(px(10.))
        .child(div().flex_1().min_w_0().child(Input::new(state)))
}

use gpui_kit::base::input::Input;

/// Amber "已修改" capsule.
fn pill(text: &'static str) -> Div {
    div()
        .px(px(7.))
        .rounded_full()
        .text_size(px(11.))
        .line_height(px(17.))
        .text_color(hex(AMBER))
        .bg(tint(AMBER, 0x22))
        .child(text)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Preset,
    Format,
    /// The level tag of one 按应用 row.
    App(usize),
    AddApp,
}

/// The global levels with their subtitles (原文 is per-app only).
const LEVELS: [(llm::Level, &str); 3] = [
    (llm::Level::Off, "只用本地规则"),
    (llm::Level::Light, "补标点、修错字，不改说法"),
    (llm::Level::Fluent, "整理成书面语，可能改语序"),
];

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
    prompts: [Prompt; 3],
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
                "系统提示词 · 通顺",
                llm::DEFAULT_PROMPT,
                (12, 30),
                window,
                cx,
            ),
            prompt(
                "llmPromptLight",
                "prompt-light",
                "系统提示词 · 轻度",
                llm::LIGHT_PROMPT,
                (8, 24),
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
            light: llm::global_level(config::value) == llm::Level::Light,
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
            return "关：识别结果只过本地规则就粘贴".into();
        }
        let s = "出错或超时直接粘贴原文；润色中光带变紫，按 esc 跳过";
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

    fn set_apps(&self, apps: &[(String, llm::Level)], cx: &mut Context<Self>) {
        let v = llm::format_app_levels(apps);
        self.model.update(cx, |m, cx| m.set("llmApps", v, cx));
    }

    /// 按应用: apps that refine differently from the global level.
    fn apps_card(&self, cx: &mut Context<Self>) -> Div {
        let apps = llm::app_levels(&config::raw("llmApps").unwrap_or_default());
        // Candidates: apps dictated into, most used first.
        let mut seen: Vec<(String, usize)> = vec![];
        for r in &self.model.read(cx).store.all {
            if let Some(a) = &r.app {
                match seen.iter_mut().find(|(s, _)| s == a) {
                    Some(e) => e.1 += 1,
                    None => seen.push((a.clone(), 1)),
                }
            }
        }
        seen.sort_by_key(|e| std::cmp::Reverse(e.1));
        let candidates: Vec<String> = seen
            .into_iter()
            .map(|(a, _)| a)
            .filter(|a| !apps.iter().any(|(x, _)| x.eq_ignore_ascii_case(a)))
            .take(12)
            .collect();
        let mut rows = vec![];
        for (i, (app, l)) in apps.iter().enumerate() {
            let open = self.menu == Some(Menu::App(i));
            let this = cx.entity().downgrade();
            let list = apps.clone();
            let tag = drop_menu(
                format!("llmApp-{i}"),
                level_tag(format!("llmApp-{i}"), *l),
                110.,
                llm::Level::ALL
                    .iter()
                    .map(|x| MenuEntry {
                        label: level_menu_label(*x).into(),
                        checked: x == l,
                        rule: false,
                    })
                    .collect(),
                open,
                self.menu_toggle(Menu::App(i), open, cx),
                self.menu_close(cx),
                move |k, _, cx| {
                    let mut list = list.clone();
                    list[i].1 = llm::Level::ALL[k];
                    let _ = this.update(cx, |this, cx| {
                        this.menu = None;
                        this.set_apps(&list, cx);
                    });
                },
            );
            let this = cx.entity().downgrade();
            let list = apps.clone();
            let remove = hit(SharedString::from(format!("llmApp-{i}-remove")))
                .size(px(22.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .hover(|s| s.bg(hex(HOVER)))
                .child(icon("plaid/xmark.svg", 10., hex(TEXT3)))
                .on_click(move |_, _, cx| {
                    let mut list = list.clone();
                    list.remove(i);
                    let _ = this.update(cx, |this, cx| this.set_apps(&list, cx));
                });
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(34.))
                    .when(i > 0, |d| d.border_t_1().border_color(hexa(LINE)))
                    .child(app_icon(app, 22.).map_or_else(
                        || app_badge(app).into_any_element(),
                        IntoElement::into_any_element,
                    ))
                    .child(div().flex_1().min_w_0().truncate().child(app.clone()))
                    .child(tag)
                    .child(remove),
            );
        }
        let open = self.menu == Some(Menu::AddApp);
        let this = cx.entity().downgrade();
        let list = apps.clone();
        let picks = candidates.clone();
        // A new app starts at the opposite of the global choice: 关 under an LLM level, 轻度 under 关.
        let fresh = if llm::global_level(config::value) > llm::Level::Off {
            llm::Level::Off
        } else {
            llm::Level::Light
        };
        let face = small_button("llmApps-add", "+ 添加", |_, _, _| {});
        let add = if candidates.is_empty() {
            disabled(face).into_any_element()
        } else {
            drop_menu(
                "llmApps-add",
                face,
                160.,
                candidates
                    .iter()
                    .map(|a| MenuEntry {
                        label: a.clone().into(),
                        checked: false,
                        rule: false,
                    })
                    .collect(),
                open,
                self.menu_toggle(Menu::AddApp, open, cx),
                self.menu_close(cx),
                move |k, _, cx| {
                    let mut list = list.clone();
                    list.push((picks[k].clone(), fresh));
                    let _ = this.update(cx, |this, cx| {
                        this.menu = None;
                        this.set_apps(&list, cx);
                    });
                },
            )
            .into_any_element()
        };
        group_body()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(15.))
                            .font_weight(medium())
                            .child("按应用"),
                    )
                    .child(add),
            )
            .child(hint("在这些应用里改用单独的方式；原文连本地规则也不用"))
            .when(
                apps.iter().any(|(_, l)| *l >= llm::Level::Light)
                    && config::value("llmBaseUrl").is_none(),
                |d| {
                    d.child(
                        div()
                            .text_size(px(12.))
                            .text_color(hex(AMBER))
                            .child("还没配置 LLM 服务：轻度和通顺会直接粘贴原文"),
                    )
                },
            )
            .when(!rows.is_empty(), |d| {
                d.child(div().flex().flex_col().children(rows))
            })
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
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(8.))
                        .bg(hexa(GLASS_WELL))
                        .border_1()
                        .border_color(hexa(LINE))
                        .text_color(hex(if *failed { RED } else { TEXT }))
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

/// `llmMinChars` choices; anything else saved by hand shows as the default.
const MIN_CHARS: [usize; 3] = [0, 20, 50];

fn min_chars() -> usize {
    config::value("llmMinChars")
        .and_then(|s| s.parse().ok())
        .unwrap_or(plaid_core::dictation::REFINE_MIN_CHARS)
}

fn history() -> usize {
    config::value("llmHistory")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(3)
        .min(10)
}

/// The rules preview: the sample as recognized (removed characters struck through) and as it would paste
/// (inserted characters in cyan).
fn preview() -> Div {
    let rules = Rules::load(config::value);
    let out = rules.apply(RULE_SAMPLE);
    let (deleted, inserted) = super::perf::diff(RULE_SAMPLE, &out);
    let struck = HighlightStyle {
        color: Some(hex(TEXT3)),
        strikethrough: Some(StrikethroughStyle {
            thickness: px(1.5),
            color: Some(hex(RED)),
        }),
        ..Default::default()
    };
    let added = HighlightStyle {
        color: Some(hex(CYAN)),
        background_color: Some(tint(CYAN, 0x30)),
        ..Default::default()
    };
    let changed: usize = deleted
        .iter()
        .map(|r| RULE_SAMPLE[r.clone()].chars().count())
        .sum::<usize>()
        .max(
            inserted
                .iter()
                .map(|r| out[r.clone()].chars().count())
                .sum::<usize>(),
        );
    let label = |tag: &'static str, note: &'static str, right: String, lit: bool| {
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .font(mono())
            .text_size(px(11.))
            .text_color(hex(TEXT2))
            .child(
                div()
                    .px(px(7.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .map(|d| {
                        if lit {
                            d.bg(hex(CYAN))
                                .text_color(hex(BG))
                                .font_weight(FontWeight::SEMIBOLD)
                        } else {
                            d.border_1().border_color(hexa(LINE3))
                        }
                    })
                    .child(tag),
            )
            .child(div().flex_1().child(note))
            .child(right)
    };
    group_body()
        .gap(px(12.))
        .child(label(
            "识别",
            "火山返回的原文",
            "本地 · 不调用 LLM".into(),
            false,
        ))
        .child(
            div()
                .font(mono())
                .text_size(px(14.))
                .line_height(px(24.))
                .text_color(hex(TEXT2))
                .child(
                    StyledText::new(RULE_SAMPLE)
                        .with_highlights(deleted.into_iter().map(|g| (g, struck))),
                ),
        )
        .child(
            div()
                .h(px(1.))
                .flex()
                .child(
                    div()
                        .flex_1()
                        .h_full()
                        .bg(hgrada(CYAN << 8, (CYAN << 8) | 0x80)),
                )
                .child(
                    div()
                        .flex_1()
                        .h_full()
                        .bg(hgrada((VIOLET << 8) | 0x80, VIOLET << 8)),
                ),
        )
        .child(label(
            "粘贴",
            "出现在光标处的文字",
            if changed == 0 {
                "与原文一致".into()
            } else {
                format!("改动 {changed} 字")
            },
            true,
        ))
        .child(
            div()
                .text_size(px(22.))
                .line_height(px(32.))
                .min_h(px(64.))
                .child(
                    StyledText::new(out).with_highlights(inserted.into_iter().map(|g| (g, added))),
                ),
        )
}

/// One rule as a tile: title, a dot that lights when on, the example underneath. The tile toggles the rule.
/// One global level: violet bar and wash when picked.
fn level_card(
    l: llm::Level,
    sub: &'static str,
    picked: bool,
    model: &Entity<Model>,
) -> Stateful<Div> {
    let model = model.clone();
    hit(SharedString::from(format!("llmLevel-{}", l.key())))
        .relative()
        .flex()
        .flex_col()
        .gap(px(2.))
        .pl(px(14.))
        .pr(px(12.))
        .py(px(8.))
        .rounded(px(8.))
        .border_1()
        .overflow_hidden()
        .map(|d| {
            if picked {
                d.bg(tint(VIOLET, 0x1a)).border_color(tint(VIOLET, 0x55))
            } else {
                d.border_color(hexa(LINE)).hover(|s| s.bg(hex(HOVER)))
            }
        })
        .when(picked, |d| {
            d.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(3.))
                    .bg(hex(VIOLET))
                    .shadow(vec![glow(VIOLET, 8., 0x99)]),
            )
        })
        .child(
            div()
                .text_size(px(13.))
                .font_weight(medium())
                .text_color(hex(if picked { TEXT } else { TEXT2 }))
                .child(l.label()),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(hex(if picked { TEXT2 } else { TEXT3 }))
                .child(sub),
        )
        .on_click(move |_, _, cx| {
            model.update(cx, |m, cx| {
                if l == llm::Level::Off {
                    m.set("llmEnabled", bool_str(false), cx);
                } else {
                    m.set("llmEnabled", bool_str(true), cx);
                    m.set("llmLevel", l.key(), cx);
                }
            })
        })
}

fn level_menu_label(l: llm::Level) -> &'static str {
    match l {
        llm::Level::Raw => "原文",
        llm::Level::Off => "关 · 只用规则",
        llm::Level::Light => "轻度",
        llm::Level::Fluent => "通顺",
    }
}

/// The per-app level as a small tag: violet when it calls the LLM.
fn level_tag(id: String, l: llm::Level) -> Stateful<Div> {
    let llm = l >= llm::Level::Light;
    hit(SharedString::from(id))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .h(px(22.))
        .pl(px(8.))
        .pr(px(6.))
        .rounded(px(6.))
        .text_size(px(12.))
        .map(|d| {
            if llm {
                d.bg(tint(VIOLET, 0x22)).text_color(hex(VIOLET))
            } else {
                d.bg(hex(RAISED2)).text_color(hex(TEXT2))
            }
        })
        .hover(|s| s.bg(hex(HOVER)))
        .child(l.label())
        .child(icon(
            "plaid/chev-down.svg",
            9.,
            hex(if llm { VIOLET } else { TEXT3 }),
        ))
}

/// First letter of an app in a small mono square.
fn app_badge(app: &str) -> Div {
    let c: String = app
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    div()
        .size(px(22.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .bg(hex(RAISED2))
        .border_1()
        .border_color(hexa(LINE2))
        .font(mono())
        .text_size(px(11.))
        .text_color(hex(TEXT2))
        .child(c)
}

fn rule_tile(i: usize, model: &Entity<Model>) -> Stateful<Div> {
    let key = KEYS[i];
    let (title, example) = RULE_TILES[i];
    let on = on_if_one(key);
    let model = model.clone();
    hit(key)
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(14.))
        .pt(px(14.))
        .pb(px(12.))
        .rounded(px(14.))
        .border_1()
        .map(|d| {
            if on {
                d.bg(tint(CYAN, 0x14))
                    .border_color(tint(CYAN, 0x66))
                    .shadow(vec![glow(CYAN, 18., 0x1a)])
            } else {
                d.bg(hexa(GLASS))
                    .border_color(hexa(LINE))
                    .hover(|s| s.border_color(hexa(LINE3)))
            }
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(14.))
                        .font_weight(medium())
                        .child(title),
                )
                .child(div().size(px(10.)).flex_none().rounded_full().map(|d| {
                    if on {
                        d.bg(hex(CYAN)).shadow(vec![glow(CYAN, 10., 0xcc)])
                    } else {
                        d.border(px(1.5)).border_color(hex(0x3a3d45))
                    }
                })),
        )
        .child(
            div()
                .font(mono())
                .text_size(px(11.))
                .text_color(hex(TEXT2))
                .truncate()
                .child(example),
        )
        .on_click(move |_, _, cx| {
            model.update(cx, |m, cx| m.set(key, bool_str(!on), cx));
        })
}

impl Render for Refine {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = on_if_one("llmEnabled");
        let tiles = |from: usize| {
            div()
                .flex()
                .gap(px(10.))
                .children((from..from + 3).map(|i| rule_tile(i, &self.model)))
        };
        let rules = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(card_head(
                "本地规则",
                "在本机运行，不调用 LLM，短句也生效。点一下切换。",
            ))
            .child(tiles(0))
            .child(tiles(3));
        let picked = MIN_CHARS
            .iter()
            .position(|&n| n == min_chars())
            .unwrap_or(1);
        let level = llm::global_level(config::value);
        let llm = group_body()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .size(px(8.))
                            .rounded_full()
                            .bg(hex(if enabled { VIOLET } else { TEXT3 }))
                            .when(enabled, |d| d.shadow(vec![glow(VIOLET, 10., 0xcc)])),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(15.))
                            .font_weight(medium())
                            .child("LLM 润色"),
                    ),
            )
            .child(
                div().flex().flex_col().gap(px(6.)).children(
                    LEVELS
                        .iter()
                        .map(|&(l, sub)| level_card(l, sub, l == level, &self.model)),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(hex(TEXT2))
                            .child("什么时候润色"),
                    )
                    .child({
                        let model = self.model.clone();
                        segmented(
                            "llmMinChars",
                            &["每句", "20 字以上", "50 字以上"],
                            picked,
                            move |i, _, cx| {
                                let v = MIN_CHARS[i.min(MIN_CHARS.len() - 1)];
                                model.update(cx, |m, cx| m.set("llmMinChars", v.to_string(), cx))
                            },
                        )
                    })
                    .child(hint("更短的句子只走本地规则，识别完直接粘贴")),
            )
            .child(hint(self.status(cx)));
        let llm = div()
            .w(px(300.))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(llm)
            .child(self.apps_card(cx));
        super::page_body("refine")
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(16.))
                            .child(preview())
                            .child(rules),
                    )
                    .child(llm),
            )
            .child(div().pt(px(8.)).child(group("LLM 服务")))
            .child(section("服务商", self.provider_rows(window, cx)))
            .child(section("上下文", self.context_rows(window, cx)))
            .child(section("提示词", self.prompt_rows(window, cx)))
            .child(section("测试", self.test_rows(window, cx)))
            .child(hint("启用 LLM 后，识别文本及已配置的上下文会发送给所选服务；使用远程接口时会离开本机。「词典 → 后台校对」独立运行，关闭润色不会关闭后台校对。"))
    }
}

#[cfg(all(feature = "snapshot", any(target_os = "macos", target_os = "linux")))]
pub(super) fn check(ui: &mut super::snapshot::Ui) -> Result<(), String> {
    use super::Page;
    use super::snapshot::{expect, facts};
    ui.open(Page::Refine, facts())?;
    ui.shot("refine")?;

    let before = config::raw("llmEnabled");
    ui.click("llmLevel-off")?;
    expect(
        "level off",
        config::raw("llmEnabled").as_deref() == Some("0"),
        &before,
    )?;
    ui.shot("refine-off")?;
    ui.click("llmLevel-light")?;
    expect(
        "level light",
        config::raw("llmEnabled").as_deref() == Some("1")
            && config::raw("llmLevel").as_deref() == Some("light"),
        &None,
    )?;
    ui.click("llmApps-add")?;
    ui.shot("refine-apps-add")?;
    ui.click("llmApps-add-menu-0")?;
    ui.click("llmApps-add")?;
    ui.click("llmApps-add-menu-0")?;
    ui.click("llmApp-1")?;
    ui.click("llmApp-1-menu-0")?;
    let apps = llm::app_levels(&config::raw("llmApps").unwrap_or_default());
    expect(
        "two apps: 关 then 原文",
        apps.len() == 2 && apps[0].1 == llm::Level::Off && apps[1].1 == llm::Level::Raw,
        &config::raw("llmApps"),
    )?;
    ui.shot("refine-apps")?;
    ui.click("llmApp-0-remove")?;
    expect(
        "one app left",
        llm::app_levels(&config::raw("llmApps").unwrap_or_default()).len() == 1,
        &config::raw("llmApps"),
    )?;
    ui.click("llmLevel-fluent")?;
    expect(
        "level fluent",
        config::raw("llmLevel").as_deref() == Some("fluent"),
        &None,
    )?;

    for (i, key) in KEYS.iter().enumerate() {
        ui.click(key)?;
        expect(
            &format!("rule {i} on"),
            config::raw(key).as_deref() == Some("1"),
            &None,
        )?;
    }
    ui.shot("refine-rules")?;
    ui.click(KEYS[0])?;
    expect(
        "rule 0 off",
        config::raw(KEYS[0]).as_deref() == Some("0"),
        &None,
    )?;
    ui.click("llmMinChars-2")?;
    expect(
        "min chars 50",
        config::raw("llmMinChars").as_deref() == Some("50"),
        &None,
    )?;
    ui.scroll("llmMinChars-2", -420.)?;
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

    ui.scroll("llmAppContext", -300.)?;
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
