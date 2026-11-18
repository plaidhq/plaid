//! Write-only API key field: shows whether a key is stored, never the key itself.
use super::mav::*;
use super::{Model, field};
use crate::secret;
use gpui_kit::base::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

pub struct KeyField {
    model: Entity<Model>,
    /// Credential store account, recomputed per render (the LLM account follows llmBaseUrl).
    account: Box<dyn Fn() -> String>,
    input: Entity<InputState>,
    /// (account, stored) from the last lookup; looked up again when the account changes.
    stored: Option<(String, bool)>,
    editing: bool,
    error: Option<String>,
}

impl KeyField {
    pub fn new(
        model: Entity<Model>,
        account: impl Fn() -> String + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> KeyField {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("粘贴 API key")
        });
        cx.subscribe_in(&input, window, |this, _, e: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = e {
                this.save(window, cx);
            }
            cx.notify();
        })
        .detach();
        KeyField {
            model,
            account: Box::new(account),
            input,
            stored: None,
            editing: false,
            error: None,
        }
    }

    fn stored(&mut self, cx: &App) -> bool {
        let account = (self.account)();
        if let Some((a, s)) = &self.stored
            && *a == account
        {
            return *s;
        }
        let facts = &self.model.read(cx).facts;
        let s = if facts.fixture {
            account == "apiKey" && facts.volc_key
        } else {
            secret::get(&account).ok().flatten().is_some()
        };
        self.stored = Some((account, s));
        s
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let v = self.input.read(cx).value().trim().to_string();
        if v.is_empty() {
            return;
        }
        self.write(&v, window, cx);
    }

    fn write(&mut self, v: &str, window: &mut Window, cx: &mut Context<Self>) {
        let account = (self.account)();
        let fixture = self.model.read(cx).facts.fixture;
        let r = if fixture {
            Ok(())
        } else {
            secret::set(&account, v)
        };
        match r {
            Ok(()) => {
                self.error = None;
                self.editing = false;
                self.stored = Some((account.clone(), !v.is_empty()));
                self.input.update(cx, |i, cx| {
                    i.set_value("", window, cx);
                    i.set_placeholder("粘贴 API key", window, cx);
                });
                if account == "apiKey" {
                    self.model.update(cx, |m, cx| {
                        m.facts.volc_key = !v.is_empty();
                        cx.notify();
                    });
                }
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }
}

impl Render for KeyField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let stored = self.stored(cx);
        let line = div().flex().items_center().gap(px(8.));
        let line = if stored && !self.editing {
            line.child(badge(Status::Ok, "已存入钥匙串"))
                .child(button(
                    "key-change",
                    "更换…",
                    cx.listener(|this, _, w, cx| {
                        this.editing = true;
                        this.input.update(cx, |i, cx| {
                            i.set_placeholder("输入新的 key", w, cx);
                            i.focus(w, cx);
                        });
                        cx.notify();
                    }),
                ))
                .child(button(
                    "key-delete",
                    "删除",
                    cx.listener(|this, _, w, cx| this.write("", w, cx)),
                ))
        } else {
            let empty = self.input.read(cx).value().trim().is_empty();
            let save = default_button(
                "key-save",
                "保存",
                cx.listener(|this, _, w, cx| this.save(w, cx)),
            );
            line.child(field(&self.input, 260., window, cx))
                .child(if empty { disabled(save) } else { save })
                .when(stored, |d| {
                    d.child(button(
                        "key-cancel",
                        "取消",
                        cx.listener(|this, _, w, cx| {
                            this.editing = false;
                            this.input.update(cx, |i, cx| i.set_value("", w, cx));
                            cx.notify();
                        }),
                    ))
                })
        };
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(line)
            .when_some(self.error.clone(), |d, e| {
                d.child(hint(e).text_color(hex(0xa3241d)))
            })
    }
}
