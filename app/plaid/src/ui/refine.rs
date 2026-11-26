//! 润色 (local text rules with a live preview, then the LLM pass between the ASR result and the paste) and 服务
//! (recognition and LLM endpoints and their keys): one view, two pages.
use super::key_field::KeyField;
use super::mav::*;
use super::{Model, Page, Settings, bool_str, field, on_if_one, on_unless_zero, pick_list};
use crate::{config, secret};
use gpui_kit::base::input::{
    InputBaseState, InputEvent, InputModeKind, InputState, Textarea, TextareaState,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::pct;
use plaid_core::rules::{KEYS, Rules};
use plaid_core::{cloud, llm, refine};
use std::time::Instant;

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
    /// The 添加应用 menu of one style card.
    StyleApps(usize),
    /// The 测试 section's app.
    TestApp,
}

/// The style card being edited, with its two fields.
struct StyleEdit {
    i: usize,
    name: Entity<InputState>,
    prompt: Entity<TextareaState>,
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

/// The signed-in Cloud account as last fetched.
#[derive(Default)]
struct CloudAccount {
    busy: bool,
    error: Option<String>,
    info: Option<cloud::Account>,
    /// The 账户 row shows the name field instead of the name.
    renaming: bool,
}

pub struct Refine {
    model: Entity<Model>,
    /// Page::Refine (润色) or Page::Service (服务).
    page: Page,
    nav: WeakEntity<Settings>,
    /// The LLM endpoint's key (the account follows llmBaseUrl).
    key: Entity<KeyField>,
    /// The Volcengine key.
    asr_key: Entity<KeyField>,
    invite: Entity<InputState>,
    /// 服务 › Cloud: the new user name while renaming.
    new_name: Entity<InputState>,
    account: CloudAccount,
    menu: Option<Menu>,
    base: Entity<InputState>,
    llm_model: Entity<InputState>,
    extra: Entity<TextareaState>,
    timeout: Entity<InputState>,
    context: Entity<TextareaState>,
    prompts: [Prompt; 3],
    sample: Entity<TextareaState>,
    test: Test,
    editing: Option<StyleEdit>,
    /// The app the test request pretends to dictate into (its level and styles); None is the global level.
    test_app: Option<String>,
    /// The add-app picker's search field.
    query: Entity<InputState>,
    /// Apps in the Applications folders (macOS), scanned when the page opens.
    installed: Vec<String>,
}

impl Refine {
    pub fn new(
        model: Entity<Model>,
        page: Page,
        nav: WeakEntity<Settings>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Refine {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let m = model.clone();
        let asr_key = cx.new(|cx| KeyField::new(m, "key", || "apiKey".into(), window, cx));
        let m = model.clone();
        let key = cx.new(|cx| {
            KeyField::new(
                m,
                "llm-key",
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
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("搜索或输入应用名"));
        cx.subscribe(&query, |_, _, e: &InputEvent, cx| {
            if let InputEvent::Change = e {
                cx.notify();
            }
        })
        .detach();
        #[cfg(target_os = "macos")]
        let installed = crate::mac::installed_apps()
            .into_iter()
            .map(|a| a.0)
            .collect();
        #[cfg(not(target_os = "macos"))]
        let installed = vec![];
        let sample = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6));
        sample.update(cx, |i, cx| i.set_value(SAMPLE, window, cx));
        let invite = cx.new(|cx| InputState::new(window, cx).placeholder("输入邀请码"));
        let new_name = cx.new(|cx| {
            InputState::new(window, cx).placeholder(format!("最多 {} 个字符", cloud::NAME_MAX))
        });
        cx.subscribe_in(&new_name, window, |this, _, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::PressEnter { .. }) {
                this.cloud_rename(cx);
            }
        })
        .detach();
        Refine {
            model,
            page,
            nav,
            key,
            asr_key,
            invite,
            new_name,
            account: CloudAccount::default(),
            menu: None,
            base,
            llm_model,
            extra,
            timeout,
            context,
            prompts,
            sample,
            test: Test::default(),
            editing: None,
            test_app: None,
            query,
            installed,
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
        if llm::endpoint(config::value).base.is_empty() {
            self.test = Test {
                result: Some(("还没有填写接口地址".into(), true)),
                ..Test::default()
            };
            cx.notify();
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

    /// The controller's prompt (`refine::prompt`), built from the current settings.
    fn request(&self, text: &str, fixture: bool, cx: &App) -> Result<llm::Request, String> {
        let ep = llm::endpoint(config::value);
        let key = (!fixture && !ep.base.is_empty())
            .then(|| secret::llm_key(&ep.base))
            .flatten();
        let front = self.test_app.as_deref();
        let fill = refine::Fill {
            app: front.unwrap_or("Plaid 设置"),
            front,
            history: &[],
            hotwords: &config::terms(true).join(", "),
            fixes: &self.model.read(cx).store.fixes(),
        };
        let light = llm::level(config::value) == llm::Level::Light;
        let plan = refine::direct(config::value, light, &fill, timeout_ms());
        let extra = config::value("llmExtraBody");
        plan.request(&ep, key.as_deref(), &plan.user_with(text), extra.as_deref())
    }

    fn status(&self, cx: &App) -> String {
        if !on_if_one("llmEnabled") {
            return "关：识别结果只过本地规则就粘贴".into();
        }
        let s = "出错或超时直接粘贴原文；润色时按 esc 跳过";
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

    /// An add-app button and its pop-up: a search field, then 所有应用 (when `all`), 常用 (apps dictated into)
    /// and 全部应用 (installed), minus `taken`. A typed name that matches nothing can be added as is.
    #[allow(clippy::too_many_arguments)]
    fn app_picker(
        &self,
        id: SharedString,
        face: Stateful<Div>,
        which: Menu,
        all: bool,
        taken: &[String],
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, Option<String>, &mut Context<Self>) + 'static,
    ) -> Div {
        let open = self.menu == Some(which);
        let toggle = cx.listener(move |this, _, window, cx| {
            if this.menu == Some(which) {
                this.menu = None;
            } else {
                this.query.update(cx, |q, cx| {
                    q.set_value("", window, cx);
                    q.focus(window, cx);
                });
                this.menu = Some(which);
            }
            cx.notify();
        });
        let wrap = div().flex().flex_col().child(face.on_click(toggle));
        if !open {
            return wrap;
        }
        let q = self.query.read(cx).value().trim().to_string();
        let free = |a: &String| !taken.iter().any(|t| t.eq_ignore_ascii_case(a));
        let hit_q = |a: &String| q.is_empty() || a.to_lowercase().contains(&q.to_lowercase());
        let used: Vec<String> = used_apps(&self.model.read(cx).store)
            .into_iter()
            .filter(|a| free(a) && hit_q(a))
            .take(if q.is_empty() { 8 } else { usize::MAX })
            .collect();
        let rest: Vec<String> = self
            .installed
            .iter()
            .filter(|a| free(a) && hit_q(a) && !used.iter().any(|u| u.eq_ignore_ascii_case(a)))
            .cloned()
            .collect();
        let exact = used.iter().chain(&rest).any(|a| a.eq_ignore_ascii_case(&q));
        // Display order: (label, pick, typed); a header has no pick and None inside is 所有应用.
        let mut entries: Vec<(String, Option<Option<String>>, bool)> = vec![];
        if all && q.is_empty() {
            entries.push(("所有应用".into(), Some(None), false));
        }
        for (title, list) in [("常用", used), ("全部应用", rest)] {
            if !list.is_empty() && q.is_empty() {
                entries.push((title.into(), None, false));
            }
            entries.extend(list.into_iter().map(|a| (a.clone(), Some(Some(a)), false)));
        }
        if !q.is_empty() && !exact {
            entries.push((format!("添加「{q}」"), Some(Some(q.clone())), true));
        }
        let on_pick = std::rc::Rc::new(on_pick);
        let mut k = 0;
        let mut rows: Vec<AnyElement> = vec![];
        for (label, pick, typed) in entries {
            let Some(app) = pick else {
                rows.push(
                    div()
                        .pt(px(8.))
                        .pb(px(2.))
                        .px(px(8.))
                        .text_size(px(11.))
                        .text_color(hex(TEXT3))
                        .child(label)
                        .into_any_element(),
                );
                continue;
            };
            let glyph = match (&app, typed) {
                (Some(a), false) => app_icon(a, 18.).map_or_else(
                    || app_badge(a).size(px(18.)).into_any_element(),
                    IntoElement::into_any_element,
                ),
                _ => div().size(px(18.)).flex_none().into_any_element(),
            };
            let pick = on_pick.clone();
            rows.push(
                hit(SharedString::from(format!("{id}-menu-{k}")))
                    .h(px(30.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .hover(|s| s.bg(tint(CYAN, 0x26)))
                    .child(glyph)
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        pick(this, app.clone(), cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            k += 1;
        }
        let close = cx.listener(|this, _, _, cx| {
            this.menu = None;
            cx.notify();
        });
        let search = div()
            .h(px(32.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(10.))
            .rounded(px(7.))
            .bg(hexa(GLASS_WELL))
            .border_1()
            .border_color(hexa(LINE))
            .child(icon("plaid/search.svg", 12., hex(TEXT2)))
            .child(div().flex_1().min_w_0().child(Input::new(&self.query)));
        wrap.child(deferred(
            anchored().snap_to_window_with_margin(px(8.)).child(
                hit(SharedString::from(format!("{id}-menu")))
                    .occlude()
                    .mt(px(4.))
                    .w(px(250.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .p(px(6.))
                    .rounded(px(10.))
                    .bg(hex(RAISED2))
                    .border_1()
                    .border_color(hexa(LINE3))
                    .shadow(vec![shadow(0., 12., 32., 0., 0x000000a0, false)])
                    .on_mouse_down_out(close)
                    .child(search)
                    .child(
                        div()
                            .id(SharedString::from(format!("{id}-list")))
                            .max_h(px(320.))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .children(rows)
                            .when(k == 0, |d| {
                                d.child(div().p(px(8.)).child(hint("没有可添加的应用")))
                            }),
                    ),
            ),
        ))
    }

    fn set_styles(&self, v: &[llm::Style], cx: &mut Context<Self>) {
        let v = llm::format_styles(v);
        self.model.update(cx, |m, cx| m.set("llmStyles", v, cx));
    }

    /// Opens style `i` for editing; each keystroke rewrites the whole list.
    fn edit_style(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(st) = llm::styles(config::raw).get(i).cloned() else {
            return;
        };
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("名称"));
        name.update(cx, |t, cx| t.set_value(st.name, window, cx));
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("例如：回复保持在三句以内，结论放在第一句。")
        });
        prompt.update(cx, |t, cx| t.set_value(st.prompt, window, cx));
        cx.subscribe_in(&name, window, move |this, input, e: &InputEvent, _, cx| {
            if let InputEvent::Change = e {
                let v = input.read(cx).value().to_string();
                this.update_style(i, cx, |st| st.name = v);
            }
        })
        .detach();
        cx.subscribe_in(
            &prompt,
            window,
            move |this, input, e: &InputEvent, _, cx| {
                if let InputEvent::Change = e {
                    let v = input.read(cx).value().to_string();
                    this.update_style(i, cx, |st| st.prompt = v);
                }
                cx.notify();
            },
        )
        .detach();
        name.update(cx, |t, cx| t.focus(window, cx));
        self.editing = Some(StyleEdit { i, name, prompt });
        self.menu = None;
        cx.notify();
    }

    fn update_style(&self, i: usize, cx: &mut Context<Self>, f: impl FnOnce(&mut llm::Style)) {
        let mut list = llm::styles(config::raw);
        if let Some(st) = list.get_mut(i) {
            f(st);
            self.set_styles(&list, cx);
        }
    }

    fn new_style(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut list = llm::styles(config::raw);
        list.push(llm::Style {
            name: "新风格".into(),
            ..llm::Style::default()
        });
        self.set_styles(&list, cx);
        self.edit_style(list.len() - 1, window, cx);
    }

    fn delete_style(&mut self, i: usize, cx: &mut Context<Self>) {
        let mut list = llm::styles(config::raw);
        if i < list.len() {
            list.remove(i);
            self.set_styles(&list, cx);
        }
        self.editing = None;
        cx.notify();
    }

    /// 风格: one card per style.
    fn styles_block(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let list = llm::styles(config::raw);
        let new = small_button(
            "style-new",
            "+ 新建",
            cx.listener(|this, _, w, cx| this.new_style(w, cx)),
        );
        let cards: Vec<AnyElement> = list
            .iter()
            .enumerate()
            .map(|(i, st)| self.style_card(i, st, window, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(12.))
                    .child(div().flex_1().min_w_0().child(card_head(
                        "风格",
                        "叠加在轻度/通顺之上，按应用分配。只在调用 LLM 时生效。",
                    )))
                    .child(new),
            )
            .child(div().grid().grid_cols(2).gap(px(10.)).children(cards))
    }

    fn style_card(
        &self,
        i: usize,
        st: &llm::Style,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = |s: &str| SharedString::from(format!("style-{i}-{s}"));
        let frame = || {
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(8.))
                .px(px(14.))
                .py(px(12.))
                .rounded(px(12.))
                .border_1()
        };
        if let Some(e) = self.editing.as_ref().filter(|e| e.i == i) {
            let done = button(
                id("done"),
                "完成",
                cx.listener(|this, _, _, cx| {
                    this.editing = None;
                    cx.notify();
                }),
            );
            let delete = small_button(
                id("delete"),
                "删除",
                cx.listener(move |this, _, _, cx| this.delete_style(i, cx)),
            )
            .text_color(hex(RED));
            return frame()
                .bg(tint(VIOLET, 0x10))
                .border_color(tint(VIOLET, 0x55))
                .child(hit(id("name")).child(wide_field(&e.name, window, cx)))
                .child(hit(id("prompt")).child(editor(&e.prompt, false, window, cx)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(delete)
                        .child(div().flex_1())
                        .child(done),
                )
                .into_any_element();
        }
        let mut chips: Vec<AnyElement> = vec![];
        let remove = move |this: &mut Self, cx: &mut Context<Self>, which: Option<usize>| {
            this.update_style(i, cx, |st| match which {
                None => st.all = false,
                Some(k) => {
                    st.apps.remove(k);
                }
            });
        };
        if st.all {
            let live = llm::level(config::value) >= llm::Level::Light;
            chips.push(
                app_chip(
                    id("all"),
                    "所有应用",
                    None,
                    live,
                    cx.listener(move |this, _, _, cx| remove(this, cx, None)),
                )
                .into_any_element(),
            );
        }
        for (k, app) in st.apps.iter().enumerate() {
            let live = llm::level(config::value) >= llm::Level::Light;
            chips.push(
                app_chip(
                    id(&format!("app-{k}")),
                    app.clone(),
                    Some(app),
                    live,
                    cx.listener(move |this, _, _, cx| remove(this, cx, Some(k))),
                )
                .into_any_element(),
            );
        }
        let face = hit(id("add"))
            .h(px(24.))
            .flex()
            .items_center()
            .px(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_dashed()
            .border_color(hexa(LINE3))
            .text_size(px(12.))
            .text_color(hex(TEXT2))
            .hover(|s| s.bg(hex(HOVER)))
            .child("+ 应用");
        let taken = st.apps.clone();
        chips.push(
            self.app_picker(
                id("add"),
                face,
                Menu::StyleApps(i),
                !st.all,
                &taken,
                cx,
                move |this, app, cx| {
                    this.update_style(i, cx, |st| match app {
                        None => st.all = true,
                        Some(a) => st.apps.push(a),
                    });
                },
            )
            .into_any_element(),
        );
        let on = st.all || !st.apps.is_empty();
        let edit = hit(id("edit"))
            .size(px(22.))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .hover(|s| s.bg(hex(HOVER)))
            .child(icon("plaid/pencil.svg", 11., hex(TEXT2)))
            .on_click(cx.listener(move |this, _, w, cx| this.edit_style(i, w, cx)));
        let prompt = st.prompt.trim();
        frame()
            .map(|d| {
                if on {
                    d.bg(tint(VIOLET, 0x0c)).border_color(tint(VIOLET, 0x40))
                } else {
                    d.bg(hexa(GLASS)).border_color(hexa(LINE))
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
                            .min_w_0()
                            .truncate()
                            .text_size(px(14.))
                            .font_weight(medium())
                            .child(if st.name.trim().is_empty() {
                                "未命名".to_string()
                            } else {
                                st.name.clone()
                            }),
                    )
                    .child(edit),
            )
            .child(
                div()
                    .h(px(36.))
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .line_clamp(2)
                    .text_color(hex(if prompt.is_empty() { AMBER } else { TEXT2 }))
                    .child(if prompt.is_empty() {
                        "还没写要求，不会生效".to_string()
                    } else {
                        prompt.to_string()
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .children(chips),
            )
            .into_any_element()
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
            row("API Key", self.key.clone()).into_any_element(),
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
                    row(
                        "",
                        div()
                            .flex()
                            .child(if modified { reset } else { disabled(reset) }),
                    )
                    .into_any_element(),
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
        // Apps to test as: none (the global level), then apps dictated into.
        let picks: Vec<Option<String>> = std::iter::once(None)
            .chain(
                used_apps(&self.model.read(cx).store)
                    .into_iter()
                    .take(12)
                    .map(Some),
            )
            .collect();
        let open = self.menu == Some(Menu::TestApp);
        let this = cx.entity().downgrade();
        let front = self.test_app.as_deref();
        let app = pick_list(
            "refine-test-app",
            170.,
            front.unwrap_or("不指定应用"),
            picks
                .iter()
                .enumerate()
                .map(|(k, p)| MenuEntry {
                    label: p.clone().unwrap_or_else(|| "不指定应用".into()).into(),
                    checked: p.as_deref() == front,
                    rule: k == 0 && picks.len() > 1,
                })
                .collect(),
            open,
            self.menu_toggle(Menu::TestApp, open, cx),
            self.menu_close(cx),
            move |k, _, cx| {
                let pick = picks[k].clone();
                let _ = this.update(cx, |this, cx| {
                    this.menu = None;
                    this.test_app = pick;
                    cx.notify();
                });
            },
        );
        let level = llm::level(config::value);
        let names: Vec<String> = llm::active_styles(config::raw, front)
            .into_iter()
            .map(|s| s.name)
            .collect();
        let note = match level {
            llm::Level::Off => format!(
                "{}：实际听写不会润色，这里仍按通顺测试",
                level_menu_label(level)
            ),
            _ if names.is_empty() => format!("{}，无风格", level_menu_label(level)),
            _ => format!("{} + {}", level_menu_label(level), names.join(" + ")),
        };
        let mut rows = vec![
            row(
                "应用：",
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(app)
                    .child(hint(note)),
            )
            .into_any_element(),
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

/// Apps dictated into, most used first.
fn used_apps(store: &plaid_core::store::Store) -> Vec<String> {
    let mut seen: Vec<(String, usize)> = vec![];
    for r in &store.all {
        if let Some(a) = &r.app {
            match seen.iter_mut().find(|(s, _)| s == a) {
                Some(e) => e.1 += 1,
                None => seen.push((a.clone(), 1)),
            }
        }
    }
    seen.sort_by_key(|e| std::cmp::Reverse(e.1));
    seen.into_iter().map(|(a, _)| a).collect()
}

/// One app a style is on: icon, name, ×. Dim when that app doesn't call the LLM.
fn app_chip(
    id: SharedString,
    label: impl Into<SharedString>,
    app: Option<&str>,
    live: bool,
    on_remove: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let x = hit(SharedString::from(format!("{id}-remove")))
        .size(px(16.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .hover(|s| s.bg(hex(HOVER)))
        .child(icon("plaid/xmark.svg", 8., hex(TEXT3)))
        .on_click(on_remove);
    div()
        .id(id)
        .h(px(24.))
        .flex()
        .items_center()
        .gap(px(5.))
        .pl(px(app.map_or(8., |_| 4.)))
        .pr(px(3.))
        .rounded(px(6.))
        .text_size(px(12.))
        .map(|d| {
            if live {
                d.bg(hex(RAISED2)).text_color(hex(TEXT))
            } else {
                d.bg(hexa(GLASS)).text_color(hex(TEXT3))
            }
        })
        .when_some(app, |d, a| {
            d.child(app_icon(a, 16.).map_or_else(
                || app_badge(a).size(px(16.)).into_any_element(),
                |i| i.opacity(if live { 1. } else { 0.4 }).into_any_element(),
            ))
        })
        .child(label.into())
        .when(!live, |d| {
            d.child(div().text_size(px(11.)).child("· 不润色"))
        })
        .child(x)
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
        .map(|r| plaid_core::records::chars(&RULE_SAMPLE[r.clone()]))
        .sum::<usize>()
        .max(
            inserted
                .iter()
                .map(|r| plaid_core::records::chars(&out[r.clone()]))
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
        llm::Level::Off => "关 · 只用规则",
        llm::Level::Light => "轻度",
        llm::Level::Fluent => "通顺",
    }
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
        if self.page == Page::Service {
            return self.service_page(window, cx);
        }
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
                "在本机运行，不调用 LLM，短句也生效。点卡片切换。",
            ))
            .child(tiles(0))
            .child(tiles(3));
        let picked = MIN_CHARS
            .iter()
            .position(|&n| n == min_chars())
            .unwrap_or(1);
        let level = llm::level(config::value);
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
            .child(self.service_link())
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
            .child(llm);
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
            .child(self.styles_block(window, cx))
            .child(section("上下文", self.context_rows(window, cx)))
            .child(hint("启用 LLM 后，识别文本和上面的上下文会发送到「服务」里设置的接口，使用远程接口时会离开本机。「词典 → 后台校对」不受这里的开关影响。"))
    }
}

impl Refine {
    /// The 润色 page's pointer to 服务: the endpoint in use, or that none is set.
    fn service_link(&self) -> Stateful<Div> {
        let base = config::value("llmBaseUrl").unwrap_or_default();
        let (text, color) = if base.is_empty() {
            ("未设置 LLM 服务".to_string(), AMBER)
        } else {
            let name = PRESETS
                .iter()
                .find(|p| p.2 == base.trim_end_matches('/'))
                .map(|p| p.0.to_string())
                .unwrap_or_else(|| llm::key_account(&base).trim_start_matches("llm:").into());
            let model = config::value("llmModel").unwrap_or_default();
            (
                if model.is_empty() {
                    name
                } else {
                    format!("{name} · {model}")
                },
                TEXT2,
            )
        };
        let nav = self.nav.clone();
        hit("open-service")
            .flex()
            .items_center()
            .gap(px(4.))
            .min_w_0()
            .text_size(px(12.))
            .text_color(hex(color))
            .hover(|s| s.text_color(hex(TEXT)))
            .child(div().truncate().child(text))
            .child("›")
            .on_click(move |_, _, cx| {
                let _ = nav.update(cx, |s, cx| s.show(Page::Service, cx));
            })
    }

    /// The stored device token, if this device is signed in. Snapshot runs never are.
    fn cloud_token(&self, cx: &App) -> Option<String> {
        if self.model.read(cx).facts.fixture {
            return None;
        }
        secret::get("cloudToken")
            .ok()
            .flatten()
            .filter(|t| !t.is_empty())
    }

    /// The page came into view: 服务 in Cloud mode loads the account (balance and usage move between visits).
    pub fn shown(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::Service && config::cloud() {
            self.cloud_refresh(cx);
        }
    }

    /// Fetches the signed-in account's balance and this month's usage.
    fn cloud_refresh(&mut self, cx: &mut Context<Self>) {
        if self.account.busy {
            return;
        }
        #[cfg(feature = "snapshot")]
        if self.model.read(cx).facts.fixture && self.account.info.is_none() {
            self.account.info = Some(cloud::Account {
                name: "neo".into(),
                balance_micros: 77_693_900,
                month_micros: 83_100_000,
                month_ms: 138_000,
            });
            return cx.notify();
        }
        let Some(token) = self.cloud_token(cx) else {
            return;
        };
        let base = config::cloud_url();
        self.account.busy = true;
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move { cloud::account(&base, &token) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.account.busy = false;
                match r {
                    Ok(a) => {
                        this.account.info = Some(a);
                        this.account.error = None;
                    }
                    Err(e) => this.account.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Exchanges the invite code for a device token, stores it and loads the account.
    fn cloud_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.account.busy {
            return;
        }
        let code = self.invite.read(cx).value().trim().to_string();
        if code.is_empty() {
            self.account.error = Some("请输入邀请码".into());
            return cx.notify();
        }
        let base = config::cloud_url();
        if self.model.read(cx).facts.fixture {
            self.account.error = Some("snapshot run: not sent".into());
            return cx.notify();
        }
        self.account = CloudAccount {
            busy: true,
            ..CloudAccount::default()
        };
        self.invite.update(cx, |i, cx| i.set_value("", window, cx));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move { cloud::login(&base, &code, &device_name()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.account.busy = false;
                match r.and_then(|reply| secret::set("cloudToken", &reply.token)) {
                    Ok(()) => {
                        this.model.update(cx, |m, _| m.facts.cloud_token = true);
                        this.cloud_refresh(cx)
                    }
                    Err(e) => this.account.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the name field on the current name.
    fn cloud_rename_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .account
            .info
            .as_ref()
            .map(|i| i.name.clone())
            .unwrap_or_default();
        self.new_name.update(cx, |i, cx| {
            i.set_value(name, window, cx);
            i.focus(window, cx);
        });
        self.account.renaming = true;
        self.account.error = None;
        cx.notify();
    }

    /// Sends the new name; the server refuses a name someone else has.
    fn cloud_rename(&mut self, cx: &mut Context<Self>) {
        if self.account.busy || !self.account.renaming {
            return;
        }
        let name = match cloud::check_name(self.new_name.read(cx).value().as_ref()) {
            Ok(n) => n,
            Err(e) => {
                self.account.error = Some(e);
                return cx.notify();
            }
        };
        if self.account.info.as_ref().is_some_and(|i| i.name == name) {
            self.account.renaming = false;
            self.account.error = None;
            return cx.notify();
        }
        // Snapshot runs: "amy" stands for a name someone else has.
        if self.model.read(cx).facts.fixture {
            if name.eq_ignore_ascii_case("amy") {
                self.account.error = Some("这个名字已经有人用了".into());
            } else if let Some(i) = &mut self.account.info {
                i.name = name;
                self.account.renaming = false;
                self.account.error = None;
            }
            return cx.notify();
        }
        let Some(token) = self.cloud_token(cx) else {
            return;
        };
        let base = config::cloud_url();
        self.account.busy = true;
        self.account.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move { cloud::rename(&base, &token, &name) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.account.busy = false;
                match r {
                    Ok(()) => {
                        this.account.renaming = false;
                        this.cloud_refresh(cx);
                    }
                    Err(e) => this.account.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cloud_logout(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = secret::set("cloudToken", "") {
            self.account.error = Some(e);
            return cx.notify();
        }
        // The mode stays: the tab is the choice, and the footer says Cloud is signed out.
        self.model.update(cx, |m, _| m.facts.cloud_token = false);
        self.account = CloudAccount::default();
        cx.notify();
    }

    /// 服务 › Cloud: the invite code until signed in, then the account, usage and 退出登录.
    fn cloud_card(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let a = &self.account;
        let status = |d: Div| {
            d.when_some(a.error.clone(), |d, e| {
                d.child(hint(e).text_color(hex(RED)))
            })
            .when(a.busy, |d| d.child(hint("连接中…")))
        };
        let body = group_body().child(card_head(
            "Plaid Cloud",
            "识别、润色和学习由 Cloud 完成，无需 API Key；润色级别、风格和词典照常生效",
        ));
        if !self.model.read(cx).facts.cloud_token {
            let login = default_button(
                "cloud-login",
                "登录",
                cx.listener(|this, _, window, cx| this.cloud_login(window, cx)),
            );
            return hit("cloud").child(status(
                body.child(row(
                    "邀请码",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(field(&self.invite, 200., window, cx))
                        .child(login),
                )),
            ));
        }
        let (name, balance, month) = match &a.info {
            Some(i) => (
                i.name.clone(),
                cloud::credits(i.balance_micros),
                format!(
                    "本月已用 {} · 识别 {:.0} 分钟",
                    cloud::credits(i.month_micros),
                    i.month_ms as f64 / 60_000.
                ),
            ),
            None => ("已登录".into(), "—".into(), String::new()),
        };
        let refresh = small_button(
            "cloud-refresh",
            "刷新",
            cx.listener(|this, _, _, cx| this.cloud_refresh(cx)),
        );
        let logout = small_button(
            "cloud-logout",
            "退出登录",
            cx.listener(|this, _, _, cx| this.cloud_logout(cx)),
        );
        let account = if a.renaming {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(field(&self.new_name, 160., window, cx))
                .child(default_button(
                    "cloud-rename-save",
                    "保存",
                    cx.listener(|this, _, _, cx| this.cloud_rename(cx)),
                ))
                .child(small_button(
                    "cloud-rename-cancel",
                    "取消",
                    cx.listener(|this, _, _, cx| {
                        this.account.renaming = false;
                        this.account.error = None;
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(hint("鲸喷榜上显示这个名字，不能和别人重名")),
                )
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().min_w_0().truncate().child(name))
                .when(a.info.is_some(), |d| {
                    d.child(small_button(
                        "cloud-rename",
                        "改名",
                        cx.listener(|this, _, window, cx| this.cloud_rename_start(window, cx)),
                    ))
                })
                .child(div().flex_1())
                .child(logout)
        };
        hit("cloud").child(status(
            body.child(row("账户", account)).child(row(
                "余额",
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(10.))
                    .child(div().child(balance))
                    .child(div().flex_1().min_w_0().truncate().child(hint(month)))
                    .child(refresh),
            )),
        ))
    }

    /// 服务: the BYOK / Cloud tab is the service mode (serviceMode). BYOK shows recognition (Volcengine key and
    /// options) and the LLM endpoint with its prompts and a test; Cloud shows the sign-in or the account.
    fn service_page(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let toggle = |key: &'static str, label: &'static str, sub: &'static str| {
            let model = self.model.clone();
            toggle_row(key, on_unless_zero(key), label, sub, move |v, _, cx| {
                model.update(cx, |m, cx| m.set(key, bool_str(v), cx));
            })
        };
        let this = cx.entity().downgrade();
        let cloud = config::cloud();
        let tab = segmented(
            "service-mode",
            &["BYOK", "Cloud"],
            cloud as usize,
            move |i, _, cx| {
                let _ = this.update(cx, |r, cx| {
                    let mode = if i == 1 { "cloud" } else { "byok" };
                    r.model.update(cx, |m, cx| m.set("serviceMode", mode, cx));
                    if i == 1 {
                        r.cloud_refresh(cx);
                    }
                    cx.notify();
                });
            },
        );
        let body = super::page_body("service").child(div().flex().child(tab));
        if cloud {
            return body.child(self.cloud_card(window, cx));
        }
        body.child(
            group("语音识别").child(
                group_body()
                    .child(card_head("火山引擎", "豆包语音大模型；key 只存在本机"))
                    .child(row("API Key", self.asr_key.clone()))
                    .child(toggle(
                        "asrContext",
                        "发送上下文",
                        "前台应用和输入框里光标前的文字，帮助识别专有名词",
                    ))
                    .child(toggle(
                        "nonstream",
                        "二遍识别",
                        "松键后用整段音频再识别一次，更准，约慢 300 ms",
                    )),
            ),
        )
        .child(
            group("LLM").pt(px(8.)).child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(section("服务商", self.provider_rows(window, cx)))
                    .child(section("提示词", self.prompt_rows(window, cx)))
                    .child(section("测试", self.test_rows(window, cx))),
            ),
        )
    }
}

/// The name this device shows under in the server's device list.
fn device_name() -> String {
    #[cfg(target_os = "macos")]
    let name = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    #[cfg(not(target_os = "macos"))]
    let name = std::env::var("COMPUTERNAME").ok();
    name.filter(|n| !n.is_empty())
        .unwrap_or_else(|| std::env::consts::OS.into())
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

    // 风格: assign to an app and to all apps, remove, create, edit, delete.
    ui.scroll("llmMinChars-2", -300.)?;
    ui.shot("refine-styles")?;
    ui.click("style-0-add")?;
    ui.shot("refine-styles-add")?;
    ui.click("style-0-add-menu-1")?;
    ui.click("style-1-add")?;
    ui.click("style-1-add-menu-0")?;
    let list = llm::styles(config::raw);
    expect(
        "style 0 on one app, style 1 on all",
        list.len() == llm::PRESET_STYLES.len()
            && list[0].apps.len() == 1
            && !list[0].all
            && list[1].all,
        &config::raw("llmStyles"),
    )?;
    ui.click("style-1-add")?;
    ui.click("style-1-add-menu-0")?;
    let app = list[0].apps[0].clone();
    expect(
        "both apply to the app",
        llm::active_styles(config::raw, Some(&app)).len() == 2
            && llm::active_styles(config::raw, None).len() == 1,
        &config::raw("llmStyles"),
    )?;
    ui.scroll("style-0-add", -250.)?;
    ui.click("style-2-add")?;
    ui.click("style-2-add-menu-2")?;
    expect(
        "style 2 on another app",
        llm::styles(config::raw)[2].apps.len() == 1,
        &config::raw("llmStyles"),
    )?;
    ui.shot("refine-styles-on")?;
    ui.click("style-2-app-0-remove")?;
    ui.click("style-1-all-remove")?;
    expect(
        "all removed",
        !llm::styles(config::raw)[1].all,
        &config::raw("llmStyles"),
    )?;
    ui.click("style-0-app-0-remove")?;
    expect(
        "app removed",
        llm::styles(config::raw)[0].apps.is_empty(),
        &config::raw("llmStyles"),
    )?;
    ui.click("style-new")?;
    ui.key("cmd-a")?;
    ui.type_text("短句")?;
    ui.click("style-4-prompt")?;
    ui.type_text("每句不超过二十字")?;
    let list = llm::styles(config::raw);
    expect(
        "new style typed",
        list.len() == 5 && list[4].name == "短句" && list[4].prompt == "每句不超过二十字",
        &config::raw("llmStyles"),
    )?;
    ui.shot("refine-styles-edit")?;
    ui.click("style-4-done")?;
    expect("editor closed", !ui.has("style-4-done"), &None)?;
    ui.click("style-4-edit")?;
    ui.click("style-4-delete")?;
    expect(
        "style deleted",
        llm::styles(config::raw).len() == 4,
        &config::raw("llmStyles"),
    )?;

    ui.scroll("style-0-add", -600.)?;
    ui.shot("refine-context")?;
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

    // 润色 points at 服务, which holds recognition, the LLM endpoint, prompts and the test.
    ui.scroll("llmAppContext", 3000.)?;
    expect(
        "service link on 润色",
        ui.has("open-service") && !ui.has("preset"),
        &None,
    )?;
    ui.click("open-service")?;
    expect(
        "service page: recognition and endpoint",
        ui.has("asrContext") && ui.has("preset") && ui.has("llmModel") && ui.has("refine-run"),
        &None,
    )?;
    ui.shot("service")?;
    // The tab is the service mode. The Cloud tab hides the BYOK settings, and a signed-out device is asked to sign
    // in (a snapshot run never sends the code).
    ui.click("service-mode-1")?;
    expect(
        "cloud tab: cloud mode, sign-in",
        config::cloud() && ui.has("cloud") && ui.has("cloud-login") && !ui.has("asrContext"),
        &None,
    )?;
    ui.shot("service-cloud")?;
    ui.click("service-mode-0")?;
    expect(
        "byok tab: byok mode",
        !config::cloud() && ui.has("asrContext") && !ui.has("cloud"),
        &config::raw("serviceMode"),
    )?;
    let before = config::raw("nonstream");
    ui.click("nonstream")?;
    expect(
        "nonstream toggled",
        config::raw("nonstream") != before,
        &before,
    )?;
    ui.click("nonstream")?;
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
    // The recognition card above pushes the rest of the provider rows below the fold.
    ui.scroll("llmModel", -300.)?;
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

    ui.scroll("llmExtraBody", -300.)?;
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
