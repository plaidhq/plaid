//! 命令: voice commands kept in actions.json, and how they are triggered (ActionsPage.swift).
use super::Model;
use super::mav::*;
use super::refine::{editor, top_row, wide_field};
use crate::config;
use gpui_kit::base::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::actions::{Action, Outcome, Output};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::SystemTime;

const OUTPUTS: [(Output, &str); 3] = [
    (Output::None, "不处理"),
    (Output::Hud, "HUD 显示"),
    (Output::Paste, "粘贴到光标"),
];

#[cfg(not(windows))]
const FOOTNOTE: &str = "命令用 /bin/zsh -c 执行，PATH 取自登录 shell，超时 30 秒。环境变量：PLAID_ARGS（命令名之后的内容，同时写入 stdin）、PLAID_TEXT（整句）、PLAID_APP、PLAID_BUNDLE、PLAID_WINDOW、PLAID_BEFORE（光标前文本）。引用时加双引号，如 \"$PLAID_ARGS\"；AppleScript 里别用 system attribute 读中文（编码会乱），改为 osascript … \"$PLAID_ARGS\" 经 argv 传入。";
#[cfg(windows)]
const FOOTNOTE: &str = "命令用 powershell -Command 执行，超时 30 秒。环境变量：PLAID_ARGS（命令名之后的内容，同时写入 stdin）、PLAID_TEXT（整句）、PLAID_APP、PLAID_BUNDLE、PLAID_WINDOW、PLAID_BEFORE（光标前文本）。在 PowerShell 里用 $env:PLAID_ARGS 读取。";

/// Starting points in the 添加 menu (Actions.examples).
fn examples() -> [Action; 4] {
    let a = |name: &str, aliases: &[&str], run: &str, output| Action {
        name: name.into(),
        aliases: aliases.iter().map(|s| s.to_string()).collect(),
        run: run.into(),
        output,
        confirm: false,
    };
    [
        a(
            "搜索",
            &["搜一下"],
            r#"q=$(printf %s "$PLAID_ARGS" | perl -pe 's/([^A-Za-z0-9._~-])/sprintf("%%%02X",ord($1))/ge'); open "https://www.google.com/search?q=$q""#,
            Output::None,
        ),
        a("今天日期", &[], "date +%Y-%m-%d", Output::Paste),
        a(
            "暂停音乐",
            &[],
            r#"osascript -e 'if application "Music" is running then tell application "Music" to pause'"#,
            Output::None,
        ),
        a(
            "通知",
            &[],
            r#"osascript -e 'on run argv' -e 'display notification (item 1 of argv) with title "Plaid"' -e 'end run' "$PLAID_ARGS""#,
            Output::None,
        ),
    ]
}

fn path() -> PathBuf {
    config::data_dir().join("actions.json")
}

fn mtime() -> Option<SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

fn split_aliases(s: &str) -> Vec<String> {
    s.split([',', '，', '、'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

#[derive(Clone, Copy)]
enum Text {
    Name,
    Aliases,
    Run,
}

/// Editors of one command; `list[i]` is the command, `cards[i]` its card.
struct Card {
    uid: usize,
    name: Entity<InputState>,
    aliases: Entity<InputState>,
    run: Entity<TextareaState>,
    args: Entity<InputState>,
    running: bool,
    last: Option<Outcome>,
}

pub struct Commands {
    model: Entity<Model>,
    list: Vec<Action>,
    /// actions.json doesn't parse: the page is read-only so the file isn't overwritten.
    error: Option<String>,
    /// mtime of the file as last read or written (inner None = missing); None = never read.
    loaded: Option<Option<SystemTime>>,
    cards: Vec<Card>,
    wake: Entity<InputState>,
    adding: bool,
    next_uid: usize,
    /// uid of the card whose editor is expanded; the others show as one row.
    open: Option<usize>,
}

impl Commands {
    pub fn new(model: Entity<Model>, window: &mut Window, cx: &mut Context<Self>) -> Commands {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let wake = cx.new(|cx| InputState::new(window, cx).placeholder("Plaid"));
        let w = config::raw("wakeWord").unwrap_or_else(|| "Plaid".into());
        wake.update(cx, |i, cx| i.set_value(w, window, cx));
        cx.subscribe_in(&wake, window, |this, input, e: &InputEvent, _, cx| {
            if let InputEvent::Change = e {
                let v = input.read(cx).value().to_string();
                this.model.update(cx, |m, cx| m.set("wakeWord", v, cx));
            }
        })
        .detach();
        let mut this = Commands {
            model,
            list: vec![],
            error: None,
            loaded: None,
            cards: vec![],
            wake,
            adding: false,
            next_uid: 0,
            open: None,
        };
        this.reload(window, cx);
        this
    }

    /// Re-reads actions.json when its mtime moved (Actions.reload). Cards of unchanged entries are kept.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stamp = mtime();
        if self.loaded == Some(stamp) {
            return;
        }
        self.loaded = Some(stamp);
        let list = match std::fs::read(path()) {
            Err(_) => vec![],
            Ok(b) => match serde_json::from_slice::<Vec<Action>>(&b) {
                Ok(l) => l,
                Err(e) => {
                    self.error = Some(e.to_string());
                    self.lock(true, window, cx);
                    return;
                }
            },
        };
        let was_error = self.error.take().is_some();
        let mut old: Vec<Option<Card>> = std::mem::take(&mut self.cards)
            .into_iter()
            .map(Some)
            .collect();
        for (i, a) in list.iter().enumerate() {
            let keep = self.list.get(i).is_some_and(|o| o.name == a.name);
            let card = match old.get_mut(i).and_then(Option::take) {
                Some(c) if keep => c,
                _ => self.card(window, cx),
            };
            fill(&card, a, window, cx);
            self.cards.push(card);
        }
        self.list = list;
        if was_error {
            self.lock(false, window, cx);
        }
        cx.notify();
    }

    fn lock(&self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        for c in &self.cards {
            c.name.update(cx, |i, cx| i.set_disabled(on, cx));
            c.aliases.update(cx, |i, cx| i.set_disabled(on, cx));
            c.run.update(cx, |i, cx| i.set_disabled(on, cx));
            c.args.update(cx, |i, cx| i.set_disabled(on, cx));
        }
        let _ = window;
        cx.notify();
    }

    fn card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Card {
        let uid = self.next_uid;
        self.next_uid += 1;
        let line = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let name = line("命令名", window, cx);
        let aliases = line("别名，用逗号分隔", window, cx);
        let args = line("参数（PLAID_ARGS）", window, cx);
        let run = cx.new(|cx| TextareaState::new(window, cx).rows(5));
        for (input, which) in [(&name, Text::Name), (&aliases, Text::Aliases)] {
            cx.subscribe_in(input, window, move |this, input, e: &InputEvent, _, cx| {
                if let InputEvent::Change = e {
                    let v = input.read(cx).value().to_string();
                    this.edit(uid, which, v, cx);
                }
            })
            .detach();
        }
        cx.subscribe_in(&run, window, move |this, input, e: &InputEvent, _, cx| {
            if let InputEvent::Change = e {
                let v = input.read(cx).value().to_string();
                this.edit(uid, Text::Run, v, cx);
            }
        })
        .detach();
        cx.subscribe_in(&args, window, |_, _, e: &InputEvent, _, cx| {
            if let InputEvent::Change = e {
                cx.notify();
            }
        })
        .detach();
        Card {
            uid,
            name,
            aliases,
            run,
            args,
            running: false,
            last: None,
        }
    }

    fn index(&self, uid: usize) -> Option<usize> {
        self.cards.iter().position(|c| c.uid == uid)
    }

    fn edit(&mut self, uid: usize, which: Text, v: String, cx: &mut Context<Self>) {
        let Some(i) = self.index(uid) else { return };
        self.change(i, cx, |a| match which {
            Text::Name => a.name = v,
            Text::Aliases => a.aliases = split_aliases(&v),
            Text::Run => a.run = v,
        });
    }

    /// Applies `f` to command `i` and saves when it actually changed.
    fn change(&mut self, i: usize, cx: &mut Context<Self>, f: impl FnOnce(&mut Action)) {
        if self.error.is_some() {
            return;
        }
        let Some(a) = self.list.get_mut(i) else {
            return;
        };
        let before = a.clone();
        f(a);
        if *a != before {
            self.save();
        }
        cx.notify();
    }

    /// Writes actions.json (atomically) unless it failed to parse; the new mtime counts as read.
    fn save(&mut self) {
        if self.error.is_some() {
            return;
        }
        let p = path();
        let tmp = p.with_extension("json.tmp");
        let body = serde_json::to_string_pretty(&self.list).unwrap_or_else(|_| "[]".into()) + "\n";
        let written = std::fs::create_dir_all(config::data_dir())
            .and_then(|_| std::fs::write(&tmp, body))
            .and_then(|_| std::fs::rename(&tmp, &p));
        if let Err(e) = written {
            crate::trace::log(&format!("actions.json write: {e}"));
        }
        self.loaded = Some(mtime());
    }

    fn add(&mut self, pick: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.adding = false;
        if self.error.is_some() {
            return cx.notify();
        }
        let a = match pick {
            0 => Action::default(),
            i => examples()[i - 1].clone(),
        };
        let card = self.card(window, cx);
        fill(&card, &a, window, cx);
        self.open = Some(card.uid);
        self.cards.push(card);
        self.list.push(a);
        self.save();
        cx.notify();
    }

    fn delete(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.error.is_some() || i >= self.list.len() {
            return;
        }
        self.list.remove(i);
        self.cards.remove(i);
        self.save();
        cx.notify();
    }

    /// Runs command `i` once with the args field (ActionCard.testRow): never pastes. Snapshot runs stop before the
    /// shell.
    fn try_run(&mut self, i: usize, cx: &mut Context<Self>) {
        let (Some(a), Some(card)) = (self.list.get(i).cloned(), self.cards.get_mut(i)) else {
            return;
        };
        if card.running || a.run.trim().is_empty() {
            return;
        }
        if self.model.read(cx).facts.fixture {
            card.last = Some(Outcome::failed("snapshot run: not run"));
            return cx.notify();
        }
        let args = card.args.read(cx).value().to_string();
        let text = [a.name.as_str(), args.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let env = vec![
            ("PLAID_TEXT", text),
            ("PLAID_ARGS", args.clone()),
            ("PLAID_APP", "Plaid".to_string()),
            ("PLAID_BUNDLE", String::new()),
            ("PLAID_WINDOW", "Plaid 设置".to_string()),
            ("PLAID_BEFORE", String::new()),
        ];
        card.running = true;
        let uid = card.uid;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move { crate::run::run(&a, &args, &env) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(i) = this.index(uid) {
                    this.cards[i].running = false;
                    this.cards[i].last = Some(r);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_file(&self, cx: &App) {
        if self.model.read(cx).facts.fixture {
            return;
        }
        let p = path();
        if !p.exists() {
            let _ = std::fs::create_dir_all(config::data_dir());
            let _ = std::fs::write(&p, "[]\n");
        }
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&p).spawn();
        #[cfg(windows)]
        let _ = std::process::Command::new("explorer").arg(&p).spawn();
    }

    /// Names of other commands sharing a trigger with command `i` (case-insensitive).
    fn conflicts(&self, i: usize) -> Vec<&str> {
        let mine: HashSet<String> = self.list[i]
            .triggers()
            .iter()
            .map(|t| t.to_lowercase())
            .collect();
        self.list
            .iter()
            .enumerate()
            .filter(|(j, o)| {
                *j != i
                    && o.triggers()
                        .iter()
                        .any(|t| mine.contains(&t.to_lowercase()))
            })
            .map(|(_, o)| o.name.as_str())
            .collect()
    }

    fn trigger_card(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let bare = config::value("commandMode").as_deref() == Some("bare");
        let model = self.model.clone();
        let w = self.wake.read(cx).value();
        let w = w.trim().to_string();
        let note = if bare {
            hint("以命令名开头的句子都会执行。口述内容碰巧以命令名开头也会触发，例如「搜索引擎优化……」会执行「搜索」")
                .text_color(hex(AMBER))
        } else if w.is_empty() {
            hint("唤醒词为空时等同直接说命令名")
        } else {
            hint(format!(
                "只有以唤醒词开头的句子当作命令，例如「{w}，搜索 Swift 并发」；唤醒词后没有匹配的命令时提示，不粘贴"
            ))
        };
        group_body()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(div().text_size(px(15.)).font_weight(medium()).child("触发"))
                    .child(segmented(
                        "commandMode",
                        &["唤醒词", "直接说命令名"],
                        usize::from(bare),
                        move |i, _, cx| {
                            let v = if i == 1 { "bare" } else { "wake" };
                            model.update(cx, |m, cx| m.set("commandMode", v, cx));
                        },
                    ))
                    .when(!bare, |d| {
                        d.child(div().text_color(hex(TEXT2)).child("唤醒词")).child(
                            hit("wakeWord")
                                .w(px(160.))
                                .child(super::field(&self.wake, 160., window, cx)),
                        )
                    }),
            )
            .child(note)
    }

    /// One command folded to a line: 「name」, aliases, a leader, then the shell line and how its output is used.
    fn command_row(&self, i: usize, open: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let a = &self.list[i];
        let uid = self.cards[i].uid;
        let broken =
            a.name.trim().is_empty() || a.run.trim().is_empty() || !self.conflicts(i).is_empty();
        let name = a.name.trim();
        let run = a.run.trim().lines().next().unwrap_or("").to_string();
        let output = match a.output {
            Output::None => None,
            Output::Hud => Some("HUD"),
            Output::Paste => Some("粘贴"),
        };
        hit(SharedString::from(format!("cmd-{i}-row")))
            .flex()
            .items_center()
            .gap(px(10.))
            .min_h(px(42.))
            .py(px(8.))
            .mx(px(-10.))
            .px(px(10.))
            .rounded(px(8.))
            .when(i > 0, |d| d.border_t_1().border_color(hexa(LINE)))
            .cursor_pointer()
            .hover(|s| s.bg(hexa(0xffffff08)))
            .child(icon(
                if open {
                    "plaid/chev-down.svg"
                } else {
                    "plaid/chev-right.svg"
                },
                11.,
                hex(TEXT3),
            ))
            .child(
                div()
                    .flex_none()
                    .max_w(px(220.))
                    .truncate()
                    .font_weight(medium())
                    .map(|d| {
                        if name.is_empty() {
                            d.text_color(hex(TEXT3)).child("未命名")
                        } else {
                            d.child(format!("「{name}」"))
                        }
                    }),
            )
            .when(!a.aliases.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .max_w(px(180.))
                        .truncate()
                        .text_size(px(12.))
                        .text_color(hex(TEXT3))
                        .child(format!("也说 {}", a.aliases.join(" / "))),
                )
            })
            .when(broken, |d| d.child(div().text_color(hex(AMBER)).child("⚠")))
            .child(div().flex_1().min_w(px(16.)).h(px(1.)).bg(hexa(LINE)))
            .child(
                div()
                    .flex_none()
                    .max_w(px(340.))
                    .truncate()
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(6.))
                    .bg(hexa(GLASS_WELL))
                    .border_1()
                    .border_color(hexa(LINE))
                    .font(mono())
                    .text_size(px(12.))
                    .map(|d| {
                        if run.is_empty() {
                            d.text_color(hex(TEXT3)).child("（空）")
                        } else {
                            d.text_color(hex(TEXT2)).child(run)
                        }
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(32.))
                    .text_size(px(11.))
                    .text_color(hex(TEXT3))
                    .child(output.unwrap_or("—")),
            )
            .when(a.confirm, |d| d.child(keycap("⏎")))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open = if this.open == Some(uid) {
                    None
                } else {
                    Some(uid)
                };
                cx.notify();
            }))
    }

    fn add_menu(&self, cx: &mut Context<Self>) -> Div {
        let locked = self.error.is_some();
        let this = cx.entity().downgrade();
        let face = popup("cmd-add", "添加", 80.).on_click(move |_, _, cx| {
            let _ = this.update(cx, |this, cx| {
                if this.error.is_none() {
                    this.adding = !this.adding;
                    cx.notify();
                }
            });
        });
        let this = cx.entity().downgrade();
        let close = this.clone();
        div()
            .flex()
            .flex_col()
            .child(if locked { disabled(face) } else { face })
            .when(self.adding, |d| {
                let labels: Vec<String> = std::iter::once("空白命令".to_string())
                    .chain(examples().iter().map(|e| format!("示例：{}", e.name)))
                    .collect();
                let items = labels
                    .into_iter()
                    .enumerate()
                    .map(|(i, label)| MenuEntry {
                        label: label.into(),
                        checked: false,
                        // NSMenu separator after 空白命令.
                        rule: i == 0,
                    })
                    .collect();
                d.child(deferred(
                    anchored().snap_to_window_with_margin(px(8.)).child(
                        menu("cmd-add-menu", 80., items, move |i, w, cx| {
                            let _ = this.update(cx, |this, cx| this.add(i, w, cx));
                        })
                        .on_mouse_down_out(move |_, _, cx| {
                            let _ = close.update(cx, |this, cx| {
                                this.adding = false;
                                cx.notify();
                            });
                        }),
                    ),
                ))
            })
    }

    fn card_view(&self, i: usize, window: &Window, cx: &mut Context<Self>) -> Div {
        let a = &self.list[i];
        let c = &self.cards[i];
        let locked = self.error.is_some();
        let id = |s: &str| SharedString::from(format!("cmd-{i}-{s}"));
        let warning = if a.name.trim().is_empty() || a.run.trim().is_empty() {
            Some("命令名或命令为空，不会触发".to_string())
        } else {
            let names = self.conflicts(i);
            (!names.is_empty()).then(|| {
                format!(
                    "触发词与「{}」重复，较长的先匹配，相同时取靠前的",
                    names.join("」「")
                )
            })
        };
        let delete = hit(id("delete"))
            .flex_none()
            .px(px(4.))
            .child(icon("plaid/trash.svg", 13., hex(TEXT2)))
            .on_click(cx.listener(move |this, _, _, cx| this.delete(i, cx)));
        let output = OUTPUTS
            .iter()
            .position(|(o, _)| *o == a.output)
            .unwrap_or(1);
        let labels: Vec<&str> = OUTPUTS.iter().map(|(_, l)| *l).collect();
        let this = cx.entity().downgrade();
        let output = segmented(id("output"), &labels, output, move |k, _, cx| {
            let _ = this.update(cx, |this, cx| {
                this.change(i, cx, |a| a.output = OUTPUTS[k].0)
            });
        });
        let this = cx.entity().downgrade();
        let confirm = checkbox(
            id("confirm"),
            a.confirm,
            "执行前确认",
            move |v, _, cx| {
                let _ = this.update(cx, |this, cx| this.change(i, cx, |a| a.confirm = v));
            },
        );
        let can_try = !c.running && !a.run.trim().is_empty() && !locked;
        let try_button = button(
            id("try"),
            if c.running {
                "运行中…"
            } else {
                "试运行"
            },
            cx.listener(move |this, _, _, cx| this.try_run(i, cx)),
        );
        let lock = |el: Stateful<Div>| if locked { disabled(el) } else { el };
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .pt(px(4.))
            .pb(px(14.))
            .child(row(
                "命令名：",
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(hit(id("name")).child(super::field(&c.name, 160., window, cx)))
                    .child(
                        hit(id("aliases"))
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .child(wide_field(&c.aliases, window, cx)),
                    )
                    .child(lock(delete)),
            ))
            .when_some(warning, |d, w| {
                d.child(row("", hint(format!("⚠ {w}")).text_color(hex(AMBER))))
            })
            .child(top_row(
                "命令：",
                hit(id("run")).child(editor(&c.run, true, window, cx)),
            ))
            .child(row("输出：", div().flex().child(lock(output))))
            .child(row("", lock(confirm)))
            .child(row("", hint("识别后按 ⏎ 才执行，esc 取消")))
            .child(row(
                "试运行：",
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        hit(id("args"))
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .child(wide_field(&c.args, window, cx)),
                    )
                    .child(if can_try {
                        try_button
                    } else {
                        disabled(try_button)
                    }),
            ));
        if let Some(r) = &c.last {
            let line = if r.ok() {
                hint(format!(
                    "完成 · {} ms{}",
                    r.ms,
                    if a.output == Output::Paste {
                        " · 试运行不粘贴"
                    } else {
                        ""
                    }
                ))
            } else {
                hint(format!("失败 · {} · {} ms", r.failure(), r.ms)).text_color(hex(RED))
            };
            body = body.child(row("", hit(id("result")).child(line)));
            let shown: Vec<&str> = [r.out.as_str(), r.err.as_str()]
                .into_iter()
                .map(|s| s.trim_matches(['\n', '\r']))
                .filter(|s| !s.is_empty())
                .collect();
            let shown = shown.join("\n");
            if !shown.is_empty() {
                let text: String = shown.lines().take(12).collect::<Vec<_>>().join("\n");
                body = body.child(top_row(
                    "",
                    div()
                        .px(px(6.))
                        .py(px(4.))
                        .bg(hexa(GLASS_WELL))
                        .border_1()
                        .border_color(hexa(LINE2))
                        .font(mono())
                        .text_size(px(12.))
                        .child(text),
                ));
            }
        }
        body
    }
}

/// Puts `a` into a card's editors without emitting edits.
fn fill(card: &Card, a: &Action, window: &mut Window, cx: &mut App) {
    let set = |input: &Entity<InputState>, v: String, window: &mut Window, cx: &mut App| {
        if input.read(cx).value() != v.as_str() {
            input.update(cx, |i, cx| i.set_value(v, window, cx));
        }
    };
    set(&card.name, a.name.clone(), window, cx);
    if split_aliases(&card.aliases.read(cx).value()) != a.aliases {
        set(&card.aliases, a.aliases.join(", "), window, cx);
    }
    if card.run.read(cx).value() != a.run.as_str() {
        let run = a.run.clone();
        card.run.update(cx, |i, cx| i.set_value(run, window, cx));
    }
}

impl Render for Commands {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.reload(window, cx);
        let n = self.list.len();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .child(hint("说出命令名执行自定义 shell 命令，代替粘贴"))
            .child(button(
                "open-actions",
                "打开 actions.json",
                cx.listener(|this, _, _, cx| this.open_file(cx)),
            ));
        let mut list = group_body().gap(px(0.)).child(
            card_head("命令", format!("{n} 个 · 点一行展开编辑"))
                .items_center()
                .pb(px(8.))
                .child(self.add_menu(cx)),
        );
        if n == 0 {
            list = list.child(
                div()
                    .py(px(10.))
                    .child(hint("还没有命令。从「添加」里选一个示例开始。")),
            );
        }
        for i in 0..n {
            let open = self.open == Some(self.cards[i].uid);
            list = list.child(self.command_row(i, open, cx));
            if open {
                list = list.child(self.card_view(i, window, cx));
            }
        }
        let commands = div()
            .flex()
            .flex_col()
            .child(list)
            .child(div().mt(px(8.)).mx(px(6.)).child(hint(FOOTNOTE)));
        super::page_body("commands")
            .child(header)
            .when_some(self.error.clone(), |d, e| {
                d.child(
                    hit("actions-error")
                        .flex()
                        .items_start()
                        .gap(px(8.))
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(5.))
                        .bg(tint(RED, 0x14))
                        .border_1()
                        .border_color(tint(RED, 0x55))
                        .child(icon("plaid/xcircle.svg", 14., hex(RED)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(hex(RED))
                                .child(format!("actions.json 解析失败，修好文件前这里只读：{e}")),
                        ),
                )
            })
            .child(self.trigger_card(window, cx))
            .child(commands)
    }
}

#[cfg(all(feature = "snapshot", any(target_os = "macos", target_os = "linux")))]
pub(super) fn check(ui: &mut super::snapshot::Ui) -> Result<(), String> {
    use super::Page;
    use super::snapshot::{expect, facts};
    let file = || -> Result<Vec<Action>, String> {
        let b = std::fs::read(path()).map_err(|e| e.to_string())?;
        serde_json::from_slice(&b).map_err(|e| e.to_string())
    };
    ui.open(Page::Commands, facts())?;
    ui.shot("commands")?;
    expect(
        "fixture commands shown",
        ui.has("cmd-0-row") && ui.has("cmd-1-row") && !ui.has("cmd-2-row") && !ui.has("cmd-0-name"),
        &None,
    )?;
    let untouched = std::fs::read_to_string(path()).ok();
    expect(
        "opening does not rewrite actions.json",
        untouched.as_deref().is_some_and(|s| s.contains("phrases")),
        &untouched,
    )?;

    ui.click("commandMode-1")?;
    expect(
        "bare mode",
        config::raw("commandMode").as_deref() == Some("bare") && !ui.has("wakeWord"),
        &config::raw("commandMode"),
    )?;
    ui.shot("commands-bare")?;
    ui.click("commandMode-0")?;
    expect(
        "wake mode",
        config::raw("commandMode").as_deref() == Some("wake") && ui.has("wakeWord"),
        &config::raw("commandMode"),
    )?;
    ui.click("wakeWord")?;
    ui.key("cmd-a")?;
    ui.type_text("Hey")?;
    expect(
        "wake word typed",
        config::raw("wakeWord").as_deref() == Some("Hey"),
        &config::raw("wakeWord"),
    )?;

    ui.click("cmd-0-row")?;
    expect(
        "row expands",
        ui.has("cmd-0-name") && !ui.has("cmd-1-name"),
        &None,
    )?;
    ui.shot("commands-open")?;
    ui.click("cmd-1-row")?;
    expect(
        "one open at a time",
        ui.has("cmd-1-name") && !ui.has("cmd-0-name"),
        &None,
    )?;
    ui.click("cmd-1-row")?;
    expect("row folds", !ui.has("cmd-1-name"), &None)?;

    ui.click("cmd-add")?;
    ui.shot("commands-add")?;
    ui.click("cmd-add-menu-1")?;
    let l = file()?;
    expect(
        "example added",
        l.len() == 3
            && l[2].name == "搜索"
            && l[2].aliases == ["搜一下"]
            && l[2].output == Output::None,
        &None,
    )?;
    ui.click("cmd-add")?;
    ui.click("cmd-add-menu-0")?;
    let l = file()?;
    expect(
        "blank added",
        l.len() == 4 && l[3] == Action::default(),
        &None,
    )?;

    ui.scroll("commandMode", -1500.)?;
    ui.click("cmd-3-name")?;
    ui.type_text("hello")?;
    ui.click("cmd-3-aliases")?;
    ui.type_text("hi, yo")?;
    ui.click("cmd-3-run")?;
    ui.type_text("echo hi")?;
    ui.click("cmd-3-output-2")?;
    ui.click("cmd-3-confirm")?;
    let l = file()?;
    expect(
        "blank edited",
        l[3].name == "hello"
            && l[3].aliases == ["hi", "yo"]
            && l[3].run == "echo hi"
            && l[3].output == Output::Paste
            && l[3].confirm,
        &Some(format!("{:?}", l.get(3))),
    )?;
    ui.click("cmd-3-try")?;
    expect("trial result shown", ui.has("cmd-3-result"), &None)?;
    ui.shot("commands-edited")?;
    ui.click("cmd-3-delete")?;
    expect("deleted", file()?.len() == 3, &None)?;

    // A broken file turns the page read-only and is never overwritten.
    std::fs::write(path(), "[{").map_err(|e| e.to_string())?;
    ui.frame()?;
    expect("parse error shown", ui.has("actions-error"), &None)?;
    ui.scroll("cmd-2-row", 3000.)?;
    ui.shot("commands-error")?;
    ui.click("cmd-add")?;
    expect(
        "add locked",
        !ui.has("cmd-add-menu-0") && std::fs::read_to_string(path()).ok().as_deref() == Some("[{"),
        &None,
    )?;
    std::fs::write(path(), "[]").map_err(|e| e.to_string())?;
    ui.frame()?;
    expect(
        "error cleared",
        !ui.has("actions-error") && !ui.has("cmd-0-row"),
        &None,
    )?;
    ui.close()
}
