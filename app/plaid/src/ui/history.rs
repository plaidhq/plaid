//! 历史: dictation records newest first, grouped by local day, with status filter, search, a detail box, retry of
//! the saved recording (through the controller), copy, playback, export for feedback, and clearing.
use super::mav::*;
use super::{Model, Page, on_if_one, reveal, send};
use crate::config;
use chrono::{DateTime, Datelike, Local, Utc};
use gpui_kit::base::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::dictation::Msg;
use plaid_core::store::{self, LIMIT, Record};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

const LATENCY: &str = "historyLatency";

/// List row height; day headers use it too so the list stays uniform.
const ROW_H: f32 = 56.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Saved,
    Failed,
    Llm,
    Edited,
}

impl Filter {
    /// Chip order; the first three keep their indices from the old segmented control.
    const ALL: [Filter; 5] = [
        Filter::All,
        Filter::Saved,
        Filter::Failed,
        Filter::Llm,
        Filter::Edited,
    ];

    fn keeps(self, r: &Record) -> bool {
        match self {
            Filter::All => true,
            Filter::Saved => r.outcome.as_deref() == Some("saved"),
            Filter::Failed => r.is_failure(),
            Filter::Llm => r.llm == "ok",
            Filter::Edited => r.edited.is_some(),
        }
    }
}

/// Confirmation sheet over the page.
enum Sheet {
    Clear(usize),
    Retry(DateTime<Utc>),
}

/// One line of the table: a day header or a record.
enum Line {
    Day(String),
    Rec(Box<Record>),
}

/// A retry posted to the controller. The controller reports only through the HUD, so the page infers the end from
/// the record changing on disk, or gives up after the controller's own timeout.
struct Retry {
    id: DateTime<Utc>,
    before: (String, Option<String>),
    _timeout: Task<()>,
}

/// Navigates the settings window to another page.
type Go = Rc<dyn Fn(Page, &mut Window, &mut App)>;

pub struct History {
    model: Entity<Model>,
    go: Go,
    search: Entity<InputState>,
    filter: Filter,
    selected: Vec<DateTime<Utc>>,
    anchor: Option<DateTime<Utc>>,
    actions: bool,
    context: Option<Point<Pixels>>,
    sheet: Option<Sheet>,
    details: bool,
    retry: Option<Retry>,
    note: Option<&'static str>,
    exporting: bool,
    player: Option<std::process::Child>,
    audio_bytes: u64,
    scroll: UniformListScrollHandle,
    wave: RefCell<Option<(PathBuf, Rc<Vec<f32>>)>>,
}

impl History {
    pub fn new(
        model: Entity<Model>,
        go: impl Fn(Page, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> History {
        cx.observe(&model, |this, _, cx| this.changed(cx)).detach();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索文本或应用"));
        cx.subscribe_in(&search, window, |this, _, e: &InputEvent, _, cx| {
            if let InputEvent::Change = e {
                this.selected.clear();
                this.anchor = None;
            }
            cx.notify();
        })
        .detach();
        let audio_bytes = dir_size(&model.read(cx).store.audio_dir());
        History {
            model,
            go: Rc::new(go),
            search,
            filter: Filter::All,
            selected: vec![],
            anchor: None,
            actions: false,
            context: None,
            sheet: None,
            details: false,
            retry: None,
            note: None,
            exporting: false,
            player: None,
            audio_bytes,
            scroll: UniformListScrollHandle::new(),
            wave: RefCell::new(None),
        }
    }

    /// The history changed on disk: drop vanished selections, finish a retry whose record changed.
    fn changed(&mut self, cx: &mut Context<Self>) {
        let store = &self.model.read(cx).store;
        self.selected
            .retain(|id| store.all.iter().any(|r| r.at == *id));
        if let Some(r) = &self.retry {
            match store.all.iter().rev().find(|x| x.at == r.id) {
                Some(x) if (x.text.clone(), x.outcome.clone()) != r.before => {
                    self.retry = None;
                    self.note = Some("识别结果已保存在原记录中，请手动复制；未自动粘贴。");
                }
                None => self.retry = None,
                _ => {}
            }
        }
        self.audio_bytes = dir_size(&store.audio_dir());
        cx.notify();
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
    }

    fn record<'a>(&self, id: DateTime<Utc>, cx: &'a App) -> Option<&'a Record> {
        self.model
            .read(cx)
            .store
            .all
            .iter()
            .rev()
            .find(|r| r.at == id)
    }

    fn audio_path(&self, r: &Record, cx: &App) -> Option<PathBuf> {
        let name = Path::new(r.audio.as_deref()?).file_name()?;
        let p = self.model.read(cx).store.audio_dir().join(name);
        p.is_file().then_some(p)
    }

    fn set_filter(&mut self, f: Filter, cx: &mut Context<Self>) {
        if f != self.filter {
            self.filter = f;
            self.selected.clear();
            self.anchor = None;
        }
        cx.notify();
    }

    /// Click on a row: plain selects one, ⌘/Ctrl toggles, ⇧ extends from the anchor over the visible rows.
    fn click_row(
        &mut self,
        id: DateTime<Utc>,
        m: Modifiers,
        order: &[DateTime<Utc>],
        cx: &mut Context<Self>,
    ) {
        self.context = None;
        if m.shift
            && let Some(a) = self.anchor
            && let (Some(i), Some(j)) = (
                order.iter().position(|x| *x == a),
                order.iter().position(|x| *x == id),
            )
        {
            let (lo, hi) = (i.min(j), i.max(j));
            self.selected = order[lo..=hi].to_vec();
        } else if m.secondary(cx) {
            if let Some(k) = self.selected.iter().position(|x| *x == id) {
                self.selected.remove(k);
            } else {
                self.selected.push(id);
            }
            self.anchor = Some(id);
        } else {
            self.selected = vec![id];
            self.anchor = Some(id);
        }
        cx.notify();
    }

    fn copy(&self, ids: &[DateTime<Utc>], cx: &mut App) {
        let text: Vec<String> = self
            .model
            .read(cx)
            .store
            .all
            .iter()
            .filter(|r| ids.contains(&r.at) && !r.text.is_empty())
            .map(|r| r.text.clone())
            .collect();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.join("\n")));
        }
    }

    fn play(&mut self, path: &Path) {
        self.stop();
        #[cfg(target_os = "macos")]
        let r = std::process::Command::new("afplay").arg(path).spawn();
        #[cfg(windows)]
        let r = {
            use std::os::windows::process::CommandExt;
            let p = path.display().to_string().replace('\'', "''");
            std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!("(New-Object Media.SoundPlayer '{p}').PlaySync()"),
                ])
                .creation_flags(0x0800_0000)
                .spawn()
        };
        #[cfg(not(any(target_os = "macos", windows)))]
        let r: std::io::Result<std::process::Child> = Err(std::io::Error::other(format!(
            "no player for {}",
            path.display()
        )));
        match r {
            Ok(c) => self.player = Some(c),
            Err(e) => crate::trace::log(&format!("play: {e}")),
        }
    }

    fn stop(&mut self) {
        if let Some(mut c) = self.player.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn retry(&mut self, id: DateTime<Utc>, cx: &mut Context<Self>) {
        let Some(r) = self.record(id, cx) else {
            return;
        };
        let before = (r.text.clone(), r.outcome.clone());
        // The controller gives up after max(30, audio + 10) s; allow a little more for the save.
        let secs = 30.max(r.audio_ms / 1000 + 10) as u64 + 5;
        send(Msg::Recover { id }, cx);
        let timeout = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(secs))
                .await;
            let _ = this.update(cx, |h, cx| {
                if h.retry.as_ref().is_some_and(|r| r.id == id) {
                    h.retry = None;
                    cx.notify();
                }
            });
        });
        self.note = None;
        self.retry = Some(Retry {
            id,
            before,
            _timeout: timeout,
        });
        cx.notify();
    }

    /// Through the controller when the app runs (it refuses while busy); snapshot runs clear the fixture directly.
    fn clear(&mut self, cx: &mut Context<Self>) {
        if cx.try_global::<super::Live>().is_some() {
            send(Msg::Clear, cx);
        } else {
            self.model.update(cx, |m, cx| {
                m.store.clear();
                cx.notify();
            });
        }
        self.selected.clear();
        self.anchor = None;
        self.retry = None;
        self.note = None;
        cx.notify();
    }

    fn export(&mut self, ids: Vec<DateTime<Utc>>, window: &mut Window, cx: &mut Context<Self>) {
        if self.exporting {
            return;
        }
        let m = self.model.read(cx);
        let records: Vec<Record> = m
            .store
            .all
            .iter()
            .filter(|r| ids.contains(&r.at))
            .cloned()
            .collect();
        if records.is_empty() {
            return;
        }
        let info = export_info(&m.facts);
        let data = m.store.dir().to_owned();
        let name = format!("plaid-feedback-{}.zip", Local::now().format("%Y%m%d-%H%M"));
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let start = [home.join("Desktop"), home]
            .into_iter()
            .find(|d| d.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let picked = cx.prompt_for_new_path(&start, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let out = match picked.await {
                Ok(Ok(Some(p))) => p,
                Ok(Err(e)) => {
                    crate::trace::log(&format!("export: {e}"));
                    return;
                }
                _ => return,
            };
            let _ = this.update(cx, |h, cx| {
                h.exporting = true;
                cx.notify();
            });
            let result = cx
                .background_executor()
                .spawn(async move { write_bundle(&data, &records, &info, &out).map(|_| out) })
                .await;
            let _ = this.update_in(cx, |h, window, cx| {
                h.exporting = false;
                match result {
                    Ok(out) => reveal(&out),
                    Err(e) => {
                        crate::trace::log(&format!("export: {e}"));
                        drop(window.prompt(
                            PromptLevel::Critical,
                            "导出失败",
                            Some(&e),
                            &["好"],
                            cx,
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // --- Rendering ---------------------------------------------------------------------------------------------

    fn search_field(&self, shown: usize, cx: &mut Context<Self>) -> Div {
        let typed = !self.search.read(cx).value().is_empty();
        let search = self.search.clone();
        div()
            .h(px(36.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .rounded(px(10.))
            .bg(hex(SURFACE))
            .border_1()
            .border_color(hexa(LINE))
            .child(icon("plaid/search.svg", 14., hex(TEXT2)))
            .child(div().flex_1().min_w_0().child(Input::new(&self.search)))
            .when(typed, |d| {
                d.child(
                    div()
                        .flex_none()
                        .font(mono())
                        .text_size(px(11.))
                        .text_color(hex(TEXT2))
                        .child(format!("{shown} 条")),
                )
                .child(
                    hit("history-search-clear")
                        .flex_none()
                        .child(icon("plaid/xcircle.svg", 13., hex(TEXT3)))
                        .on_click(move |_, w, cx| {
                            search.update(cx, |s, cx| s.set_value("", w, cx));
                        }),
                )
            })
    }

    /// Filter chips with counts, then the actions button.
    fn chips(&self, counts: [usize; 5], cx: &mut Context<Self>) -> Div {
        let labels = ["全部", "未粘贴", "失败", "润色", "改过"];
        let mut row = div().flex().flex_none().items_center().gap(px(6.));
        for (i, f) in Filter::ALL.into_iter().enumerate() {
            let on = self.filter == f;
            let this = cx.entity().downgrade();
            row = row.child(
                hit(format!("history-filter-{i}"))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .h(px(24.))
                    .px(px(10.))
                    .rounded(px(12.))
                    .text_size(px(12.))
                    .whitespace_nowrap()
                    .border_1()
                    .map(|d| {
                        if on {
                            d.bg(tint(TEXT, 0x1a))
                                .border_color(gpui_kit::transparent_black())
                                .text_color(hex(TEXT))
                        } else {
                            d.border_color(hexa(LINE))
                                .text_color(hex(TEXT2))
                                .hover(|s| s.bg(tint(TEXT, 0x0d)))
                        }
                    })
                    .child(labels[i])
                    .child(
                        div()
                            .font(mono())
                            .text_size(px(10.))
                            .text_color(hex(if on { TEXT2 } else { TEXT3 }))
                            .child(counts[i].to_string()),
                    )
                    .on_click(move |_, _, cx| {
                        let _ = this.update(cx, |h, cx| h.set_filter(f, cx));
                    }),
            );
        }
        row.child(div().flex_1()).child(self.action_button(cx))
    }

    /// NSPopUpButton in pull-down style with the action (ellipsis) glyph, and its drawn menu.
    fn action_button(&self, cx: &mut Context<Self>) -> Div {
        let empty = self.model.read(cx).store.all.is_empty();
        let exporting = self.exporting;
        let items: Vec<MenuEntry> = [
            ("显示延迟", on_if_one(LATENCY)),
            ("性能统计…", false),
            ("导出最近 20 条…", false),
            ("显示数据文件夹", false),
            ("清空历史…", false),
        ]
        .into_iter()
        .map(|(l, c)| MenuEntry {
            label: l.into(),
            checked: c,
            rule: false,
        })
        .collect();
        let this = cx.entity().downgrade();
        let (toggle, close) = (this.clone(), this.clone());
        let go = self.go.clone();
        let face = hit("history-actions")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(24.))
            .rounded(px(12.))
            .border_1()
            .border_color(hexa(LINE))
            .hover(|s| s.bg(tint(TEXT, 0x0d)))
            .child(icon("plaid/action.svg", 14., hex(TEXT2)))
            .on_click(move |_, _, cx| {
                let _ = toggle.update(cx, |h, cx| {
                    h.actions = !h.actions;
                    cx.notify();
                });
            });
        div().flex().flex_col().child(face).when(self.actions, |d| {
            d.child(deferred(
                anchored().snap_to_window_with_margin(px(8.)).child(
                    menu("history-actions-menu", 180., items, move |i, w, cx| {
                        let go = go.clone();
                        let _ = this.update(cx, |h, cx| {
                            h.actions = false;
                            match i {
                                0 => h.model.update(cx, |m, cx| {
                                    m.set(LATENCY, super::bool_str(!on_if_one(LATENCY)), cx)
                                }),
                                1 => go(Page::Perf, w, cx),
                                2 if !empty && !exporting => {
                                    let all = &h.model.read(cx).store.all;
                                    let ids = all[all.len().saturating_sub(20)..]
                                        .iter()
                                        .map(|r| r.at)
                                        .collect();
                                    h.export(ids, w, cx);
                                }
                                3 => reveal(&config::data_dir().join("records.jsonl")),
                                4 if !empty => {
                                    h.sheet = Some(Sheet::Clear(h.model.read(cx).store.all.len()))
                                }
                                _ => {}
                            }
                            cx.notify();
                        });
                    })
                    .mt(px(2.))
                    .on_mouse_down_out(move |_, _, cx| {
                        let _ = close.update(cx, |h, cx| {
                            h.actions = false;
                            cx.notify();
                        });
                    }),
                ),
            ))
        })
    }

    /// Retry progress or result, and the export spinner, under the chips.
    fn status_line(&self) -> Option<Stateful<Div>> {
        let (text, busy) = if self.retry.is_some() {
            ("正在重新识别；不会自动粘贴…", true)
        } else if self.exporting {
            ("正在导出…", true)
        } else {
            (self.note?, false)
        };
        Some(
            hit("history-status")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .px(px(2.))
                .when(busy, |d| d.child(spinner(11., TEXT2, 0)))
                .child(hint(text).text_color(hex(TEXT))),
        )
    }

    fn list(
        &self,
        lines: Rc<Vec<Line>>,
        marked: Vec<DateTime<Utc>>,
        empty: Option<(&'static str, &'static str)>,
        cx: &mut Context<Self>,
    ) -> Div {
        let frame = div().flex_1().min_h_0().flex().flex_col();
        if let Some((title, body)) = empty {
            return frame.child(
                hit("history-empty")
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .font_weight(medium())
                            .text_color(hex(TEXT2))
                            .child(title),
                    )
                    .child(hint(body)),
            );
        }
        let this = cx.entity().downgrade();
        let marked = Rc::new(marked);
        let latency = on_if_one(LATENCY);
        let retrying = self.retry.as_ref().map(|r| r.id);
        let order: Rc<Vec<DateTime<Utc>>> = Rc::new(
            lines
                .iter()
                .filter_map(|l| match l {
                    Line::Rec(r) => Some(r.at),
                    Line::Day(_) => None,
                })
                .collect(),
        );
        let n = lines.len();
        frame.child(
            uniform_list("history-list", n, move |range, _, _| {
                range
                    .map(|i| match &lines[i] {
                        Line::Day(label) => day_row(label.clone()).into_any_element(),
                        Line::Rec(r) => {
                            let id = r.at;
                            let (t1, t2, ord) = (this.clone(), this.clone(), order.clone());
                            let row =
                                record_row(r, marked.contains(&id), latency, retrying == Some(id))
                                    .on_click(move |e, _, cx| {
                                        if e.is_right_click() {
                                            return;
                                        }
                                        let m = e.modifiers();
                                        let _ = t1.update(cx, |h, cx| h.click_row(id, m, &ord, cx));
                                    })
                                    .on_mouse_down(MouseButton::Right, move |e, _, cx| {
                                        let p = e.position;
                                        let _ = t2.update(cx, |h, cx| {
                                            if !h.selected.contains(&id) {
                                                h.selected = vec![id];
                                                h.anchor = Some(id);
                                            }
                                            h.context = Some(p);
                                            cx.notify();
                                        });
                                    });
                            div().h(px(ROW_H)).py(px(2.)).child(row).into_any_element()
                        }
                    })
                    .collect()
            })
            .track_scroll(&self.scroll)
            .flex_1(),
        )
    }

    /// Right-click menu over the selection.
    fn context_menu(&self, at: Point<Pixels>, cx: &mut Context<Self>) -> impl IntoElement {
        let n = self.selected.len();
        let items = vec![
            MenuEntry {
                label: if n > 1 {
                    format!("复制 {n} 条").into()
                } else {
                    "复制".into()
                },
                checked: false,
                rule: false,
            },
            MenuEntry {
                label: format!("导出选中的 {n} 条…").into(),
                checked: false,
                rule: false,
            },
        ];
        let this = cx.entity().downgrade();
        let close = this.clone();
        deferred(
            anchored()
                .position(at)
                .snap_to_window_with_margin(px(8.))
                .child(
                    menu("history-context", 160., items, move |i, w, cx| {
                        let _ = this.update(cx, |h, cx| {
                            h.context = None;
                            let ids = h.selected.clone();
                            if i == 0 {
                                h.copy(&ids, cx);
                            } else {
                                h.export(ids, w, cx);
                            }
                            cx.notify();
                        });
                    })
                    .on_mouse_down_out(move |_, _, cx| {
                        let _ = close.update(cx, |h, cx| {
                            h.context = None;
                            cx.notify();
                        });
                    }),
                ),
        )
    }

    /// Peaks of the recording for the waveform strip, read once per file.
    fn peaks(&self, path: &Path) -> Rc<Vec<f32>> {
        if let Some((p, v)) = &*self.wave.borrow()
            && p == path
        {
            return v.clone();
        }
        let v = Rc::new(peaks(path));
        *self.wave.borrow_mut() = Some((path.to_path_buf(), v.clone()));
        v
    }

    /// The right panel: one record (the selection, or the newest shown), or the multi-selection actions.
    fn detail(&self, focus: Option<DateTime<Utc>>, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        let panel = || {
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .rounded(px(16.))
                .bg(hex(SURFACE))
                .border_1()
                .border_color(hexa(LINE))
        };
        if self.selected.len() > 1 {
            let n = self.selected.len();
            let ids = self.selected.clone();
            let (t1, t2) = (this.clone(), this);
            let ids2 = ids.clone();
            return panel().child(
                hit("history-detail")
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .px(px(22.))
                    .py(px(18.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(medium())
                            .child(format!("已选 {n} 条")),
                    )
                    .child(hint("⌘ 点击加减，⇧ 点击连选。"))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(primary(
                                "history-copy-many",
                                format!("复制 {n} 条"),
                                move |_, _, cx| {
                                    let _ = t1.update(cx, |h, cx| h.copy(&ids, cx));
                                },
                            ))
                            .child(enabled(
                                !self.exporting,
                                button(
                                    "history-export-many",
                                    format!("导出选中的 {n} 条…"),
                                    move |_, w, cx| {
                                        let ids = ids2.clone();
                                        let _ = t2.update(cx, |h, cx| h.export(ids, w, cx));
                                    },
                                ),
                            )),
                    ),
            );
        }
        let Some(r) = focus.and_then(|id| self.record(id, cx)).cloned() else {
            return panel();
        };
        let id = r.at;
        let wav = self.audio_path(&r, cx);
        let retrying = self.retry.as_ref().is_some_and(|x| x.id == id);
        let (tag, color) = path_tag(&r, retrying);
        let llm = r.llm == "ok";
        let mark = if llm { VIOLET } else { CYAN };
        let app = r.app.clone().unwrap_or_else(|| "未知应用".into());

        let header = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(
                div()
                    .size(px(26.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(7.))
                    .bg(hex(RAISED2))
                    .font(mono())
                    .text_size(px(10.))
                    .child(badge(&app)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(15.))
                    .font_weight(medium())
                    .child(app),
            )
            .child(
                div()
                    .flex_none()
                    .font(mono())
                    .text_size(px(11.))
                    .text_color(hex(TEXT2))
                    .child(when(r.at)),
            )
            .child(
                div()
                    .ml_auto()
                    .flex_none()
                    .px(px(9.))
                    .py(px(2.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(tint(color, 0x66))
                    .font(mono())
                    .text_size(px(11.))
                    .text_color(hex(color))
                    .child(tag),
            );

        let secs = format!("{}:{:02}", r.audio_ms / 60000, (r.audio_ms / 1000) % 60);
        let mut strip = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(px(50.))
            .px(px(12.))
            .rounded(px(12.))
            .bg(hex(WELL))
            .border_1()
            .border_color(hexa(LINE));
        if let Some(path) = wav.clone() {
            let peaks = self.peaks(&path);
            let t_play = this.clone();
            strip = strip
                .child(
                    hit("history-play")
                        .size(px(28.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(hex(mark))
                        .shadow(vec![glow(mark, 12., 0x55)])
                        .child(icon("plaid/tri-right.svg", 10., hex(BG)).ml(px(2.)))
                        .on_click(move |_, _, cx| {
                            let p = path.clone();
                            let _ = t_play.update(cx, |h, _| h.play(&p));
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .h(px(30.))
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .children(peaks.iter().map(|a| {
                            div()
                                .flex_1()
                                .h(px(2. + a * 26.))
                                .rounded(px(1.))
                                .bg(tint(mark, 0xa0))
                        })),
                );
        } else {
            strip = strip.child(
                div()
                    .flex_1()
                    .child(hint("没有保留录音，无法播放或重新识别")),
            );
        }
        strip = strip.child(
            div()
                .flex_none()
                .font(mono())
                .text_size(px(11.))
                .text_color(hex(TEXT2))
                .child(secs),
        );

        // Recognized text with what the rules and the LLM removed struck through; the result with what they
        // added in the path color.
        let original = r.asr.clone().unwrap_or_else(|| r.raw.clone());
        let (deleted, inserted) = super::perf::diff(&original, &r.text);
        let changed: usize = deleted
            .iter()
            .map(|g| {
                original[g.clone()]
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .count()
            })
            .chain(inserted.iter().map(|g| {
                r.text[g.clone()]
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .count()
            }))
            .sum();
        let struck = HighlightStyle {
            color: Some(hex(TEXT3)),
            strikethrough: Some(StrikethroughStyle {
                thickness: px(1.5),
                color: Some(hex(RED)),
            }),
            ..Default::default()
        };
        let added = |c: u32| HighlightStyle {
            color: Some(hex(c)),
            underline: Some(UnderlineStyle {
                thickness: px(2.),
                color: Some(hex(c)),
                wavy: false,
            }),
            ..Default::default()
        };
        let mut texts = div().flex().flex_col().gap(px(14.));
        if !r.text.is_empty() && !original.is_empty() && original != r.text {
            texts = texts.child(block(
                "识别原文",
                div()
                    .text_size(px(14.))
                    .line_height(px(24.))
                    .text_color(hex(TEXT2))
                    .child(
                        StyledText::new(original.clone())
                            .with_highlights(deleted.into_iter().map(|g| (g, struck))),
                    ),
            ));
        }
        let out_label = if r.outcome.as_deref() == Some("saved") {
            "已保存，未粘贴"
        } else {
            "粘贴出去的"
        };
        texts = texts.child(if r.text.is_empty() {
            block(
                if r.is_failure() {
                    "识别失败"
                } else {
                    "结果"
                },
                div()
                    .text_size(px(15.))
                    .text_color(hex(TEXT3))
                    .child(match &r.error {
                        Some(e) => format!("没有识别结果：{e}"),
                        None => "没有识别结果".into(),
                    }),
            )
        } else {
            block(
                out_label,
                div().text_size(px(20.)).line_height(px(30.)).child(
                    StyledText::new(r.text.clone())
                        .with_highlights(inserted.into_iter().map(|g| (g, added(mark)))),
                ),
            )
        });
        if let Some(e) = &r.edited {
            let (_, ins) = super::perf::diff(&r.text, e);
            texts = texts.child(block(
                "你后来改成",
                div().text_size(px(15.)).line_height(px(24.)).child(
                    StyledText::new(e.clone())
                        .with_highlights(ins.into_iter().map(|g| (g, added(AMBER)))),
                ),
            ));
        }

        let llm_bad = !matches!(r.llm.as_str(), "ok" | "off");
        let chars = r.text.chars().filter(|c| !c.is_whitespace()).count() as i64;
        let facts = [
            (
                "松开 → 粘贴",
                if r.outcome.is_none() {
                    format!("{:.2}s", r.total_ms as f64 / 1000.)
                } else {
                    "—".into()
                },
                false,
            ),
            (
                "LLM",
                match r.llm.as_str() {
                    "ok" => r
                        .llm_ms
                        .map_or("—".into(), |v| format!("{:.2}s", v as f64 / 1000.)),
                    "off" => "关".into(),
                    "skipped" => "跳过".into(),
                    "timeout" => "超时".into(),
                    _ => "出错".into(),
                },
                llm_bad && r.llm != "skipped",
            ),
            ("改动", format!("{changed} 字"), false),
            (
                "说话速度",
                if r.audio_ms > 0 && chars > 0 {
                    format!("{}/分", chars * 60_000 / r.audio_ms)
                } else {
                    "—".into()
                },
                false,
            ),
        ];
        let facts = div()
            .flex()
            .gap(px(10.))
            .children(facts.into_iter().map(|(k, v, warn)| {
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(hexa(LINE))
                    .child(div().text_size(px(11.)).text_color(hex(TEXT2)).child(k))
                    .child(
                        div()
                            .truncate()
                            .font(mono())
                            .text_size(px(17.))
                            .font_weight(bold())
                            .text_color(hex(if warn { AMBER } else { TEXT }))
                            .child(v),
                    )
            }));

        let text = r.text.clone();
        let (t_copy, t_retry, t_details) = (this.clone(), this.clone(), this);
        let mut buttons = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .child(enabled(
                !text.is_empty(),
                primary("history-copy", "复制", move |_, _, cx| {
                    let _ = t_copy.update(cx, |h, cx| h.copy(&[id], cx));
                }),
            ));
        if let Some(path) = wav {
            buttons = buttons
                .child(enabled(
                    self.retry.is_none(),
                    button("history-retry", "重新识别…", move |_, _, cx| {
                        let _ = t_retry.update(cx, |h, cx| {
                            h.sheet = Some(Sheet::Retry(id));
                            cx.notify();
                        });
                    }),
                ))
                .child(button("history-reveal", reveal_label(), move |_, _, _| {
                    reveal(&path)
                }));
        }

        let mut fields: Vec<(&str, String, bool)> = vec![];
        if let Some(e) = &r.error {
            fields.push(("错误", e.clone(), true));
        }
        if llm_bad {
            fields.push(("LLM", r.llm.clone(), true));
        }
        fields.push(("首字", ms(r.first_text_ms), false));
        fields.push(("识别", ms(Some(r.asr_ms).filter(|v| *v > 0)), false));
        if let Some(v) = r.trigger_ms {
            fields.push(("按键 → 启动", format!("{v} ms"), false));
        }
        if let Some(v) = r.mic_start_ms {
            fields.push(("麦克风启动", format!("{v} ms"), false));
        }
        if r.asr.is_some() && r.raw != r.text {
            fields.push(("规则后", r.raw.clone(), false));
        }
        if r.noise_rms.is_some() || r.speech_rms.is_some() {
            let level = format!(
                "噪声约 {}，语音约 {}（RMS）{}",
                r.noise_rms.map_or("—".into(), |v| v.to_string()),
                r.speech_rms.map_or("—".into(), |v| v.to_string()),
                r.mic.as_ref().map_or(String::new(), |m| format!("，{m}"))
            );
            fields.push(("电平", level, false));
        }
        let details = self.details;
        panel().child(
            hit("history-detail")
                .size_full()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(14.))
                .px(px(22.))
                .py(px(18.))
                .child(header)
                .child(strip)
                .child(texts)
                .child(facts)
                .child(buttons)
                .child(disclosure(
                    "history-details",
                    details,
                    if r.error.is_some() {
                        "错误与详情"
                    } else {
                        "详情"
                    },
                    move |_, cx| {
                        let _ = t_details.update(cx, |h, cx| {
                            h.details = !h.details;
                            cx.notify();
                        });
                    },
                ))
                .when(details, |d| {
                    d.child(div().flex().flex_col().gap(px(4.)).pl(px(15.)).children(
                        fields.into_iter().map(|(k, v, warn)| {
                            div()
                                .flex()
                                .gap(px(10.))
                                .text_size(px(11.))
                                .child(div().w(px(80.)).flex_none().text_color(hex(TEXT2)).child(k))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_color(hex(if warn { AMBER } else { TEXT }))
                                        .child(v),
                                )
                        }),
                    ))
                }),
        )
    }

    fn bottom(&self, n: usize, cx: &mut Context<Self>) -> Div {
        let mut text = format!("最多保留 {LIMIT} 条 · 只保存在本机");
        if self.audio_bytes > 0 {
            text.push_str(&format!(" · 录音占用 {}", size_label(self.audio_bytes)));
        }
        let this = cx.entity().downgrade();
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(40.))
            .px(px(28.))
            .gap(px(10.))
            .font(mono())
            .text_size(px(11.))
            .text_color(hex(TEXT2))
            .child(text)
            .child(div().flex_1())
            .child(enabled(
                n > 0,
                small_button("history-clear", "清空历史…", move |_, _, cx| {
                    let _ = this.update(cx, |h, cx| {
                        h.sheet = Some(Sheet::Clear(n));
                        cx.notify();
                    });
                })
                .font(sans()),
            ))
    }

    /// A 10.9 sheet: hangs from the top of the pane, blocks the page behind it.
    fn sheet(&self, sheet: &Sheet, cx: &mut Context<Self>) -> Stateful<Div> {
        let (key, title, body, ok) = match sheet {
            Sheet::Clear(n) => (
                "history-clear",
                format!("删除全部 {n} 条记录和录音？"),
                "「今天」和「报告」里的用量统计会保留。",
                "删除",
            ),
            Sheet::Retry(_) => (
                "history-retry",
                "重新识别这段录音？".to_string(),
                "这段录音将上传到当前云端识别服务。结果保存到原记录，不自动粘贴，也不运行润色或命令。",
                "重新识别",
            ),
        };
        let retry = match sheet {
            Sheet::Retry(id) => Some(*id),
            Sheet::Clear(_) => None,
        };
        let this = cx.entity().downgrade();
        let cancel = this.clone();
        div()
            .id("history-sheet")
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_start()
            .justify_center()
            .child(
                div()
                    .w(px(430.))
                    .h_auto()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .px(px(20.))
                    .pt(px(16.))
                    .pb(px(14.))
                    .bg(hex(RAISED))
                    .border_1()
                    .border_t_0()
                    .border_color(hexa(LINE2))
                    .rounded_b(px(5.))
                    .shadow(vec![shadow(0., 8., 24., 0., 0x00000066, false)])
                    .child(div().font_weight(bold()).child(title))
                    .child(
                        div()
                            .text_size(px(11.))
                            .line_height(px(15.))
                            .text_color(hex(TEXT2))
                            .child(body),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .flex()
                            .justify_end()
                            .gap(px(10.))
                            .child(button(
                                format!("{key}-cancel"),
                                "取消",
                                move |_, _, cx| {
                                    let _ = cancel.update(cx, |h, cx| {
                                        h.sheet = None;
                                        cx.notify();
                                    });
                                },
                            ))
                            .child(default_button(format!("{key}-ok"), ok, move |_, _, cx| {
                                let _ = this.update(cx, |h, cx| {
                                    h.sheet = None;
                                    match retry {
                                        Some(id) => h.retry(id, cx),
                                        None => h.clear(cx),
                                    }
                                    cx.notify();
                                });
                            })),
                    ),
            )
    }
}

impl Drop for History {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Render for History {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let q = self.query(cx);
        let store = &self.model.read(cx).store;
        let total = store.all.len();
        let found: Vec<&Record> = store.all.iter().filter(|r| matches(r, &q)).collect();
        let counts = Filter::ALL.map(|f| found.iter().filter(|r| f.keeps(r)).count());
        let shown: Vec<&Record> = found
            .into_iter()
            .rev()
            .filter(|r| self.filter.keeps(r))
            .collect();
        // With nothing selected the newest shown record fills the panel.
        let focus = match self.selected.as_slice() {
            [] => shown.first().map(|r| r.at),
            [id] => Some(*id),
            _ => None,
        };
        let marked = if self.selected.is_empty() {
            focus.into_iter().collect()
        } else {
            self.selected.clone()
        };
        let lines = Rc::new(group(&shown));
        let n_shown = shown.len();
        let empty = if total == 0 {
            Some((
                "还没有听写记录",
                "按住快捷键说话，松开后文字会粘贴到光标处。",
            ))
        } else if n_shown > 0 {
            None
        } else if !q.is_empty() {
            Some(("没有匹配的记录", "试试其他关键词，或切换筛选。"))
        } else {
            Some(("没有匹配的记录", "这个筛选下没有记录。"))
        };
        let context = self.context;
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap(px(16.))
                    .pt(px(6.))
                    .px(px(28.))
                    .child(
                        div()
                            .w(px(380.))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(self.search_field(n_shown, cx))
                            .child(self.chips(counts, cx))
                            .children(self.status_line())
                            .child(self.list(lines, marked, empty, cx)),
                    )
                    .child(self.detail(focus, cx)),
            )
            .child(self.bottom(total, cx))
            .when_some(context, |d, at| d.child(self.context_menu(at, cx)))
            .when_some(self.sheet.as_ref(), |d, s| d.child(self.sheet(s, cx)))
    }
}

// --- Rows ------------------------------------------------------------------------------------------------------

/// Day headers take a full row so the list stays uniform; the label sits at the bottom.
fn day_row(label: String) -> Div {
    div()
        .w_full()
        .h(px(ROW_H))
        .flex()
        .items_end()
        .px(px(2.))
        .pb(px(6.))
        .font(mono())
        .text_size(px(10.))
        .text_color(hex(TEXT3))
        .child(label)
}

/// The 2 px bar color and the panel tag for a record's path.
fn path_tag(r: &Record, retrying: bool) -> (&'static str, u32) {
    if retrying {
        ("重新识别中…", TEXT2)
    } else if r.is_failure() {
        ("识别失败", RED)
    } else if r.outcome.as_deref() == Some("saved") {
        ("未粘贴", AMBER)
    } else if r.llm == "ok" {
        ("LLM 润色", VIOLET)
    } else {
        ("直接粘贴", CYAN)
    }
}

fn record_row(r: &Record, selected: bool, latency: bool, retrying: bool) -> Stateful<Div> {
    let local = r.at.with_timezone(&Local);
    let (tag, color) = path_tag(r, retrying);
    let delivered = r.outcome.is_none() && !retrying;
    let right = if delivered {
        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .when(r.edited.is_some(), |d| {
                d.child(icon("plaid/pencil.svg", 10., hex(AMBER)))
            })
            .child(format!(
                "{} 字",
                r.text.chars().filter(|c| !c.is_whitespace()).count()
            ))
    } else {
        div().text_color(hex(color)).child(tag)
    };
    let text = if r.text.is_empty() {
        div().text_color(hex(TEXT3)).child("没有识别结果")
    } else {
        div().child(r.text.replace('\n', " "))
    };
    hit(format!("history-row-{}", r.at.timestamp_millis()))
        .size_full()
        .flex()
        .gap(px(10.))
        .px(px(12.))
        .py(px(6.))
        .rounded(px(10.))
        .border_1()
        .map(|d| {
            if selected {
                d.bg(hex(RAISED)).border_color(hexa(LINE2))
            } else {
                d.border_color(gpui_kit::transparent_black())
                    .hover(|s| s.bg(tint(TEXT, 0x08)))
            }
        })
        .child(
            div()
                .w(px(2.))
                .h_full()
                .flex_none()
                .rounded(px(1.))
                .bg(hex(color)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(3.))
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .font(mono())
                        .text_size(px(10.))
                        .text_color(hex(TEXT2))
                        .child(div().flex_none().child(local.format("%H:%M").to_string()))
                        .child(
                            div()
                                .flex_none()
                                .max_w(px(120.))
                                .truncate()
                                .child(r.app.clone().unwrap_or_else(|| "未知应用".into())),
                        )
                        .when(latency, |d| {
                            let bad = !matches!(r.llm.as_str(), "ok" | "off");
                            d.child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(hex(if bad { AMBER } else { TEXT3 }))
                                    .when(r.outcome.is_none(), |d| d.child(latency_line(r))),
                            )
                        })
                        .child(div().ml_auto().flex_none().child(right)),
                )
                .child(text.truncate().text_size(px(13.)).text_color(hex(TEXT))),
        )
}

/// Swift's "首字 X · 识别 X · LLM X · 粘贴 X", in ms.
fn latency_line(r: &Record) -> String {
    let n = |v: Option<i64>| v.map_or("—".to_string(), thousands);
    let llm = if r.llm == "ok" {
        n(r.llm_ms)
    } else {
        r.llm.clone()
    };
    format!(
        "首字 {} · 识别 {} · LLM {llm} · 粘贴 {}",
        n(r.first_text_ms),
        n(Some(r.asr_ms)),
        n(Some(r.total_ms))
    )
}

/// Newest first, a header before each local day: "今天 12 句".
fn group(shown: &[&Record]) -> Vec<Line> {
    let today = Local::now().date_naive();
    let mut out = vec![];
    let mut i = 0;
    while i < shown.len() {
        let day = store::day(shown[i].at);
        let n = shown[i..]
            .iter()
            .take_while(|r| store::day(r.at) == day)
            .count();
        let d = shown[i].at.with_timezone(&Local).date_naive();
        let title = if d == today {
            "今天".to_string()
        } else if today.pred_opt() == Some(d) {
            "昨天".into()
        } else {
            const WEEK: [&str; 7] = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
            format!(
                "{}月{}日 {}",
                d.month(),
                d.day(),
                WEEK[d.weekday().num_days_from_monday() as usize]
            )
        };
        out.push(Line::Day(format!("{title} · {n} 条")));
        out.extend(
            shown[i..i + n]
                .iter()
                .map(|r| Line::Rec(Box::new((*r).clone()))),
        );
        i += n;
    }
    out
}

fn matches(r: &Record, q: &str) -> bool {
    q.is_empty()
        || r.text.to_lowercase().contains(q)
        || r.raw.to_lowercase().contains(q)
        || r.app.as_ref().is_some_and(|a| a.to_lowercase().contains(q))
}

// --- Helpers ---------------------------------------------------------------------------------------------------

/// The light pill for the main action of the panel.
fn primary(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    hit(id)
        .flex()
        .flex_none()
        .items_center()
        .h(px(30.))
        .px(px(16.))
        .rounded_full()
        .bg(hex(TEXT))
        .text_color(hex(BG))
        .text_size(px(13.))
        .font_weight(medium())
        .whitespace_nowrap()
        .hover(|s| s.opacity(0.9))
        .child(label.into())
        .on_click(on_click)
}

/// A labeled text block in the panel.
fn block(label: &'static str, body: Div) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .font(mono())
                .text_size(px(10.))
                .text_color(hex(TEXT3))
                .child(label),
        )
        .child(body)
}

/// Two-letter app mark: initials of a multi-word name, else its first letters (first character for CJK).
fn badge(app: &str) -> String {
    let words: Vec<&str> = app.split_whitespace().collect();
    if words.len() > 1 && app.is_ascii() {
        words
            .iter()
            .take(2)
            .filter_map(|w| w.chars().next())
            .collect()
    } else if app.is_ascii() {
        app.chars().take(2).collect()
    } else {
        app.chars().take(1).collect()
    }
}

/// "今天 14:32", "昨天 09:10" or "11月18日 14:32".
fn when(at: DateTime<Utc>) -> String {
    let l = at.with_timezone(&Local);
    let today = Local::now().date_naive();
    let day = if l.date_naive() == today {
        "今天".to_string()
    } else if today.pred_opt() == Some(l.date_naive()) {
        "昨天".into()
    } else {
        format!("{}月{}日", l.month(), l.day())
    };
    format!("{day} {}", l.format("%H:%M"))
}

/// 64 bucket peaks of a recording, 0..1 relative to its loudest bucket (flat when silent or unreadable).
fn peaks(path: &Path) -> Vec<f32> {
    const N: usize = 64;
    let bytes = std::fs::read(path).unwrap_or_default();
    let pcm = plaid_core::wav::decode(&bytes).unwrap_or(&[]);
    let samples: Vec<i16> = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect();
    let per = samples.len().div_ceil(N).max(1);
    let mut v: Vec<f32> = (0..N)
        .map(|i| {
            samples
                .get(i * per..((i + 1) * per).min(samples.len()))
                .map_or(0, |c| c.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0))
                as f32
        })
        .collect();
    let top = v.iter().cloned().fold(0., f32::max);
    // Below about -50 dBFS counts as silence.
    if top > 100. {
        v.iter_mut().for_each(|a| *a /= top);
    } else {
        v.iter_mut().for_each(|a| *a = 0.);
    }
    v
}

/// A button that does nothing and is drawn dimmed when `on` is false.
fn enabled(on: bool, b: Stateful<Div>) -> Stateful<Div> {
    if on { b } else { disabled(b).occlude() }
}

fn reveal_label() -> &'static str {
    if cfg!(windows) {
        "在资源管理器中显示"
    } else {
        "在访达中显示"
    }
}

fn thousands(v: i64) -> String {
    let s = v.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if v < 0 { format!("-{out}") } else { out }
}

fn ms(v: Option<i64>) -> String {
    v.map_or("—".into(), |v| format!("{} ms", thousands(v)))
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok()?.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

fn size_label(b: u64) -> String {
    let mb = b as f64 / 1_048_576.;
    if mb >= 10. {
        format!("{mb:.0} MB")
    } else if mb >= 0.1 {
        format!("{mb:.1} MB")
    } else {
        format!("{} KB", (b / 1024).max(1))
    }
}

// --- Export ----------------------------------------------------------------------------------------------------

/// Non-secret facts for a feedback bundle, as the Swift app writes them (unset settings as "default").
fn export_info(facts: &super::Facts) -> serde_json::Map<String, serde_json::Value> {
    let mut info = serde_json::Map::new();
    info.insert("version".into(), env!("CARGO_PKG_VERSION").into());
    info.insert("os".into(), os_version().into());
    let chosen = config::value("micDevice");
    let mic = chosen
        .as_ref()
        .and_then(|c| {
            facts
                .mics
                .iter()
                .find(|(id, _)| id == c)
                .map(|(_, n)| n.clone())
        })
        .or_else(|| facts.default_mic.clone());
    if let Some(m) = mic {
        info.insert("mic".into(), m.into());
    }
    for k in [
        "micDevice",
        "nonstream",
        "silenceRMS",
        "tailMs",
        "codec",
        "asrContext",
        "learnEdits",
        "autoLearn",
        "llmEnabled",
        "llmFormat",
        "llmBaseUrl",
        "llmModel",
        "llmTimeoutMs",
    ] {
        info.insert(
            k.into(),
            config::value(k).unwrap_or_else(|| "default".into()).into(),
        );
    }
    info
}

fn os_version() -> String {
    #[cfg(target_os = "macos")]
    let v = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .map(|o| format!("macOS {}", String::from_utf8_lossy(&o.stdout).trim()));
    #[cfg(not(target_os = "macos"))]
    let v: Option<String> = None;
    v.unwrap_or_else(|| std::env::consts::OS.to_string())
}

/// Writes `<out stem>/{records.jsonl, audio/, hotwords.txt, events.log, info.json}` to a temp folder and zips it
/// to `out` (the folder itself is the zip's top entry).
fn write_bundle(
    data: &Path,
    records: &[Record],
    info: &serde_json::Map<String, serde_json::Value>,
    out: &Path,
) -> Result<(), String> {
    let stem = out
        .file_stem()
        .ok_or("bad file name")?
        .to_string_lossy()
        .to_string();
    let tmp = std::env::temp_dir().join(format!(
        "plaid-export-{}-{}",
        std::process::id(),
        Utc::now().timestamp_millis()
    ));
    let result = (|| -> Result<(), String> {
        let root = tmp.join(&stem);
        std::fs::create_dir_all(root.join("audio")).map_err(|e| e.to_string())?;
        let mut lines = String::new();
        for r in records {
            // Value maps are BTreeMaps: keys come out sorted, as the Swift export writes them.
            let v = serde_json::to_value(r).map_err(|e| e.to_string())?;
            lines.push_str(&serde_json::to_string(&v).map_err(|e| e.to_string())?);
            lines.push('\n');
        }
        std::fs::write(root.join("records.jsonl"), lines).map_err(|e| e.to_string())?;
        for a in records.iter().filter_map(|r| r.audio.as_deref()) {
            if let Some(name) = Path::new(a).file_name() {
                let _ = std::fs::copy(data.join("audio").join(name), root.join("audio").join(name));
            }
        }
        let _ = std::fs::copy(data.join("hotwords.txt"), root.join("hotwords.txt"));
        let _ = std::fs::copy(data.join("events.log"), root.join("events.log"));
        let info = serde_json::to_vec_pretty(info).map_err(|e| e.to_string())?;
        std::fs::write(root.join("info.json"), info).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(out);
        zip(&tmp, &stem, out)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn zip(parent: &Path, folder: &str, out: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("/usr/bin/ditto")
        .args([
            "-c",
            "-k",
            "--norsrc",
            "--noextattr",
            "--noacl",
            "--keepParent",
        ])
        .arg(parent.join(folder))
        .arg(out)
        .status();
    #[cfg(not(target_os = "macos"))]
    let status = {
        let mut c = std::process::Command::new("tar");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        c.args(["-a", "-c", "-f"])
            .arg(out)
            .arg("-C")
            .arg(parent)
            .arg(folder)
            .status()
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("zip exited with {s}")),
        Err(e) => Err(format!("zip: {e}")),
    }
}

// --- Offscreen check -------------------------------------------------------------------------------------------

#[cfg(all(feature = "snapshot", any(target_os = "macos", target_os = "linux")))]
pub(super) fn check(ui: &mut super::snapshot::Ui) -> Result<(), String> {
    use super::snapshot::expect;
    let dir = config::data_dir();
    let painted = || -> Vec<String> {
        PROBES.with(|p| {
            p.borrow()
                .keys()
                .filter(|k| k.starts_with("history-row-"))
                .cloned()
                .collect()
        })
    };
    let all = plaid_core::store::Store::load(&dir).all;
    let row_id = |r: &Record| format!("history-row-{}", r.at.timestamp_millis());
    let failed: Vec<String> = all.iter().filter(|r| r.is_failure()).map(row_id).collect();
    let with_audio = all
        .iter()
        .rev()
        .find(|r| r.is_failure() && r.audio.is_some())
        .ok_or("fixture has no failed record with audio")?;
    let target = row_id(with_audio);

    ui.open(Page::History, super::snapshot::facts())?;
    expect(
        "history rows painted",
        !painted().is_empty() && ui.has("history-filter-0"),
        &None,
    )?;
    ui.shot("history")?;

    ui.click("history-filter-2")?;
    let rows = painted();
    expect(
        "failed filter shows only failed rows",
        !rows.is_empty() && rows.iter().all(|k| failed.contains(k)),
        &Some(format!("{} painted", rows.len())),
    )?;
    ui.click(&target)?;
    expect(
        "detail with retry for a failed record with audio",
        ui.has("history-detail") && ui.has("history-retry") && ui.has("history-play"),
        &None,
    )?;
    ui.click("history-details")?;
    ui.shot("history-failed-detail")?;
    ui.click("history-retry")?;
    expect("retry asks first", ui.has("history-retry-ok"), &None)?;
    ui.shot("history-retry-confirm")?;
    // Without the running app there is no controller: the post is dropped, the page shows it as in flight.
    ui.click("history-retry-ok")?;
    expect(
        "retry in flight",
        ui.has("history-status") && !ui.has("history-retry-ok"),
        &None,
    )?;

    ui.click("history-filter-1")?;
    let saved: Vec<String> = all
        .iter()
        .filter(|r| r.outcome.as_deref() == Some("saved"))
        .map(row_id)
        .collect();
    let rows = painted();
    expect(
        "saved filter shows only saved rows",
        !rows.is_empty() && rows.iter().all(|k| saved.contains(k)),
        &None,
    )?;
    expect(
        "filter change shows the newest matching record",
        ui.has("history-detail"),
        &None,
    )?;
    for (i, name, keeps) in [
        (
            3,
            "llm",
            (|r: &Record| r.llm == "ok") as fn(&Record) -> bool,
        ),
        (4, "edited", |r: &Record| r.edited.is_some()),
    ] {
        ui.click(&format!("history-filter-{i}"))?;
        let want: Vec<String> = all.iter().filter(|r| keeps(r)).map(row_id).collect();
        let rows = painted();
        expect(
            &format!("{name} filter shows only its rows"),
            !rows.is_empty() && rows.iter().all(|k| want.contains(k)),
            &Some(format!("{} painted", rows.len())),
        )?;
        ui.shot(&format!("history-{name}"))?;
    }
    // A record the local rules changed: its recognized text shows with the removals struck through.
    ui.click("history-filter-0")?;
    if let Some(r) = all
        .iter()
        .rev()
        .find(|r| r.asr.is_some() && !r.is_failure())
    {
        let id = row_id(r);
        for _ in 0..20 {
            if ui.bounds(&id).is_some() {
                break;
            }
            let over = painted().into_iter().next().ok_or("no rows")?;
            ui.scroll(&over, -300.)?;
        }
        ui.click(&id)?;
        ui.shot("history-rules")?;
        let over = painted().into_iter().next().ok_or("no rows")?;
        ui.scroll(&over, 20000.)?;
    }

    ui.click("history-filter-0")?;
    let before = config::raw(LATENCY);
    ui.click("history-actions")?;
    ui.shot("history-actions")?;
    ui.click("history-actions-menu-0")?;
    expect(
        "latency toggle writes historyLatency",
        config::raw(LATENCY).as_deref() == Some("1") && !ui.has("history-actions-menu-0"),
        &before,
    )?;
    let first = painted()
        .into_iter()
        .filter_map(|k| ui.bounds(&k).map(|b| (f32::from(b.origin.y), k)))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, k)| k)
        .ok_or("no rows")?;
    ui.click(&first)?;
    expect("detail for a pasted record", ui.has("history-copy"), &None)?;
    ui.shot("history-latency")?;
    let y = ui.bounds(&first).map(|b| b.origin.y);
    ui.scroll(&first, -600.)?;
    expect(
        "list scrolls",
        ui.bounds(&first).map(|b| b.origin.y) != y,
        &None,
    )?;
    ui.click("history-actions")?;
    ui.click("history-actions-menu-0")?;
    expect(
        "latency toggled off",
        config::raw(LATENCY).as_deref() == Some("0"),
        &None,
    )?;

    ui.click("history-clear")?;
    expect("clear asks first", ui.has("history-clear-ok"), &None)?;
    ui.shot("history-clear-confirm")?;
    ui.click("history-clear-cancel")?;
    expect(
        "cancel keeps the history",
        dir.join("records.jsonl").exists() && !ui.has("history-clear-ok"),
        &None,
    )?;
    ui.click("history-clear")?;
    ui.click("history-clear-ok")?;
    expect(
        "clear removes records and audio, keeps usage",
        !dir.join("records.jsonl").exists()
            && !dir.join("audio").exists()
            && dir.join("usage.json").exists()
            && ui.has("history-empty"),
        &None,
    )?;
    ui.shot("history-empty")?;
    ui.close()?;
    // Later checks expect the fixture history.
    super::snapshot::fixture(&dir).map_err(|e| format!("fixture: {e}"))
}
