//! 词典: the user's hotwords and replacements (hotwords.txt, edited as a draft that saves itself), suggestions from
//! manual edits, rules learned from them, and the learning switches.
use super::mav::*;
use super::{Model, Page, Settings, bool_str, field, on_unless_zero, pick_list};
use crate::{config, secret};
use chrono::Local;
use gpui_kit::base::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::hotwords::{self, Snapshot};
use plaid_core::llm;
use plaid_core::store::{AUTO_RULES, Fix, Store, write_atomic};
use plaid_core::text::trim_ws;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

const ORANGE: u32 = 0xc25e00;
const GREEN: u32 = 0x2e9d48;
const PURPLE: u32 = 0x8a46b0;

const TABS: [&str; 4] = ["全部", "热词", "读音别名", "替换"];
const TAB_HINTS: [&str; 4] = [
    "热词用于识别和优化；替换规则按原文匹配，不区分大小写。改动自动保存，下次录音生效。",
    "识别时优先选这些词。专有名词、英文术语、人名放这里。",
    "别名：读音相近的片段改成想要的；附近出现排除词时不改。",
    "原文匹配，不区分大小写；右侧留空表示删除。",
];
const KINDS: [&str; 3] = ["热词", "替换规则", "高级原文"];

fn path() -> PathBuf {
    config::data_dir().join("hotwords.txt")
}

fn read(p: impl AsRef<Path>) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn modified() -> Option<SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

/// "from -> to" lines the user ignored (suggestions and learned rules alike).
fn dismissed() -> HashSet<String> {
    config::raw("dismissedFixes")
        .unwrap_or_default()
        .split('\n')
        .map(str::to_owned)
        .collect()
}

fn rule(from: &str, to: &str) -> String {
    format!("{from} -> {to}")
}

/// Suggestions from manual edits that no line, learned rule or ignore already covers. `own` is hotwords.txt (the
/// draft on the page); `autos` the learned rules when the caller has them (hotwords.auto.txt covers them otherwise).
fn pending_fixes(store: &Store, own: &str, autos: &[(String, String, usize)]) -> Vec<Fix> {
    let dir = config::data_dir();
    let extra = config::value("hotwordsExtra").map(read).unwrap_or_default();
    let lines = hotwords::lines(&[own.to_owned(), read(dir.join(AUTO_RULES)), extra].join("\n"));
    let have: HashSet<&str> = lines.iter().map(String::as_str).collect();
    let entries = Snapshot::new(&lines, 0.).entries;
    let dismissed = dismissed();
    store
        .fixes()
        .into_iter()
        .filter(|f| {
            let key = rule(&f.from, &f.to);
            !dismissed.contains(&key)
                && !have.contains(key.as_str())
                && !autos.iter().any(|(a, b, _)| *a == f.from && *b == f.to)
                && !entries
                    .iter()
                    .any(|e| e.target == f.to && e.aliases.contains(&f.from))
        })
        .collect()
}

/// Number of pending suggestions, for the sidebar badge.
#[allow(dead_code)]
pub(super) fn pending(model: &Model) -> usize {
    pending_fixes(&model.store, &read(path()), &[]).len()
}

/// What one dictionary line does, as its row shows it.
enum Shape {
    Replace {
        from: String,
        to: String,
    },
    Word {
        target: String,
        aliases: Vec<String>,
        block: Vec<String>,
    },
    Raw,
}

fn words(s: &str) -> Vec<String> {
    s.split('|')
        .map(trim_ws)
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Two `->` parts with a non-empty left side.
fn replacement(line: &str) -> Option<(String, String)> {
    let p: Vec<&str> = line.split("->").map(trim_ws).collect();
    (p.len() == 2 && !p[0].is_empty()).then(|| (p[0].to_owned(), p[1].to_owned()))
}

impl Shape {
    fn of(line: &str) -> Shape {
        let line = trim_ws(line);
        if let Some((from, to)) = replacement(line) {
            return Shape::Replace { from, to };
        }
        if !line.contains("->") {
            let mut halves = line.split("~~~");
            let mut w = words(halves.next().unwrap_or(""));
            let block = halves.next().map(words).unwrap_or_default();
            if !w.is_empty() {
                let target = w.remove(0);
                return Shape::Word {
                    target,
                    aliases: w,
                    block,
                };
            }
        }
        Shape::Raw
    }

    /// Tab it belongs to (0 = only under 全部).
    fn tab(&self) -> usize {
        match self {
            Shape::Word { aliases, .. } if aliases.is_empty() => 1,
            Shape::Word { .. } => 2,
            Shape::Replace { .. } => 3,
            Shape::Raw => 0,
        }
    }

    fn title(&self, line: &str) -> String {
        match self {
            Shape::Replace { from, to } if to.is_empty() => format!("{from} → （删除）"),
            Shape::Replace { from, to } => format!("{from} → {to}"),
            Shape::Word {
                target, aliases, ..
            } if !aliases.is_empty() => format!("{} → {target}", aliases.join(" / ")),
            Shape::Word { target, .. } => target.clone(),
            Shape::Raw => trim_ws(line).to_owned(),
        }
    }

    fn note(&self) -> &'static str {
        match self {
            Shape::Replace { .. } => "原样替换 · 不区分大小写",
            Shape::Word { aliases, .. } if !aliases.is_empty() => "读音别名",
            Shape::Word { .. } => "热词",
            Shape::Raw => "高级原文",
        }
    }

    fn block(&self) -> Option<String> {
        match self {
            Shape::Word { block, .. } if !block.is_empty() => {
                Some(format!("附近出现这些词时不改：{}", block.join("、")))
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Word,
    Replace,
    Advanced,
}

/// The add / edit sheet for one line.
struct Sheet {
    /// Line number being edited; None adds.
    index: Option<usize>,
    original: String,
    kind: Kind,
    word: Entity<InputState>,
    from: Entity<InputState>,
    to: Entity<InputState>,
    raw: Entity<InputState>,
    error: Option<&'static str>,
}

impl Sheet {
    fn line(&self, cx: &App) -> String {
        let v = |s: &Entity<InputState>| s.read(cx).value().to_string();
        match self.kind {
            Kind::Word => trim_ws(&v(&self.word)).to_owned(),
            Kind::Replace => rule(trim_ws(&v(&self.from)), trim_ws(&v(&self.to))),
            Kind::Advanced => v(&self.raw),
        }
    }

    fn validation(&self, cx: &App) -> Option<&'static str> {
        let v = |s: &Entity<InputState>| s.read(cx).value().to_string();
        let line = self.line(cx);
        match self.kind {
            Kind::Word => {
                if v(&self.word).contains('\n') {
                    Some("每个条目只能有一行。")
                } else if line.is_empty() {
                    Some("请输入想要识别的词。")
                } else if line.starts_with('#')
                    || line.contains("->")
                    || line.contains('|')
                    || line.contains("~~~")
                {
                    Some("替换请选「替换规则」；别名、排除词和注释请用「高级原文」。")
                } else {
                    None
                }
            }
            Kind::Replace => {
                let (from, to) = (v(&self.from), v(&self.to));
                if from.contains('\n') || to.contains('\n') {
                    Some("原文和替换内容都只能有一行。")
                } else if trim_ws(&from).is_empty() {
                    Some("请输入要匹配的原文。")
                } else if trim_ws(&from).starts_with('#')
                    || from.contains("->")
                    || to.contains("->")
                {
                    Some("这些字符会改变规则语法，请改用「高级原文」。")
                } else {
                    None
                }
            }
            Kind::Advanced => {
                let raw = v(&self.raw);
                if raw.contains('\n') {
                    Some("这里只编辑一行，多行内容请用词典页的「编辑原文」。")
                } else if trim_ws(&raw).is_empty() {
                    Some("请输入一行内容。")
                } else {
                    None
                }
            }
        }
    }

    fn first_field(&self) -> &Entity<InputState> {
        match self.kind {
            Kind::Word => &self.word,
            Kind::Replace => &self.from,
            Kind::Advanced => &self.raw,
        }
    }
}

/// Derived from the history and the files; dropped whenever either may have changed.
struct Derived {
    fixes: Vec<Fix>,
    autos: Vec<(String, String, usize)>,
}

pub struct Dictionary {
    model: Entity<Model>,
    nav: WeakEntity<Settings>,
    /// The draft of hotwords.txt; `saved` is what the file last got. Edits save 0.5 s later, or right away for
    /// adds and sheet saves, and on page release (window close).
    text: String,
    saved: String,
    error: Option<String>,
    stamp: Option<SystemTime>,
    /// Bumped by every edit and flush; a pending debounce saves only if it is still the latest.
    generation: u64,
    derived: Option<Rc<Derived>>,
    tab: usize,
    selected: Option<usize>,
    search: Entity<InputState>,
    raw: bool,
    raw_input: Entity<TextareaState>,
    batch_open: bool,
    confirm: bool,
    sheet: Option<Sheet>,
    /// A DeepSeek key for background checking (env or keychain), read once.
    fallback_key: bool,
}

impl Dictionary {
    pub fn new(
        model: Entity<Model>,
        nav: WeakEntity<Settings>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Dictionary {
        cx.observe(&model, |this, _, cx| {
            this.derived = None;
            cx.notify();
        })
        .detach();
        cx.on_release(|this, cx| {
            if this.text != this.saved {
                this.flush(cx);
            }
        })
        .detach();
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索热词、错法或替换内容"));
        cx.subscribe(&search, |this, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                this.selected = None;
                cx.notify();
            }
        })
        .detach();
        let raw_input = cx.new(|cx| TextareaState::new(window, cx).rows(16));
        cx.subscribe(&raw_input, |this, s, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                this.text = s.read(cx).value().to_string();
                this.schedule(cx);
            }
        })
        .detach();
        let fallback_key = !model.read(cx).facts.fixture
            && (std::env::var("PLAID_LLM_KEY").is_ok_and(|k| !k.is_empty())
                || secret::get(&llm::key_account(llm::LEARNER_FALLBACK.0))
                    .ok()
                    .flatten()
                    .is_some_and(|k| !k.is_empty()));
        let text = read(path());
        Dictionary {
            model,
            nav,
            saved: text.clone(),
            text,
            error: None,
            stamp: modified(),
            generation: 0,
            derived: None,
            tab: 0,
            selected: None,
            search,
            raw: false,
            raw_input,
            batch_open: false,
            confirm: false,
            sheet: None,
            fallback_key,
        }
    }

    // --- Draft -------------------------------------------------------------------------------------------------

    /// Writes the draft now; regenerates the learned rules against it.
    fn flush(&mut self, cx: &App) {
        self.generation += 1;
        self.derived = None;
        let body = self.text.clone();
        match write_atomic(&path(), body.as_bytes()) {
            Ok(()) => {
                self.saved = body;
                self.error = None;
                self.stamp = modified();
                self.model
                    .read(cx)
                    .store
                    .write_auto_rules(&config::terms(false), &dismissed());
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Saves the draft 0.5 s after the last edit.
    fn schedule(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.derived = None;
        if self.text != self.saved {
            let generation = self.generation;
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.flush(cx);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// Adds a line unless the same line exists, and saves.
    fn append(&mut self, line: &str, cx: &mut Context<Self>) {
        if self.text.split('\n').any(|l| l == line) {
            return;
        }
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
        self.text.push_str(line);
        self.text.push('\n');
        self.flush(cx);
        cx.notify();
    }

    fn remove(&mut self, i: usize, cx: &mut Context<Self>) {
        let mut lines: Vec<&str> = self.text.split('\n').collect();
        if i >= lines.len() {
            return;
        }
        lines.remove(i);
        self.text = lines.join("\n");
        self.selected = None;
        self.schedule(cx);
    }

    /// Re-reads hotwords.txt when it changed on disk and the draft has nothing unsaved.
    fn reload(&mut self) {
        if self.text != self.saved {
            return;
        }
        let stamp = modified();
        if stamp != self.stamp {
            self.stamp = stamp;
            self.text = read(path());
            self.saved = self.text.clone();
            self.derived = None;
        }
    }

    // --- Suggestions -------------------------------------------------------------------------------------------

    fn derived(&mut self, cx: &App) -> Rc<Derived> {
        if let Some(d) = &self.derived {
            return d.clone();
        }
        let store = &self.model.read(cx).store;
        let autos = store.auto_rules(&config::terms(false), &dismissed());
        let fixes = pending_fixes(store, &self.text, &autos);
        let d = Rc::new(Derived { fixes, autos });
        self.derived = Some(d.clone());
        d
    }

    fn dismiss(&mut self, keys: &[String], cx: &mut Context<Self>) {
        let mut s = config::raw("dismissedFixes").unwrap_or_default();
        for k in keys {
            s.push_str(k);
            s.push('\n');
        }
        self.model
            .update(cx, |m, cx| m.set("dismissedFixes", s, cx));
        self.derived = None;
    }

    /// Adds every suggestion as a hotword (or an alias), skipping lines that exist; one save.
    fn accept(&mut self, fixes: &[Fix], alias: bool, cx: &mut Context<Self>) {
        let mut existing: HashSet<String> = self
            .text
            .split('\n')
            .map(|l| trim_ws(l).to_owned())
            .collect();
        let add: Vec<String> = fixes
            .iter()
            .map(|f| {
                if alias {
                    format!("{} | {}", f.to, f.from)
                } else {
                    f.to.clone()
                }
            })
            .filter(|l| existing.insert(l.clone()))
            .collect();
        if add.is_empty() {
            return;
        }
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
        self.text.push_str(&add.join("\n"));
        self.text.push('\n');
        self.flush(cx);
        cx.notify();
    }

    fn batch(&mut self, i: usize, cx: &mut Context<Self>) {
        self.batch_open = false;
        let fixes = self.derived(cx).fixes.clone();
        match i {
            0 => self.accept(&fixes, false, cx),
            1 => self.accept(&fixes, true, cx),
            _ => self.confirm = true,
        }
        cx.notify();
    }

    fn dismiss_all(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<String> = self
            .derived(cx)
            .fixes
            .iter()
            .map(|f| rule(&f.from, &f.to))
            .collect();
        self.confirm = false;
        self.dismiss(&keys, cx);
        cx.notify();
    }

    fn dismiss_auto(&mut self, key: String, cx: &mut Context<Self>) {
        self.dismiss(&[key], cx);
        self.model
            .read(cx)
            .store
            .write_auto_rules(&config::terms(false), &dismissed());
        cx.notify();
    }

    // --- Entry sheet -------------------------------------------------------------------------------------------

    fn open_sheet(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let original = index
            .and_then(|i| self.text.split('\n').nth(i))
            .unwrap_or("")
            .to_owned();
        let line = trim_ws(&original);
        let parts = replacement(line);
        let advanced = line.contains("->")
            || line.contains('|')
            || line.contains("~~~")
            || line.starts_with('#');
        let kind = if parts.is_some() {
            Kind::Replace
        } else if advanced {
            Kind::Advanced
        } else {
            Kind::Word
        };
        let (from, to) = parts.unwrap_or_default();
        let word = self.sheet_input(
            if advanced { "" } else { line },
            "例如：Plaid、张小明",
            window,
            cx,
        );
        let from = self.sheet_input(&from, "例如：克劳德", window, cx);
        let to = self.sheet_input(&to, "例如：Claude；留空表示删除", window, cx);
        let raw = self.sheet_input(&original, "目标词 | 别名 ~~~ 排除词", window, cx);
        let sheet = Sheet {
            index,
            original,
            kind,
            word,
            from,
            to,
            raw,
            error: None,
        };
        sheet.first_field().update(cx, |s, cx| s.focus(window, cx));
        self.sheet = Some(sheet);
        self.batch_open = false;
        cx.notify();
    }

    fn sheet_input(
        &mut self,
        value: &str,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let s = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let value = value.to_owned();
        s.update(cx, |s, cx| s.set_value(value, window, cx));
        cx.subscribe_in(&s, window, |this, _, e: &InputEvent, window, cx| match e {
            InputEvent::Change => {
                if let Some(sheet) = &mut this.sheet {
                    sheet.error = None;
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.save_sheet(window, cx),
            _ => {}
        })
        .detach();
        s
    }

    fn set_kind(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.sheet {
            sheet.kind = [Kind::Word, Kind::Replace, Kind::Advanced][i.min(2)];
            sheet.error = None;
            sheet.first_field().update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }

    fn save_sheet(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = &self.sheet else { return };
        if sheet.validation(cx).is_some() {
            return;
        }
        let line = sheet.line(cx);
        let index = sheet.index;
        let mut lines: Vec<String> = self.text.split('\n').map(str::to_owned).collect();
        let error = if let Some(i) = index
            && lines.get(i) != Some(&sheet.original)
        {
            Some("原文已发生变化。请取消后重新打开这个条目。")
        } else if index.is_some() && line == sheet.original {
            self.sheet = None;
            cx.notify();
            return;
        } else if lines
            .iter()
            .enumerate()
            .any(|(j, l)| Some(j) != index && trim_ws(l) == trim_ws(&line))
        {
            Some("已存在相同的条目。")
        } else {
            None
        };
        if let Some(e) = error {
            if let Some(sheet) = &mut self.sheet {
                sheet.error = Some(e);
            }
            cx.notify();
            return;
        }
        self.sheet = None;
        if let Some(i) = index {
            lines[i] = line;
            self.text = lines.join("\n");
            self.flush(cx);
            cx.notify();
        } else {
            self.append(&line, cx);
        }
    }

    fn close_sheet(&mut self, cx: &mut Context<Self>) {
        self.sheet = None;
        self.confirm = false;
        cx.notify();
    }

    fn set_raw(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.raw = on;
        if on {
            let text = self.text.clone();
            self.raw_input.update(cx, |s, cx| {
                s.set_value(text, window, cx);
                s.focus(window, cx);
            });
        }
        cx.notify();
    }

    /// Background checking is on but no LLM can do it: no refine endpoint and no DeepSeek key.
    fn no_llm(&self) -> bool {
        on_unless_zero("autoLearn") && config::value("llmBaseUrl").is_none() && !self.fallback_key
    }

    // --- Rendering ---------------------------------------------------------------------------------------------

    fn entries(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        let lines: Vec<&str> = self.text.split('\n').collect();
        let all: Vec<(usize, Shape)> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                let t = trim_ws(l);
                !t.is_empty() && !t.starts_with('#')
            })
            .map(|(i, l)| (i, Shape::of(l)))
            .collect();
        let total = all.len();
        let query = self.search.read(cx).value().to_lowercase();
        let in_tab: Vec<(usize, Shape)> = all
            .into_iter()
            .filter(|(_, s)| self.tab == 0 || s.tab() == self.tab)
            .collect();
        let tab_count = in_tab.len();
        let shown: Vec<(usize, Shape)> = in_tab
            .into_iter()
            .filter(|(i, _)| query.is_empty() || lines[*i].to_lowercase().contains(&query))
            .collect();

        let search_focused = self.search.read(cx).focus_handle(cx).is_focused(window);
        let search = hit("dict-search").child(
            field_frame(search_focused)
                .h(px(22.))
                .px(px(5.))
                .gap(px(4.))
                .child(icon("plaid/search.svg", 11., hex(0x8a8a8a)))
                .child(div().flex_1().min_w_0().child(Input::new(&self.search)))
                .when(!query.is_empty(), |d| {
                    let this = this.clone();
                    d.child(hint(format!("{} 条", shown.len()))).child(
                        hit("dict-search-clear")
                            .size(px(13.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(hex(0xa8a8a8))
                            .child(icon("plaid/xmark.svg", 7., hex(0xffffff)))
                            .on_click(move |_, w, cx| {
                                let _ = this.update(cx, |this, cx| {
                                    this.search.update(cx, |s, cx| s.set_value("", w, cx));
                                    cx.notify();
                                });
                            }),
                    )
                }),
        );

        let body: AnyElement = if total == 0 {
            let this = this.clone();
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .p(px(16.))
                .child(div().font_weight(bold()).child("还没有词典条目"))
                .child(hint("添加人名、项目名和术语，或将常见错法设为替换规则。"))
                .child(div().mt(px(4.)).child(button(
                    "dict-add-first",
                    "添加第一个条目",
                    move |_, w, cx| {
                        let _ = this.update(cx, |this, cx| this.open_sheet(None, w, cx));
                    },
                )))
                .into_any_element()
        } else if shown.is_empty() && !query.is_empty() {
            div()
                .p(px(12.))
                .child(hint("没有匹配的条目，试试其他关键词。"))
                .into_any_element()
        } else {
            div()
                .id("dict-rows")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(shown.iter().enumerate().map(|(n, (i, shape))| {
                    let i = *i;
                    let selected = self.selected == Some(i);
                    let sub = if selected { 0xdfe9fa } else { HINT };
                    let (pick, edit, del) = (this.clone(), this.clone(), this.clone());
                    let title = shape.title(lines[i]);
                    let raw = matches!(shape, Shape::Raw);
                    table_row(SharedString::from(format!("entry-{i}")), n, selected)
                        .text_size(px(12.))
                        .child(
                            cell(0.)
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .flex_none()
                                        .max_w(relative(0.75))
                                        .truncate()
                                        .when(raw, |d| d.font_family("Menlo").text_size(px(11.)))
                                        .child(title),
                                )
                                .when_some(shape.block(), |d, b| {
                                    d.child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(11.))
                                            .text_color(hex(sub))
                                            .child(b),
                                    )
                                }),
                        )
                        .child(
                            cell(150.)
                                .truncate()
                                .text_size(px(11.))
                                .text_color(hex(sub))
                                .child(shape.note()),
                        )
                        .child(
                            cell(46.)
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.))
                                .child(
                                    hit(SharedString::from(format!("entry-edit-{i}")))
                                        .child(icon(
                                            "plaid/pencil.svg",
                                            11.,
                                            hex(if selected { 0xffffff } else { 0x555555 }),
                                        ))
                                        .on_click(move |_, w, cx| {
                                            cx.stop_propagation();
                                            let _ = edit.update(cx, |this, cx| {
                                                this.open_sheet(Some(i), w, cx)
                                            });
                                        }),
                                )
                                .child(
                                    hit(SharedString::from(format!("entry-del-{i}")))
                                        .child(icon(
                                            "plaid/xmark.svg",
                                            10.,
                                            hex(if selected { 0xffffff } else { 0x555555 }),
                                        ))
                                        .on_click(move |_, _, cx| {
                                            cx.stop_propagation();
                                            let _ = del.update(cx, |this, cx| this.remove(i, cx));
                                        }),
                                ),
                        )
                        .on_click(move |_, _, cx| {
                            let _ = pick.update(cx, |this, cx| {
                                this.selected = Some(i);
                                cx.notify();
                            });
                        })
                }))
                .into_any_element()
        };

        let (add, del) = (this.clone(), this.clone());
        let selected = self.selected;
        let table = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                table_well()
                    .flex_1()
                    .min_h_0()
                    .border_color(hex(0x9f9f9f))
                    .child(table_header(&[("条目", 0.), ("说明", 150.), ("", 46.)]))
                    .child(body),
            )
            .child(
                bottom_bar()
                    .border_1()
                    .border_t_0()
                    .border_color(hex(0x9f9f9f))
                    .bg(vgrad(0xfdfdfd, 0xececec))
                    .child(bar_button("dict-add", "plaid/plus.svg", move |_, w, cx| {
                        let _ = add.update(cx, |this, cx| this.open_sheet(None, w, cx));
                    }))
                    .child(
                        bar_button("dict-remove", "plaid/minus.svg", move |_, _, cx| {
                            let _ = del.update(cx, |this, cx| {
                                if let Some(i) = this.selected {
                                    this.remove(i, cx);
                                }
                            });
                        })
                        .when(selected.is_none(), |d| d.opacity(0.4)),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .pr(px(8.))
                            .child(hint(format!("{tab_count} 个"))),
                    ),
            );
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(search)
            .child(table)
    }

    fn raw_editor(&self, window: &Window, cx: &App) -> Div {
        let focused = self.raw_input.read(cx).focus_handle(cx).is_focused(window);
        let legend = [
            (
                "想要的 | 常见错法 | 错法 ~~~ 排除词",
                "别名：读音相近的片段改成想要的；附近出现排除词时不改",
            ),
            (
                "识别成 -> 想要的",
                "原文匹配，不区分大小写；右侧留空表示删除",
            ),
            ("# 注释", "不发送"),
        ];
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                hit("dict-raw-text").flex_1().min_h(px(320.)).child(
                    field_frame(focused)
                        .size_full()
                        .items_start()
                        .p(px(5.))
                        .font_family("Menlo")
                        .text_size(px(12.))
                        .child(div().size_full().child(Textarea::new(&self.raw_input))),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .children(legend.into_iter().map(|(code, text)| {
                        div()
                            .flex()
                            .gap(px(10.))
                            .child(
                                div()
                                    .w(px(210.))
                                    .flex_none()
                                    .font_family("Menlo")
                                    .text_size(px(11.))
                                    .child(code),
                            )
                            .child(hint(text).flex_1().min_w_0())
                    }))
                    .child(hint("最多约 100 token 发给语音识别。")),
            )
    }

    fn left(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        let content = if self.raw {
            self.raw_editor(window, cx)
        } else {
            self.entries(window, cx)
        };
        let tab_pick = this.clone();
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .pt(px(11.))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .pt(px(24.))
                    .px(px(14.))
                    .pb(px(14.))
                    .rounded(px(5.))
                    .bg(hexa(0x0000000a))
                    .border_1()
                    .border_color(hexa(0x00000024))
                    .child(hint(TAB_HINTS[self.tab]))
                    .child(content)
                    .child(div().flex().justify_end().child(button(
                        "dict-raw",
                        if self.raw {
                            "完成"
                        } else {
                            "编辑原文…"
                        },
                        move |_, w, cx| {
                            let _ = this.update(cx, |this, cx| this.set_raw(!this.raw, w, cx));
                        },
                    )))
                    // Last child, so the strip paints over the box edge it straddles.
                    .child(
                        div()
                            .absolute()
                            .top(px(-12.))
                            .left_0()
                            .right_0()
                            .flex()
                            .justify_center()
                            .child(tabs("dict-kind", &TABS, self.tab, move |i, _, cx| {
                                let _ = tab_pick.update(cx, |this, cx| {
                                    this.tab = i;
                                    this.selected = None;
                                    cx.notify();
                                });
                            })),
                    ),
            )
    }

    fn suggestions(&self, d: &Derived, cx: &mut Context<Self>) -> Option<Div> {
        if d.fixes.is_empty() {
            return None;
        }
        let this = cx.entity().downgrade();
        let lines: HashSet<&str> = self.text.split('\n').collect();
        let rows = d.fixes.iter().enumerate().map(|(i, f)| {
            let has_word = lines.contains(f.to.as_str());
            let (dismiss, alias, word) = (this.clone(), this.clone(), this.clone());
            let (key, alias_line, to) = (
                rule(&f.from, &f.to),
                format!("{} | {}", f.to, f.from),
                f.to.clone(),
            );
            let when = f.last.with_timezone(&Local).format("%-m月%-d日");
            let add_word = default_button(
                SharedString::from(format!("fix-word-{i}")),
                "加热词",
                move |_, _, cx| {
                    if !has_word {
                        let _ = word.update(cx, |this, cx| this.append(&to, cx));
                    }
                },
            )
            .h(px(18.))
            .px(px(9.))
            .text_size(px(11.));
            div()
                .flex()
                .flex_col()
                .gap(px(5.))
                .py(px(8.))
                .when(i == 0, |d| d.pt_0())
                .when(i > 0, |d| d.border_t_1().border_color(hexa(0x0000001a)))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(6.))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .line_through()
                                .text_color(hex(0x7a7a7a))
                                .child(f.from.clone()),
                        )
                        .child(div().flex_none().text_color(hex(GREEN)).child("→"))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_weight(bold())
                                .child(f.to.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .ml_auto()
                                .child(hint(format!("{} 次 · {when}", f.count))),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            hint(f.context.split('\n').collect::<Vec<_>>().join(" "))
                                .flex_1()
                                .min_w_0()
                                .truncate(),
                        )
                        .child(small_button(
                            SharedString::from(format!("fix-dismiss-{i}")),
                            "忽略",
                            move |_, _, cx| {
                                let _ = dismiss.update(cx, |this, cx| {
                                    this.dismiss(std::slice::from_ref(&key), cx);
                                    cx.notify();
                                });
                            },
                        ))
                        .child(small_button(
                            SharedString::from(format!("fix-alias-{i}")),
                            "加别名",
                            move |_, _, cx| {
                                let _ = alias.update(cx, |this, cx| this.append(&alias_line, cx));
                            },
                        ))
                        .child(if has_word {
                            disabled(add_word)
                        } else {
                            add_word
                        }),
                )
                .into_any_element()
        });
        let (toggle, close, pick) = (this.clone(), this.clone(), this.clone());
        let footer = div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(8.))
            .pt(px(8.))
            .border_t_1()
            .border_color(hexa(0x0000001a))
            .child(hint(
                "别名会将读音相近的错法改为目标词；最近 10 条建议也会作为 {{corrections}} 交给优化。",
            ).w_full())
            .when(d.fixes.len() > 1, |el| {
                el.child(pick_list(
                    "fix-batch",
                    96.,
                    "批量处理",
                    ["全部加为热词", "全部加为别名", "全部忽略"]
                        .into_iter()
                        .map(|l| MenuEntry {
                            label: l.into(),
                            checked: false,
                        })
                        .collect(),
                    self.batch_open,
                    move |_, cx| {
                        let _ = toggle.update(cx, |this, cx| {
                            this.batch_open = !this.batch_open;
                            cx.notify();
                        });
                    },
                    move |_, cx| {
                        let _ = close.update(cx, |this, cx| {
                            this.batch_open = false;
                            cx.notify();
                        });
                    },
                    move |i, _, cx| {
                        let _ = pick.update(cx, |this, cx| this.batch(i, cx));
                    },
                ))
            });
        Some(
            group(format!("待处理建议（{}）", d.fixes.len()))
                .child(group_body().gap(px(0.)).children(rows).child(footer)),
        )
    }

    fn autos(&self, d: &Derived, cx: &mut Context<Self>) -> Option<Div> {
        if d.autos.is_empty() {
            return None;
        }
        let this = cx.entity().downgrade();
        let rows = d.autos.iter().enumerate().map(|(i, (from, to, n))| {
            let this = this.clone();
            let key = rule(from, to);
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(div().text_color(hex(0x7a7a7a)).child(from.clone()))
                .child(div().text_color(hex(PURPLE)).font_weight(bold()).child("→"))
                .child(div().font_weight(bold()).child(to.clone()))
                .child(hint(format!("{n} 次")))
                .child(
                    hit(SharedString::from(format!("auto-dismiss-{i}")))
                        .ml_auto()
                        .p(px(3.))
                        .child(icon("plaid/xmark.svg", 9., hex(0x666666)))
                        .on_click(move |_, _, cx| {
                            let _ = this.update(cx, |this, cx| this.dismiss_auto(key.clone(), cx));
                        }),
                )
                .into_any_element()
        });
        Some(group(format!("自动规则（{}）", d.autos.len())).child(
            group_body().children(rows).child(hint(
                "同一个改法出现 2 次以上后自动生效，存放在 hotwords.auto.txt；忽略后不再出现。",
            )),
        ))
    }

    fn learning(&self, cx: &mut Context<Self>) -> Div {
        let switch = |key: &'static str, label: &'static str, note: AnyElement| {
            let model = self.model.clone();
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(checkbox(
                    key,
                    on_unless_zero(key),
                    label,
                    move |v, _, cx| {
                        model.update(cx, |m, cx| m.set(key, bool_str(v), cx));
                    },
                ))
                .child(div().pl(px(20.)).child(note))
        };
        let note = if self.no_llm() {
            let nav = self.nav.clone();
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(6.))
                .child(hint("没有可用的 LLM：在「优化」里填好接口和 key").text_color(hex(ORANGE)))
                .child(small_button(
                    "open-refine",
                    "打开优化",
                    move |_, _, cx| {
                        let _ = nav.update(cx, |s, cx| s.show(Page::Refine, cx));
                    },
                ))
                .into_any_element()
        } else {
            hint("独立于优化开关：优化关闭时，仍将每句文本发送到配置的 LLM 校对，只用来发现规则，不改粘贴结果")
                .into_any_element()
        };
        let _ = cx;
        group("学习").child(
            group_body()
                .gap(px(10.))
                .child(switch(
                    "learnEdits",
                    "从手动修改中学习",
                    hint("通过辅助功能读取你改过的输入框，和粘贴内容对比得出建议")
                        .into_any_element(),
                ))
                .child(switch("autoLearn", "后台校对", note)),
        )
    }

    fn sheet_view(&self, sheet: &Sheet, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let this = cx.entity().downgrade();
        let label = |t: &'static str| div().text_size(px(12.)).font_weight(bold()).child(t);
        let input =
            |id: &'static str, s: &Entity<InputState>| hit(id).child(field(s, 478., window, cx));
        let mut fields: Vec<AnyElement> = vec![];
        match sheet.kind {
            Kind::Word => {
                fields.push(label("目标词").into_any_element());
                fields.push(input("entry-word", &sheet.word).into_any_element());
                fields.push(hint("用于语音识别和优化，也参与读音相近的纠错。").into_any_element());
            }
            Kind::Replace => {
                fields.push(label("识别到的原文").into_any_element());
                fields.push(input("entry-from", &sheet.from).into_any_element());
                fields.push(label("替换为").mt(px(4.)).into_any_element());
                fields.push(input("entry-to", &sheet.to).into_any_element());
                fields.push(
                    hint("不区分大小写，替换文本中所有匹配的原文；不是读音匹配。")
                        .into_any_element(),
                );
                let from = sheet.from.read(cx).value();
                let to = sheet.to.read(cx).value();
                if !trim_ws(&from).is_empty() && trim_ws(&to).is_empty() {
                    fields.push(
                        hint("⚠ 替换内容为空，匹配到的原文会被删除。")
                            .text_color(hex(ORANGE))
                            .into_any_element(),
                    );
                }
            }
            Kind::Advanced => {
                fields.push(
                    input("entry-line", &sheet.raw)
                        .font_family("Menlo")
                        .into_any_element(),
                );
                fields.push(
                    hint("保留单行语法。别名示例：Claude | 克劳德；排除词示例：Plaid | 普莱德 ~~~ 格纹。多行和注释也可在词典页的「编辑原文」中修改。")
                        .into_any_element(),
                );
            }
        }
        let validation = sheet.validation(cx);
        let message = match (sheet.error, validation) {
            (Some(e), _) => Some(hint(e).text_color(hex(ORANGE))),
            (None, Some(v)) => Some(hint(v)),
            _ => None,
        };
        let kind = [Kind::Word, Kind::Replace, Kind::Advanced]
            .iter()
            .position(|k| *k == sheet.kind)
            .unwrap_or(0);
        let (pick, cancel, save) = (this.clone(), this.clone(), this.clone());
        let save_button = default_button("entry-save", "保存", move |_, w, cx| {
            let _ = save.update(cx, |this, cx| this.save_sheet(w, cx));
        });
        sheet_frame("entry-sheet")
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(bold())
                    .child(if sheet.index.is_none() {
                        "添加词典条目"
                    } else {
                        "编辑词典条目"
                    }),
            )
            .child(div().flex().justify_center().child(segmented(
                "entry-kind",
                &KINDS,
                kind,
                move |i, w, cx| {
                    let _ = pick.update(cx, |this, cx| this.set_kind(i, w, cx));
                },
            )))
            .child(div().flex().flex_col().gap(px(6.)).children(fields))
            .children(message)
            .child(div().h(px(1.)).bg(hexa(0x0000001f)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(10.))
                    .child(button("entry-cancel", "取消", move |_, _, cx| {
                        let _ = cancel.update(cx, |this, cx| this.close_sheet(cx));
                    }))
                    .child(if validation.is_some() {
                        disabled(default_button("entry-save", "保存", |_, _, _| {}))
                    } else {
                        save_button
                    }),
            )
    }

    fn confirm_view(&self, n: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let this = cx.entity().downgrade();
        let (cancel, ok) = (this.clone(), this);
        sheet_frame("confirm-sheet")
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(bold())
                    .child(format!("忽略全部 {n} 条待处理建议？")),
            )
            .child(hint(
                "这些建议将不再出现在待处理列表中。已有热词和替换规则不会删除。",
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(10.))
                    .child(button("confirm-cancel", "取消", move |_, _, cx| {
                        let _ = cancel.update(cx, |this, cx| this.close_sheet(cx));
                    }))
                    .child(default_button(
                        "confirm-ok",
                        "全部忽略",
                        move |_, _, cx| {
                            let _ = ok.update(cx, |this, cx| this.dismiss_all(cx));
                        },
                    )),
            )
    }
}

/// A sheet hanging from the top of the page: window-colored panel, square top, soft shadow.
fn sheet_frame(id: &'static str) -> Stateful<Div> {
    hit(id)
        .w(px(520.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .px(px(20.))
        .pt(px(18.))
        .pb(px(16.))
        .bg(hex(WINDOW))
        .border_1()
        .border_t_0()
        .border_color(hexa(0x00000040))
        .rounded_b(px(6.))
        .shadow(vec![BoxShadow {
            color: hexa(0x00000059),
            offset: point(px(0.), px(8.)),
            blur_radius: px(24.),
            spread_radius: px(0.),
            inset: false,
        }])
}

impl Render for Dictionary {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "snapshot")]
        for f in POKES.with(|p| p.take()) {
            let me = cx.entity();
            window.defer(cx, move |w, cx| f(me, w, cx));
        }
        self.reload();
        if self.raw && self.raw_input.read(cx).value().as_ref() != self.text {
            let text = self.text.clone();
            self.raw_input
                .update(cx, |s, cx| s.set_value(text, window, cx));
        }
        let d = self.derived(cx);
        let this = cx.entity().downgrade();
        let overlay = if let Some(sheet) = &self.sheet {
            Some(self.sheet_view(sheet, window, cx))
        } else if self.confirm {
            Some(self.confirm_view(d.fixes.len(), cx))
        } else {
            None
        };
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(12.))
            .pt(px(16.))
            .px(px(22.))
            .pb(px(18.))
            .when_some(self.error.clone(), |el, e| {
                let this = this.clone();
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(5.))
                        .bg(hexa(0xff9f0a1f))
                        .border_1()
                        .border_color(hexa(0xc25e0066))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(hex(0x8a4300))
                                .child(format!("热词保存失败：{e}。草稿已保留。")),
                        )
                        .child(button("dict-retry", "重试", move |_, _, cx| {
                            let _ = this.update(cx, |this, cx| {
                                this.flush(cx);
                                cx.notify();
                            });
                        })),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap(px(18.))
                    .child(self.left(window, cx))
                    .child(
                        div()
                            .id("dict-side")
                            .w(px(290.))
                            .flex_none()
                            .h_full()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .children(self.suggestions(&d, cx))
                            .children(self.autos(&d, cx))
                            .child(self.learning(cx)),
                    ),
            )
            .when_some(overlay, |el, sheet| {
                let esc = this.clone();
                el.child(
                    div()
                        .id("dict-overlay")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .bg(hexa(0x00000010))
                        .flex()
                        .items_start()
                        .justify_center()
                        .on_key_down(move |e: &KeyDownEvent, _, cx| {
                            if e.keystroke.key == "escape" {
                                let _ = esc.update(cx, |this, cx| this.close_sheet(cx));
                            }
                        })
                        .child(sheet),
                )
            })
    }
}

// --- Offscreen checks ------------------------------------------------------------------------------------------

#[cfg(feature = "snapshot")]
type Poke = Box<dyn FnOnce(Entity<Dictionary>, &mut Window, &mut App)>;

#[cfg(feature = "snapshot")]
thread_local! {
    /// Work for the next render of the page, run outside any entity update (typing, reading view state).
    static POKES: std::cell::RefCell<Vec<Poke>> = Default::default();
}

#[cfg(feature = "snapshot")]
fn poke(
    ui: &mut super::snapshot::Ui,
    f: impl FnOnce(Entity<Dictionary>, &mut Window, &mut App) + 'static,
) -> Result<(), String> {
    POKES.with(|p| p.borrow_mut().push(Box::new(f)));
    ui.frame()?;
    ui.frame()
}

/// Types into the focused field as one keystroke carrying the text (as an IME commit would).
#[cfg(feature = "snapshot")]
fn type_text(ui: &mut super::snapshot::Ui, text: &str) -> Result<(), String> {
    let handled = Rc::new(std::cell::Cell::new(false));
    let (h, text) = (handled.clone(), text.to_owned());
    poke(ui, move |_, w, cx| {
        h.set(w.dispatch_keystroke(
            Keystroke {
                modifiers: Default::default(),
                key: "a".into(),
                key_char: Some(text),
            },
            cx,
        ));
    })?;
    super::snapshot::expect("keystroke reached a text field", handled.get(), &None)
}

#[cfg(feature = "snapshot")]
fn hotwords_file() -> String {
    read(path())
}

#[cfg(feature = "snapshot")]
pub(super) fn check(ui: &mut super::snapshot::Ui) -> Result<(), String> {
    use super::snapshot::expect;
    let facts = || super::Facts {
        volc_key: true,
        mics: vec![],
        default_mic: None,
        trusted: true,
        fixture: true,
    };
    let has_line = |l: &str| hotwords_file().split('\n').any(|x| x == l);
    let dismissed_has = |k: &str| dismissed().contains(k);

    ui.open(Page::Dictionary, facts())?;
    ui.shot("dictionary")?;
    expect(
        "pending suggestions listed",
        ui.has("fix-word-0") && ui.has("fix-word-1") && !ui.has("fix-word-2"),
        &None,
    )?;
    expect("learned rule listed", ui.has("auto-dismiss-0"), &None)?;
    expect("no-LLM hint with 打开优化", ui.has("open-refine"), &None)?;

    // Batch menu → 全部忽略 asks first; cancel leaves dismissedFixes alone.
    ui.click("fix-batch")?;
    expect("batch menu open", ui.has("fix-batch-menu-2"), &None)?;
    ui.shot("dictionary-batch")?;
    ui.click("fix-batch-menu-2")?;
    expect("confirm sheet", ui.has("confirm-ok"), &None)?;
    ui.shot("dictionary-confirm")?;
    let before = config::raw("dismissedFixes");
    ui.click("confirm-cancel")?;
    expect(
        "cancel keeps dismissedFixes",
        config::raw("dismissedFixes") == before && !ui.has("confirm-ok"),
        &before,
    )?;

    // Row 0 is cooper netes → Kubernetes (3 times): Kubernetes is already a line, so 加热词 is disabled.
    let body = hotwords_file();
    ui.click("fix-word-0")?;
    expect(
        "disabled 加热词 writes nothing",
        hotwords_file() == body,
        &None,
    )?;
    // Row 1 is 瑞迪斯 → Redis.
    ui.click("fix-word-1")?;
    expect(
        "加热词 appends Redis",
        has_line("Redis"),
        &Some(body.clone()),
    )?;
    // As in Swift, a hotword does not cover the suggestion: it stays, with 加热词 disabled.
    let body = hotwords_file();
    ui.click("fix-word-1")?;
    expect("加热词 now disabled", hotwords_file() == body, &None)?;
    ui.click("fix-alias-0")?;
    expect(
        "加别名 appends the alias line",
        has_line("Kubernetes | cooper netes"),
        &Some(hotwords_file()),
    )?;
    expect(
        "aliased fix leaves the list",
        ui.has("fix-word-0") && !ui.has("fix-word-1"),
        &None,
    )?;
    // The one left is 瑞迪斯 → Redis; ignoring it records it.
    ui.click("fix-dismiss-0")?;
    expect(
        "忽略 records the rule",
        dismissed_has("瑞迪斯 -> Redis") && !ui.has("fix-word-0"),
        &config::raw("dismissedFixes"),
    )?;

    // Learned rule 杰森 → JSON: ignoring it records it and regenerates hotwords.auto.txt without it.
    let auto = config::data_dir().join(AUTO_RULES);
    expect(
        "auto file has the learned rule",
        read(&auto).contains("杰森 -> JSON"),
        &None,
    )?;
    ui.click("auto-dismiss-0")?;
    expect(
        "auto rule ignored",
        dismissed_has("杰森 -> JSON") && !read(&auto).contains("杰森 -> JSON"),
        &Some(read(&auto)),
    )?;
    expect("auto section gone", !ui.has("auto-dismiss-0"), &None)?;

    // Learning switches.
    for key in ["learnEdits", "autoLearn"] {
        ui.click(key)?;
        expect(
            &format!("{key} off"),
            config::raw(key).as_deref() == Some("0"),
            &None,
        )?;
        ui.click(key)?;
        expect(
            &format!("{key} on"),
            config::raw(key).as_deref() == Some("1"),
            &None,
        )?;
    }
    ui.click("autoLearn")?;
    expect("no-LLM hint hidden when off", !ui.has("open-refine"), &None)?;
    ui.click("autoLearn")?;

    // Tabs filter by kind.
    ui.click("dict-kind-3")?;
    let lines: Vec<String> = hotwords_file().split('\n').map(str::to_owned).collect();
    let claude = lines.iter().position(|l| l == "克劳德 -> Claude");
    let plaid = lines.iter().position(|l| l == "Plaid");
    expect(
        "替换 tab shows only replacements",
        matches!((claude, plaid), (Some(c), Some(p)) if ui.has(&format!("entry-{c}")) && !ui.has(&format!("entry-{p}"))),
        &None,
    )?;
    ui.shot("dictionary-replace")?;
    ui.click("dict-kind-0")?;

    // Search, then delete the one match: the delete waits for the debounce, and closing the window saves it.
    ui.click("dict-search")?;
    type_text(ui, "gpui")?;
    let gpui = lines.iter().position(|l| l == "GPUI").ok_or("GPUI line")?;
    expect(
        "search narrows rows",
        ui.has(&format!("entry-{gpui}")) && plaid.is_some_and(|p| !ui.has(&format!("entry-{p}"))),
        &None,
    )?;
    ui.shot("dictionary-search")?;
    ui.click(&format!("entry-del-{gpui}"))?;
    expect(
        "deleted row gone from the page",
        !ui.has(&format!("entry-{gpui}")),
        &None,
    )?;
    let debounced = has_line("GPUI");
    println!("  ..  delete before debounce: GPUI still on disk = {debounced}");
    ui.close()?;
    expect(
        "close saves the draft",
        !has_line("GPUI"),
        &Some(hotwords_file()),
    )?;

    // Add through the sheet.
    ui.open(Page::Dictionary, facts())?;
    ui.click("dict-add")?;
    expect("add sheet open", ui.has("entry-word"), &None)?;
    type_text(ui, "张小明")?;
    ui.shot("dictionary-add")?;
    ui.click("entry-save")?;
    expect(
        "sheet saved a hotword",
        has_line("张小明") && !ui.has("entry-sheet"),
        &Some(hotwords_file()),
    )?;

    // Duplicate entry is refused.
    ui.click("dict-add")?;
    type_text(ui, "Plaid")?;
    ui.click("entry-save")?;
    expect(
        "duplicate refused",
        ui.has("entry-sheet")
            && hotwords_file().matches("\nPlaid\n").count()
                + usize::from(hotwords_file().starts_with("Plaid\n"))
                == 1,
        &None,
    )?;
    ui.shot("dictionary-duplicate")?;

    // Replacement with an empty right side warns but saves as a delete rule.
    ui.click("entry-kind-1")?;
    expect(
        "replacement fields",
        ui.has("entry-from") && ui.has("entry-to"),
        &None,
    )?;
    type_text(ui, "嗯嗯")?;
    ui.shot("dictionary-empty-to")?;
    ui.click("entry-save")?;
    expect(
        "delete rule saved",
        has_line("嗯嗯 -> "),
        &Some(hotwords_file()),
    )?;

    // Editing an advanced line opens 高级原文 with the line; Esc closes.
    let lines: Vec<String> = hotwords_file().split('\n').map(str::to_owned).collect();
    let tauri = lines
        .iter()
        .position(|l| l.starts_with("Tauri |"))
        .ok_or("Tauri line")?;
    ui.click(&format!("entry-edit-{tauri}"))?;
    expect(
        "alias line edits as 读音别名 in 高级原文",
        ui.has("entry-line") && !ui.has("entry-word"),
        &None,
    )?;
    ui.shot("dictionary-edit")?;
    ui.click("entry-cancel")?;
    expect("cancel closes the sheet", !ui.has("entry-sheet"), &None)?;

    // Raw mode: typing saves on close.
    ui.click("dict-raw")?;
    expect("raw editor", ui.has("dict-raw-text"), &None)?;
    ui.click("dict-raw-text")?;
    type_text(ui, "\nZed\n")?;
    ui.shot("dictionary-raw")?;
    ui.close()?;
    expect(
        "raw edit saved on close",
        has_line("Zed"),
        &Some(hotwords_file()),
    )?;

    // 打开优化 switches pages.
    ui.open(Page::Dictionary, facts())?;
    ui.click("open-refine")?;
    expect("打开优化 leaves the page", !ui.has("dict-add"), &None)?;
    ui.close()?;

    // Empty dictionary.
    let body = hotwords_file();
    write_atomic(&path(), b"").map_err(|e| e.to_string())?;
    ui.open(Page::Dictionary, facts())?;
    expect("empty state", ui.has("dict-add-first"), &None)?;
    ui.shot("dictionary-empty")?;
    ui.close()?;
    write_atomic(&path(), body.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}
