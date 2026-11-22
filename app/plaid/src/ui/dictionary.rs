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
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

/// Pill font sizes by recent use: unused, then the lower, middle and top of the used range.
const PILL_SIZES: [f32; 4] = [12., 14., 17., 21.];

/// Index into PILL_SIZES for a word used `n` times when the most used word has `max`.
fn size_level(n: usize, max: usize) -> usize {
    match n {
        0 => 0,
        _ if n * 10 >= max * 6 => 3,
        _ if n * 10 >= max * 3 => 2,
        _ => 1,
    }
}

const KINDS: [&str; 3] = ["热词", "替换规则", "高级原文"];

fn path() -> PathBuf {
    config::data_dir().join("hotwords.txt")
}

fn read(p: impl AsRef<Path>) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// Adds one hotword line unless hotwords.txt already has it (History's 加入词典); Ok(false) when it was there.
pub(super) fn add_word(word: &str, store: &Store) -> std::io::Result<bool> {
    let mut text = read(path());
    if text.split('\n').any(|l| l.trim() == word) {
        return Ok(false);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(word);
    text.push('\n');
    write_atomic(&path(), text.as_bytes())?;
    store.write_auto_rules(&config::terms(false), &dismissed());
    Ok(true)
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
pub(super) fn pending(model: &Model) -> usize {
    let autos = model.store.auto_rules(&config::terms(false), &dismissed());
    pending_fixes(&model.store, &read(path()), &autos).len()
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
    /// Records of the last 30 days whose pasted text contains the word, keyed by the lowercased word.
    uses: HashMap<String, usize>,
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
    search: Entity<InputState>,
    raw: bool,
    raw_input: Entity<TextareaState>,
    batch_open: bool,
    confirm: bool,
    sheet: Option<Sheet>,
    /// A DeepSeek key for background checking (env or keychain), read once.
    fallback_key: bool,
    /// Focused while the confirm sheet is up, so the overlay gets Esc.
    overlay_focus: FocusHandle,
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
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("输入新词，回车添加"));
        cx.subscribe_in(
            &search,
            window,
            |this, s, e: &InputEvent, window, cx| match e {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => {
                    let line = trim_ws(&s.read(cx).value()).to_owned();
                    if !line.is_empty() {
                        this.append(&line, cx);
                        s.update(cx, |s, cx| s.set_value("", window, cx));
                    }
                }
                _ => {}
            },
        )
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
            search,
            raw: false,
            raw_input,
            batch_open: false,
            confirm: false,
            sheet: None,
            fallback_key,
            overlay_focus: cx.focus_handle(),
        }
    }

    // --- Draft -------------------------------------------------------------------------------------------------

    /// Writes the draft now; regenerates the learned rules against it.
    fn flush(&mut self, cx: &mut App) {
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
                let _ = self.nav.update(cx, |s, cx| s.count_pending(cx));
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
        let since = chrono::Utc::now() - chrono::Duration::days(30);
        let texts: Vec<String> = store
            .all
            .iter()
            .filter(|r| r.at >= since)
            .map(|r| r.text.to_lowercase())
            .collect();
        let mut uses = HashMap::new();
        let words = self
            .shapes()
            .into_iter()
            .filter_map(|(_, s)| match s {
                Shape::Word { target, .. } => Some(target),
                _ => None,
            })
            .chain(autos.iter().map(|(_, to, _)| to.clone()));
        for w in words {
            let w = w.to_lowercase();
            if !w.is_empty() && !uses.contains_key(&w) {
                let n = texts.iter().filter(|t| t.contains(&w)).count();
                uses.insert(w, n);
            }
        }
        let d = Rc::new(Derived { fixes, autos, uses });
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

    fn batch(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.batch_open = false;
        let fixes = self.derived(cx).fixes.clone();
        match i {
            0 => self.accept(&fixes, false, cx),
            1 => self.accept(&fixes, true, cx),
            _ => {
                // Nothing in the confirm sheet takes focus; the overlay does, so Esc reaches it.
                self.confirm = true;
                self.overlay_focus.focus(window, cx);
            }
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

    /// Keeps a learned rule as a line of the user's own file; ignoring it drops it from the learned set.
    fn keep_auto(&mut self, line: &str, cx: &mut Context<Self>) {
        self.append(line, cx);
        self.dismiss_auto(line.to_owned(), cx);
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
                        .font(mono())
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
                                    .font(mono())
                                    .text_size(px(11.))
                                    .child(code),
                            )
                            .child(hint(text).flex_1().min_w_0())
                    }))
                    .child(hint("最多约 100 token 发给语音识别。")),
            )
    }

    /// Non-empty, non-comment lines with their line numbers.
    fn shapes(&self) -> Vec<(usize, Shape)> {
        self.text
            .split('\n')
            .enumerate()
            .filter(|(_, l)| {
                let t = trim_ws(l);
                !t.is_empty() && !t.starts_with('#')
            })
            .map(|(i, l)| (i, Shape::of(l)))
            .collect()
    }

    /// The wall: hotwords as pills, sized by recent use. Typing filters it, Enter adds the typed line.
    fn words_card(&self, d: &Derived, window: &Window, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        let query = self.search.read(cx).value().to_lowercase();
        let query = trim_ws(&query).to_owned();
        let shapes = self.shapes();
        // A word is tinted when something rewrites into it: its own aliases, a replacement or a learned rule.
        let rewritten = |w: &str| {
            d.autos.iter().any(|(_, to, _)| to == w)
                || shapes
                    .iter()
                    .any(|(_, s)| matches!(s, Shape::Replace { to, .. } if to == w))
        };
        let words: Vec<(usize, String, bool)> = shapes
            .iter()
            .filter_map(|(i, s)| match s {
                Shape::Word {
                    target, aliases, ..
                } => Some((*i, target.clone(), !aliases.is_empty() || rewritten(target))),
                _ => None,
            })
            .collect();
        let total = words.len();
        let uses = |w: &str| d.uses.get(&w.to_lowercase()).copied().unwrap_or(0);
        let max = words.iter().map(|(_, w, _)| uses(w)).max().unwrap_or(0);
        let matches = |w: &str| query.is_empty() || w.to_lowercase().contains(&query);

        let focused = self.search.read(cx).focus_handle(cx).is_focused(window);
        let input = hit("dict-search").w(px(220.)).flex_none().child(
            div()
                .h(px(32.))
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded_full()
                .bg(hex(WELL))
                .border_1()
                .border_color(if focused {
                    tint(CYAN, 0x80)
                } else {
                    hexa(LINE2)
                })
                .child(div().text_size(px(16.)).text_color(hex(CYAN)).child("+"))
                .child(div().flex_1().min_w_0().child(Input::new(&self.search))),
        );
        let legend_dot = |tinted: bool| {
            let d = div().size(px(8.)).rounded_full();
            if tinted {
                d.bg(hex(CYAN))
            } else {
                d.border_1().border_color(hex(TEXT2))
            }
        };
        let legend = div()
            .flex()
            .items_center()
            .gap(px(14.))
            .text_size(px(11.))
            .text_color(hex(TEXT2))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(legend_dot(true))
                    .child("带改写规则"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(legend_dot(false))
                    .child("只做热词"),
            )
            .child(div().ml_auto().child("字越大，最近用得越多 · 点词编辑"));

        let pill = |id: SharedString, text: String, tinted: bool| {
            let level = size_level(uses(&text), max);
            let big = level >= 2;
            let p = hit(id)
                .px(px(12.))
                .py(px(5.))
                .rounded_full()
                .border_1()
                .text_size(px(PILL_SIZES[level]))
                .whitespace_nowrap()
                .child(text);
            let p = p.cursor_pointer();
            if tinted {
                p.bg(tint(CYAN, if big { 0x22 } else { 0x12 }))
                    .border_color(tint(CYAN, if big { 0x66 } else { 0x33 }))
                    .text_color(hex(if big { TEXT } else { 0xc9cbd1 }))
                    .hover(|s| s.bg(tint(CYAN, 0x2c)))
            } else {
                p.border_color(hexa(LINE3))
                    .text_color(hex(if big { TEXT } else { 0xa4a7ae }))
                    .hover(|s| s.bg(hexa(0xffffff0d)).border_color(hexa(0xffffff3d)))
            }
        };
        let pills: Vec<AnyElement> = words
            .iter()
            .filter(|(_, w, _)| matches(w))
            .map(|(i, w, tinted)| {
                let (i, edit) = (*i, this.clone());
                pill(SharedString::from(format!("word-{i}")), w.clone(), *tinted)
                    .on_click(move |_, w, cx| {
                        let _ = edit.update(cx, |this, cx| this.open_sheet(Some(i), w, cx));
                    })
                    .into_any_element()
            })
            .collect();
        let body = if total == 0 {
            hint("还没有词。在上面输入专有名词、人名或英文术语，回车添加。")
        } else if pills.is_empty() {
            hint(format!("没有匹配的词，回车把「{query}」加进词典。"))
        } else {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .children(pills)
        };
        group_body()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .font(mono())
                            .text_size(px(34.))
                            .line_height(px(40.))
                            .font_weight(bold())
                            .child(total.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(hex(TEXT2))
                            .child("个词 · 发给火山做热词，也发给 LLM 参考"),
                    )
                    .child(input),
            )
            .when(total > 0, |el| el.child(legend))
            .child(body)
    }

    /// Aliases, replacements and lines only the raw syntax can show, one row each.
    fn rules_card(&self, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        let query = self.search.read(cx).value().to_lowercase();
        let query = trim_ws(&query).to_owned();
        let lines: Vec<&str> = self.text.split('\n').collect();
        let rules: Vec<(usize, Shape)> = self
            .shapes()
            .into_iter()
            .filter(|(_, s)| !matches!(s, Shape::Word { aliases, .. } if aliases.is_empty()))
            .collect();
        let shown: Vec<&(usize, Shape)> = rules
            .iter()
            .filter(|(i, _)| query.is_empty() || lines[*i].to_lowercase().contains(&query))
            .collect();
        let rows = shown.iter().enumerate().map(|(n, (i, shape))| {
            let i = *i;
            let (pick, edit, del) = (this.clone(), this.clone(), this.clone());
            let (from, to, kind): (String, Option<String>, &str) = match shape {
                Shape::Word {
                    target, aliases, ..
                } => (aliases.join(" / "), Some(target.clone()), "读音别名"),
                Shape::Replace { from, to } => (from.clone(), Some(to.clone()), "替换"),
                Shape::Raw => (trim_ws(lines[i]).to_owned(), None, "原文"),
            };
            let icon_button = |id: String, path: &'static str, size: f32| {
                hit(SharedString::from(id))
                    .size(px(22.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .hover(|s| s.bg(hex(HOVER)))
                    .child(icon(path, size, hex(TEXT3)))
            };
            hit(SharedString::from(format!("entry-{i}")))
                .flex()
                .items_center()
                .gap(px(10.))
                .min_h(px(36.))
                .py(px(6.))
                .mx(px(-10.))
                .px(px(10.))
                .rounded(px(8.))
                .when(n > 0, |d| d.border_t_1().border_color(hexa(LINE)))
                .cursor_pointer()
                .hover(|s| s.bg(hexa(0xffffff08)))
                .map(|d| match &to {
                    Some(to) => d
                        .child(
                            div()
                                .w(px(130.))
                                .flex_none()
                                .truncate()
                                .text_color(hex(TEXT2))
                                .child(from.clone()),
                        )
                        .child(div().flex_none().text_color(hex(TEXT3)).child("→"))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_baseline()
                                .gap(px(8.))
                                .child(if to.is_empty() {
                                    div().flex_none().text_color(hex(RED)).child("（删除）")
                                } else {
                                    div()
                                        .flex_none()
                                        .max_w(relative(0.6))
                                        .truncate()
                                        .text_color(hex(CYAN))
                                        .child(to.clone())
                                })
                                .when_some(shape.block(), |d, b| {
                                    d.child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(11.))
                                            .text_color(hex(TEXT3))
                                            .child(b),
                                    )
                                }),
                        ),
                    None => d.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font(mono())
                            .text_size(px(11.))
                            .text_color(hex(TEXT2))
                            .child(from.clone()),
                    ),
                })
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(hex(TEXT3))
                        .child(kind),
                )
                .child(
                    icon_button(format!("entry-edit-{i}"), "plaid/pencil.svg", 11.).on_click(
                        move |_, w, cx| {
                            cx.stop_propagation();
                            let _ = edit.update(cx, |this, cx| this.open_sheet(Some(i), w, cx));
                        },
                    ),
                )
                .child(
                    icon_button(format!("entry-del-{i}"), "plaid/xmark.svg", 10.).on_click(
                        move |_, _, cx| {
                            cx.stop_propagation();
                            let _ = del.update(cx, |this, cx| this.remove(i, cx));
                        },
                    ),
                )
                .on_click(move |_, w, cx| {
                    let _ = pick.update(cx, |this, cx| this.open_sheet(Some(i), w, cx));
                })
                .into_any_element()
        });
        let rows: Vec<AnyElement> = rows.collect();
        let (add, raw) = (this.clone(), this);
        let body = if rules.is_empty() {
            hint("还没有改写规则。待处理建议里点「加别名」，或在这里新建。").into_any_element()
        } else if rows.is_empty() {
            hint("没有匹配的规则。").into_any_element()
        } else {
            div().flex().flex_col().children(rows).into_any_element()
        };
        group_body()
            .gap(px(8.))
            .child(
                card_head("改写规则", "听错的说法改成想要的词，改动自动保存")
                    .items_center()
                    .child(small_button("dict-add", "+ 新建", move |_, w, cx| {
                        let _ = add.update(cx, |this, cx| {
                            this.open_sheet(None, w, cx);
                            this.set_kind(1, w, cx);
                        });
                    }))
                    .child(small_button(
                        "dict-raw",
                        "编辑原文…",
                        move |_, w, cx| {
                            let _ = raw.update(cx, |this, cx| this.set_raw(true, w, cx));
                        },
                    )),
            )
            .child(body)
    }

    /// The whole file as text, with the syntax legend.
    fn raw_card(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        group_body()
            .flex_1()
            .min_h(px(480.))
            .child(
                card_head("编辑原文", "hotwords.txt，一行一个条目")
                    .items_center()
                    .child(default_button("dict-raw", "完成", move |_, w, cx| {
                        let _ = this.update(cx, |this, cx| this.set_raw(false, w, cx));
                    })),
            )
            .child(self.raw_editor(window, cx))
    }

    fn left(&self, d: &Derived, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let main = div()
            .id("dict-main")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(14.));
        if self.raw {
            return main.child(self.raw_card(window, cx));
        }
        main.child(self.words_card(d, window, cx))
            .child(self.rules_card(cx))
            .child(self.learned_card(d, cx))
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
                .when(i > 0, |d| d.border_t_1().border_color(hexa(LINE)))
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(6.))
                        // from → to wraps as a group (as in the canvas); the count keeps the top-right corner.
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_wrap()
                                .items_baseline()
                                .gap_x(px(6.))
                                .child(
                                    div()
                                        .line_through()
                                        .text_color(hex(TEXT3))
                                        .child(f.from.clone()),
                                )
                                .child(div().text_color(hex(GREEN)).child("→"))
                                .child(div().font_weight(bold()).child(f.to.clone())),
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
            .border_color(hexa(LINE))
            .child(hint(
                "别名会将读音相近的错法改为目标词；最近 10 条建议也会作为 {{corrections}} 交给润色。",
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
                            rule: false,
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
                    move |i, w, cx| {
                        let _ = pick.update(cx, |this, cx| this.batch(i, w, cx));
                    },
                ))
            });
        Some(
            group(format!("待处理建议（{}）", d.fixes.len()))
                .child(group_body().gap(px(0.)).children(rows).child(footer)),
        )
    }

    /// Rules learned from repeated manual edits (hotwords.auto.txt): keep one as a user line, or undo it.
    fn learned_card(&self, d: &Derived, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        let rows = d.autos.iter().enumerate().map(|(i, (from, to, n))| {
            let (keep, undo) = (this.clone(), this.clone());
            let key = rule(from, to);
            let line = key.clone();
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .py(px(8.))
                .border_t_1()
                .border_color(hexa(LINE))
                .child(
                    div()
                        .w(px(130.))
                        .flex_none()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .line_through()
                        .text_color(hex(TEXT2))
                        .child(from.clone()),
                )
                .child(div().flex_none().text_color(hex(TEXT3)).child("→"))
                .child(
                    div()
                        .w(px(130.))
                        .flex_none()
                        .truncate()
                        .text_color(hex(CYAN))
                        .child(to.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(hex(TEXT2))
                        .child(format!("改过 {n} 次")),
                )
                .child(small_button(
                    SharedString::from(format!("auto-keep-{i}")),
                    "保留",
                    move |_, _, cx| {
                        let _ = keep.update(cx, |this, cx| this.keep_auto(&line, cx));
                    },
                ))
                .child(
                    hit(SharedString::from(format!("auto-dismiss-{i}")))
                        .h(px(24.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded_full()
                        .text_size(px(12.))
                        .text_color(hex(TEXT2))
                        .hover(|s| s.bg(hex(HOVER)).text_color(hex(TEXT)))
                        .child("撤销")
                        .on_click(move |_, _, cx| {
                            let _ = undo.update(cx, |this, cx| this.dismiss_auto(key.clone(), cx));
                        }),
                )
                .into_any_element()
        });
        let on = on_unless_zero("learnEdits");
        group_body()
            .gap(px(4.))
            .child(
                card_head("最近学会", "粘贴后手动改过的词，出现两次就自动生效")
                    .pb(px(6.))
                    .child(
                        div()
                            .flex_none()
                            .font(mono())
                            .text_size(px(11.))
                            .text_color(hex(if on { TEXT2 } else { AMBER }))
                            .child(if on {
                                "自动学习 开"
                            } else {
                                "自动学习 关"
                            }),
                    ),
            )
            .children(rows)
            .when(d.autos.is_empty(), |el| {
                el.child(hint("还没有学会的词。保留后写进你的词典；撤销后不再出现。"))
            })
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
                .child(hint("没有可用的 LLM：在「文字」里填好接口和 key").text_color(hex(AMBER)))
                .child(small_button(
                    "open-refine",
                    "打开文字",
                    move |_, _, cx| {
                        let _ = nav.update(cx, |s, cx| s.show(Page::Refine, cx));
                    },
                ))
                .into_any_element()
        } else {
            hint("独立于润色开关：润色关闭时，仍将每句文本发送到配置的 LLM 校对，只用来发现规则，不改粘贴结果")
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
                fields.push(hint("用于语音识别和润色，也参与读音相近的纠错。").into_any_element());
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
                            .text_color(hex(AMBER))
                            .into_any_element(),
                    );
                }
            }
            Kind::Advanced => {
                fields.push(
                    input("entry-line", &sheet.raw)
                        .font(mono())
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
            (Some(e), _) => Some(hint(e).text_color(hex(AMBER))),
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
            .child(div().h(px(1.)).bg(hexa(LINE)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(10.))
                    .when_some(sheet.index, |d, i| {
                        let this = this.clone();
                        d.child(
                            button("entry-delete", "删除", move |_, _, cx| {
                                let _ = this.update(cx, |this, cx| {
                                    this.sheet = None;
                                    this.remove(i, cx);
                                });
                            })
                            .text_color(hex(RED))
                            .mr_auto(),
                        )
                    })
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
        .bg(hex(RAISED))
        .border_1()
        .border_t_0()
        .border_color(hexa(LINE2))
        .rounded_b(px(6.))
        .shadow(vec![shadow(0., 8., 24., 0., 0x00000066, false)])
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
                        .bg(tint(AMBER, 0x1f))
                        .border_1()
                        .border_color(tint(AMBER, 0x66))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(hex(AMBER))
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
                    .child(self.left(&d, window, cx))
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
                            .child(self.learning(cx)),
                    ),
            )
            .when_some(overlay, |el, sheet| {
                let esc = this.clone();
                el.child(
                    div()
                        .id("dict-overlay")
                        .track_focus(&self.overlay_focus)
                        .absolute()
                        .inset_0()
                        .occlude()
                        .bg(hexa(0x00000066))
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

/// Presses one named key (`escape`, `enter`) in the focused element.
#[cfg(feature = "snapshot")]
fn press(ui: &mut super::snapshot::Ui, key: &'static str) -> Result<(), String> {
    poke(ui, move |_, w, cx| {
        w.dispatch_keystroke(
            Keystroke {
                modifiers: Default::default(),
                key: key.into(),
                key_char: None,
            },
            cx,
        );
    })
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
    // Shared fixture state this check mutates; restored at the end so later checks see the fixture.
    let auto = config::data_dir().join(AUTO_RULES);
    let saved = (hotwords_file(), read(&auto), config::raw("dismissedFixes"));

    ui.open(Page::Dictionary, facts())?;
    ui.shot("dictionary")?;
    expect(
        "pending suggestions listed",
        ui.has("fix-word-0") && ui.has("fix-word-1") && !ui.has("fix-word-2"),
        &None,
    )?;
    expect("learned rule listed", ui.has("auto-dismiss-0"), &None)?;
    expect("no-LLM hint with 打开文字", ui.has("open-refine"), &None)?;

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
    // Esc dismisses the confirm sheet too.
    ui.click("fix-batch")?;
    ui.click("fix-batch-menu-2")?;
    press(ui, "escape")?;
    expect(
        "Esc closes the confirm sheet",
        config::raw("dismissedFixes") == before && !ui.has("confirm-ok"),
        &None,
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

    // Learned rule 杰森 → JSON: 保留 writes it into hotwords.txt and drops it from hotwords.auto.txt.
    expect(
        "auto file has the learned rule",
        read(&auto).contains("杰森 -> JSON"),
        &None,
    )?;
    ui.click("auto-keep-0")?;
    expect(
        "auto rule kept as a user line",
        has_line("杰森 -> JSON")
            && dismissed_has("杰森 -> JSON")
            && !read(&auto).contains("杰森 -> JSON"),
        &Some(read(&auto)),
    )?;
    expect("learned row gone", !ui.has("auto-keep-0"), &None)?;

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

    // The add field filters the wall and the rules.
    let lines: Vec<String> = hotwords_file().split('\n').map(str::to_owned).collect();
    let line_of = |l: &str| lines.iter().position(|x| x == l);
    let (claude, plaid) = (line_of("克劳德 -> Claude"), line_of("Plaid"));
    ui.click("dict-search")?;
    type_text(ui, "claude")?;
    expect(
        "typing filters rules and words",
        matches!((claude, plaid), (Some(c), Some(p)) if ui.has(&format!("entry-{c}")) && !ui.has(&format!("word-{p}"))),
        &None,
    )?;
    ui.shot("dictionary-filter")?;
    poke(ui, |me, w, cx| {
        me.update(cx, |this, cx| {
            this.search.update(cx, |s, cx| s.set_value("", w, cx))
        })
    })?;

    // Delete a word from its sheet: the delete waits for the debounce, and closing the window saves it.
    let gpui = line_of("GPUI").ok_or("GPUI line")?;
    ui.click(&format!("word-{gpui}"))?;
    expect("word opens its sheet", ui.has("entry-delete"), &None)?;
    ui.click("entry-delete")?;
    expect("delete closes the sheet", !ui.has("entry-sheet"), &None)?;
    let draft = Rc::new(std::cell::RefCell::new(String::new()));
    let dr = draft.clone();
    poke(ui, move |me, _, cx| {
        *dr.borrow_mut() = me.read(cx).text.clone()
    })?;
    expect(
        "deleted word gone from the draft",
        !draft.borrow().split('\n').any(|l| l == "GPUI"),
        &Some(draft.borrow().clone()),
    )?;
    expect("delete waits for the debounce", has_line("GPUI"), &None)?;
    ui.close()?;
    expect(
        "close saves the draft",
        !has_line("GPUI"),
        &Some(hotwords_file()),
    )?;

    // Enter in the add field adds the word and clears the field.
    ui.open(Page::Dictionary, facts())?;
    ui.click("dict-search")?;
    type_text(ui, "张小明")?;
    ui.shot("dictionary-add")?;
    press(ui, "enter")?;
    expect(
        "Enter added a hotword",
        has_line("张小明"),
        &Some(hotwords_file()),
    )?;
    let cleared = Rc::new(std::cell::Cell::new(false));
    let c = cleared.clone();
    poke(ui, move |me, _, cx| {
        c.set(me.read(cx).search.read(cx).value().is_empty())
    })?;
    expect("add field cleared", cleared.get(), &None)?;

    // 新建 opens the sheet on a replacement; Enter in a field saves like the 保存 button.
    ui.click("dict-add")?;
    expect(
        "add sheet opens on a replacement",
        ui.has("entry-from") && !ui.has("entry-delete"),
        &None,
    )?;
    ui.click("entry-kind-0")?;
    type_text(ui, "Neo")?;
    press(ui, "enter")?;
    expect(
        "Enter saves the sheet",
        has_line("Neo") && !ui.has("entry-sheet"),
        &Some(hotwords_file()),
    )?;

    // Duplicate entry is refused.
    ui.click("dict-add")?;
    ui.click("entry-kind-0")?;
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
    press(ui, "escape")?;
    expect("Esc closes the entry sheet", !ui.has("entry-sheet"), &None)?;
    ui.click(&format!("entry-edit-{tauri}"))?;
    ui.click("entry-cancel")?;
    expect("取消 closes the sheet", !ui.has("entry-sheet"), &None)?;

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

    // 打开优化 switches pages. Settings caches page views, so the page is not released: the pending
    // delete is saved by the 500 ms debounce while another page is shown.
    ui.open(Page::Dictionary, facts())?;
    // The offscreen app runs on a test dispatcher with a fake clock; grab its executor while the
    // page still renders (pokes run in this page's render).
    let executor = Rc::new(std::cell::RefCell::new(None));
    let slot = executor.clone();
    poke(ui, move |_, _, cx| {
        *slot.borrow_mut() = Some(cx.background_executor().clone())
    })?;
    let zed = hotwords_file()
        .split('\n')
        .position(|l| l == "Zed")
        .ok_or("Zed line")?;
    ui.click(&format!("word-{zed}"))?;
    ui.click("entry-delete")?;
    expect("delete pending before page switch", has_line("Zed"), &None)?;
    ui.click("open-refine")?;
    expect("打开文字 leaves the page", !ui.has("dict-add"), &None)?;
    executor
        .borrow()
        .as_ref()
        .ok_or("executor")?
        .advance_clock(Duration::from_millis(600));
    ui.frame()?;
    expect(
        "debounce saves the draft off-page",
        !has_line("Zed"),
        &Some(hotwords_file()),
    )?;
    ui.close()?;

    // Empty dictionary.
    write_atomic(&path(), b"").map_err(|e| e.to_string())?;
    ui.open(Page::Dictionary, facts())?;
    expect(
        "empty state",
        ui.has("dict-search") && !ui.has("word-0"),
        &None,
    )?;
    ui.shot("dictionary-empty")?;
    ui.close()?;

    let (body, auto_body, fixes) = saved;
    write_atomic(&path(), body.as_bytes()).map_err(|e| e.to_string())?;
    write_atomic(&auto, auto_body.as_bytes()).map_err(|e| e.to_string())?;
    config::set(
        "dismissedFixes",
        fixes.map_or(serde_json::Value::Null, serde_json::Value::String),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
