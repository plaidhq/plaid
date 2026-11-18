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
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

const LATENCY: &str = "historyLatency";

/// Column widths (0 = flexible), as in the canvas; the latency column only when `historyLatency` is on.
const TIME_W: f32 = 72.;
const APP_W: f32 = 84.;
const LATENCY_W: f32 = 290.;
const STATUS_W: f32 = 78.;
const DURATION_W: f32 = 58.;

const SAVED_COLOR: u32 = 0xa85a00;
const FAILED_COLOR: u32 = 0xc0281f;
const PASTED_COLOR: u32 = 0x555555;
const WARN_COLOR: u32 = 0xb35c00;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Saved,
    Failed,
}

impl Filter {
    fn keeps(self, r: &Record) -> bool {
        match self {
            Filter::All => true,
            Filter::Saved => r.outcome.as_deref() == Some("saved"),
            Filter::Failed => r.is_failure(),
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

    fn toolbar(&self, counts: [usize; 3], shown: usize, cx: &mut Context<Self>) -> Div {
        let labels = [
            format!("全部 {}", counts[0]),
            format!("未粘贴 {}", counts[1]),
            format!("失败 {}", counts[2]),
        ];
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let selected = match self.filter {
            Filter::All => 0,
            Filter::Saved => 1,
            Filter::Failed => 2,
        };
        let this = cx.entity().downgrade();
        let seg = segmented("history-filter", &refs, selected, move |i, _, cx| {
            let f = [Filter::All, Filter::Saved, Filter::Failed][i.min(2)];
            let _ = this.update(cx, |h, cx| h.set_filter(f, cx));
        })
        .text_size(px(12.));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .py(px(9.))
            .child(seg)
            .child(div().flex_1())
            .child(self.action_button(cx))
            .child(self.search_field(shown, cx))
    }

    fn search_field(&self, shown: usize, cx: &mut Context<Self>) -> Div {
        let typed = !self.search.read(cx).value().is_empty();
        let search = self.search.clone();
        div()
            .w(px(220.))
            .h(px(22.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(5.))
            .px(px(9.))
            .rounded(px(11.))
            .bg(hex(0xffffff))
            .border_1()
            .border_color(hex(0x9c9c9c))
            .shadow(vec![shadow(0., 1., 1., 0., 0x00000024, true)])
            .text_size(px(12.))
            .child(icon("plaid/search.svg", 12., hex(0x666666)))
            .child(div().flex_1().min_w_0().child(Input::new(&self.search)))
            .when(typed, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(hex(HINT))
                        .child(format!("{shown} 条")),
                )
                .child(
                    hit("history-search-clear")
                        .flex_none()
                        .child(icon("plaid/xcircle.svg", 12., hex(0xa3a3a3)))
                        .on_click(move |_, w, cx| {
                            search.update(cx, |s, cx| s.set_value("", w, cx));
                        }),
                )
            })
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
            .gap(px(3.))
            .h(px(21.))
            .px(px(7.))
            .rounded(px(4.))
            .bg(vgrad(0xffffff, 0xf0f0f0))
            .border_1()
            .border_color(hex(0xa5a5a5))
            .shadow(vec![shadow(0., 1., 1., 0., 0x00000012, false)])
            .active(|s| s.bg(vgrad(0xd9d9d9, 0xc9c9c9)))
            .child(icon("plaid/action.svg", 14., hex(0x3a3a3a)))
            .child(icon("plaid/tri-down.svg", 6., hex(0x3a3a3a)))
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

    /// Retry progress or result, and the export spinner, under the toolbar.
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
                .px(px(14.))
                .pb(px(6.))
                .when(busy, |d| d.child(spinner(11., 0x555555, 0)))
                .child(hint(text).text_color(hex(0x333333))),
        )
    }

    fn header(latency: bool) -> Div {
        let mut cols = vec![("时间", TIME_W), ("应用", APP_W), ("文本", 0.)];
        if latency {
            cols.push(("延迟（ms）", LATENCY_W));
        }
        cols.push(("状态", STATUS_W));
        cols.push(("用时", DURATION_W));
        table_header(&cols).text_color(hex(0x222222))
    }

    fn table(
        &self,
        lines: Rc<Vec<Line>>,
        latency: bool,
        empty: Option<(&'static str, &'static str)>,
        cx: &mut Context<Self>,
    ) -> Div {
        let well = table_well()
            .flex_1()
            .min_h_0()
            .mx(px(12.))
            .border_color(hex(0x9f9f9f))
            .child(Self::header(latency));
        if let Some((title, body)) = empty {
            return well.child(
                hit("history-empty")
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .font_weight(bold())
                            .text_color(hex(0x555555))
                            .child(title),
                    )
                    .child(hint(body)),
            );
        }
        let this = cx.entity().downgrade();
        let selected = Rc::new(self.selected.clone());
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
        well.child(
            uniform_list("history-list", n, move |range, _, _| {
                range
                    .map(|i| match &lines[i] {
                        Line::Day(label) => day_row(label.clone()).into_any_element(),
                        Line::Rec(r) => {
                            let sel = selected.contains(&r.at);
                            let id = r.at;
                            let (t1, t2, ord) = (this.clone(), this.clone(), order.clone());
                            record_row(r, i, sel, latency, retrying == Some(r.at))
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
                                })
                                .into_any_element()
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

    fn detail(&self, cx: &mut Context<Self>) -> Option<Div> {
        let this = cx.entity().downgrade();
        let frame = hit("history-detail")
            .flex()
            .flex_col()
            .flex_none()
            .max_h(px(330.))
            .overflow_y_scroll()
            .gap(px(10.))
            .mx(px(12.))
            .mt(px(10.))
            .px(px(16.))
            .py(px(12.))
            .rounded(px(5.))
            .bg(hexa(0x0000000a))
            .border_1()
            .border_color(hexa(0x00000024));
        if self.selected.len() > 1 {
            let n = self.selected.len();
            let ids = self.selected.clone();
            let (t1, t2) = (this.clone(), this);
            let ids2 = ids.clone();
            return Some(
                div().child(
                    frame.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().font_weight(bold()).child(format!("已选 {n} 条")))
                            .child(div().flex_1())
                            .child(button(
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
                ),
            );
        }
        let id = *self.selected.first()?;
        let r = self.record(id, cx)?.clone();
        let wav = self.audio_path(&r, cx);
        let retrying = self.retry.as_ref().is_some_and(|x| x.id == id);
        let local = r.at.with_timezone(&Local);
        let app = r.app.clone().unwrap_or_else(|| "未知应用".into());
        let state = if retrying {
            "重新识别中…".to_string()
        } else if r.is_failure() {
            "识别失败 · 原记录和保留的录音可用于排查或重试".into()
        } else if r.outcome.as_deref() == Some("saved") {
            "未粘贴 · 文字已保存，需手动复制；不会自动粘贴".into()
        } else if r.edited.is_some() {
            "已粘贴 · 你修改过这句".into()
        } else {
            "已粘贴".into()
        };
        let delivered = r.outcome.is_none();
        let llm_bad = !matches!(r.llm.as_str(), "ok" | "off");
        let metrics = [
            ("首字", ms(r.first_text_ms), false),
            ("识别", ms(Some(r.asr_ms).filter(|v| *v > 0)), false),
            (
                "LLM",
                match r.llm.as_str() {
                    "ok" => ms(r.llm_ms),
                    "off" => "—".into(),
                    other => other.to_string(),
                },
                llm_bad,
            ),
            (
                "粘贴",
                if delivered {
                    ms(Some(r.total_ms))
                } else {
                    "—".into()
                },
                false,
            ),
        ];
        let text = r.text.clone();
        let (t_copy, t_play, t_retry, t_details) = (this.clone(), this.clone(), this.clone(), this);
        let mut buttons = div()
            .ml_auto()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(enabled(
                !text.is_empty(),
                button("history-copy", "复制", move |_, _, cx| {
                    let _ = t_copy.update(cx, |h, cx| h.copy(&[id], cx));
                }),
            ));
        if let Some(path) = wav.clone() {
            let p2 = path.clone();
            buttons = buttons
                .child(button("history-play", "播放录音", move |_, _, cx| {
                    let p = path.clone();
                    let _ = t_play.update(cx, |h, _| h.play(&p));
                }))
                .child(button("history-reveal", reveal_label(), move |_, _, _| {
                    reveal(&p2)
                }))
                .child(enabled(
                    self.retry.is_none(),
                    button("history-retry", "重新识别…", move |_, _, cx| {
                        let _ = t_retry.update(cx, |h, cx| {
                            h.sheet = Some(Sheet::Retry(id));
                            cx.notify();
                        });
                    }),
                ));
        }
        let mut fields: Vec<(&str, String, bool)> = vec![];
        if let Some(e) = &r.error {
            fields.push(("错误", e.clone(), true));
        }
        fields.push(("应用", app.clone(), false));
        if let Some(v) = r.trigger_ms {
            fields.push(("按键 → 启动", format!("{v} ms"), false));
        }
        if let Some(v) = r.mic_start_ms {
            fields.push(("麦克风启动", format!("{v} ms"), false));
        }
        if !r.raw.is_empty() && r.raw != r.text {
            fields.push(("识别", r.raw.clone(), false));
        }
        if let Some(e) = &r.edited {
            fields.push(("修改后", e.clone(), false));
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
        if llm_bad {
            fields.push(("LLM", r.llm.clone(), true));
        }
        let details = self.details;
        Some(
            div().child(
                frame
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(10.))
                            .child(
                                div()
                                    .font_weight(bold())
                                    .child(format!("{} · {app}", local.format("%H:%M"))),
                            )
                            .child(
                                hint(state)
                                    .when(r.is_failure(), |d| d.text_color(hex(FAILED_COLOR))),
                            ),
                    )
                    .child(if text.is_empty() {
                        div().text_color(hex(0x8a8a8a)).child("没有识别结果")
                    } else {
                        div()
                            .line_height(px(20.))
                            .text_color(hex(0x1a1a1a))
                            .child(text)
                    })
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(28.))
                            .children(metrics.into_iter().map(|(k, v, warn)| {
                                div().flex().flex_col().gap(px(1.)).child(hint(k)).child(
                                    div()
                                        .font_weight(bold())
                                        .text_color(hex(if warn { WARN_COLOR } else { 0x222222 }))
                                        .child(v),
                                )
                            }))
                            .child(buttons),
                    )
                    .when(wav.is_none(), |d| {
                        d.child(hint("没有可用录音，无法重新识别。"))
                    })
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
                        d.child(div().flex().flex_col().gap(px(3.)).pl(px(15.)).children(
                            fields.into_iter().map(|(k, v, warn)| {
                                div()
                                    .flex()
                                    .gap(px(10.))
                                    .text_size(px(11.))
                                    .child(
                                        div()
                                            .w(px(80.))
                                            .flex_none()
                                            .flex()
                                            .justify_end()
                                            .text_color(hex(HINT))
                                            .child(k),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_color(hex(if warn {
                                                WARN_COLOR
                                            } else {
                                                0x222222
                                            }))
                                            .child(v),
                                    )
                            }),
                        ))
                    }),
            ),
        )
    }

    fn bottom(&self, n: usize, cx: &mut Context<Self>) -> Div {
        let mut text = format!("最近 {n} 条，最多保留 {LIMIT} 条，只保存在本机");
        if self.audio_bytes > 0 {
            text.push_str(&format!("，录音占用 {}", size_label(self.audio_bytes)));
        }
        let this = cx.entity().downgrade();
        bottom_bar()
            .h(px(24.))
            .mt(px(10.))
            .px(px(10.))
            .gap(px(10.))
            .border_color(hex(0x9f9f9f))
            .child(hint(text).text_color(hex(0x333333)))
            .child(div().flex_1())
            .child(enabled(
                n > 0,
                small_button("history-clear", "清空历史…", move |_, _, cx| {
                    let _ = this.update(cx, |h, cx| {
                        h.sheet = Some(Sheet::Clear(n));
                        cx.notify();
                    });
                }),
            ))
    }

    /// A 10.9 sheet: hangs from the top of the pane, blocks the page behind it.
    fn sheet(&self, sheet: &Sheet, cx: &mut Context<Self>) -> Stateful<Div> {
        let (key, title, body, ok) = match sheet {
            Sheet::Clear(n) => (
                "history-clear",
                format!("删除全部 {n} 条记录和录音？"),
                "概览里的用量统计会保留。",
                "删除",
            ),
            Sheet::Retry(_) => (
                "history-retry",
                "重新识别这段录音？".to_string(),
                "这段录音将上传到当前云端识别服务。结果保存到原记录，不自动粘贴，也不运行优化或命令。",
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
                    .bg(hex(0xececec))
                    .border_1()
                    .border_t_0()
                    .border_color(hexa(0x00000066))
                    .rounded_b(px(5.))
                    .shadow(vec![shadow(0., 8., 24., 0., 0x00000066, false)])
                    .child(div().font_weight(bold()).child(title))
                    .child(
                        div()
                            .text_size(px(11.))
                            .line_height(px(15.))
                            .text_color(hex(0x333333))
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
        let latency = on_if_one(LATENCY);
        let store = &self.model.read(cx).store;
        let total = store.all.len();
        let found: Vec<&Record> = store.all.iter().filter(|r| matches(r, &q)).collect();
        let counts = [
            found.len(),
            found.iter().filter(|r| Filter::Saved.keeps(r)).count(),
            found.iter().filter(|r| Filter::Failed.keeps(r)).count(),
        ];
        let shown: Vec<&Record> = found
            .into_iter()
            .rev()
            .filter(|r| self.filter.keeps(r))
            .collect();
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
            Some(("没有匹配的记录", "试试其他关键词，或切换记录状态。"))
        } else if self.filter == Filter::Saved {
            Some(("没有匹配的记录", "没有已保存但未粘贴的记录。"))
        } else {
            Some(("没有匹配的记录", "没有识别失败的记录。"))
        };
        let context = self.context;
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(self.toolbar(counts, n_shown, cx))
            .children(self.status_line())
            .child(self.table(lines, latency, empty, cx))
            .children(self.detail(cx))
            .child(self.bottom(total, cx))
            .when_some(context, |d, at| d.child(self.context_menu(at, cx)))
            .when_some(self.sheet.as_ref(), |d, s| d.child(self.sheet(s, cx)))
    }
}

// --- Rows ------------------------------------------------------------------------------------------------------

fn day_row(label: String) -> Div {
    div()
        .w_full()
        .h(px(20.))
        .flex()
        .items_center()
        .px(px(6.))
        .bg(vgrad(0xf6f6f6, 0xebebeb))
        .border_b_1()
        .border_color(hex(0xdcdcdc))
        .text_size(px(11.))
        .font_weight(bold())
        .text_color(hex(0x6b6b6b))
        .child(label)
}

fn record_row(
    r: &Record,
    i: usize,
    selected: bool,
    latency: bool,
    retrying: bool,
) -> Stateful<Div> {
    let local = r.at.with_timezone(&Local);
    let color = |c: u32| if selected { hex(0xffffff) } else { hex(c) };
    let (status, status_color) = if retrying {
        ("重新识别中…", PASTED_COLOR)
    } else if r.is_failure() {
        ("识别失败", FAILED_COLOR)
    } else if r.outcome.as_deref() == Some("saved") {
        ("未粘贴", SAVED_COLOR)
    } else {
        ("已粘贴", PASTED_COLOR)
    };
    let text = if r.text.is_empty() {
        div().text_color(color(0x8a8a8a)).child("没有识别结果")
    } else {
        div().child(r.text.replace('\n', " "))
    };
    let delivered = r.outcome.is_none();
    table_row(
        format!("history-row-{}", r.at.timestamp_millis()),
        i,
        selected,
    )
    .w_full()
    .h(px(20.))
    .text_size(px(12.))
    .child(cell(TIME_W).child(local.format("%H:%M").to_string()))
    .child(
        cell(APP_W).child(
            div()
                .truncate()
                .child(r.app.clone().unwrap_or_else(|| "未知应用".into())),
        ),
    )
    .child(cell(0.).child(text.truncate()))
    .when(latency, |d| {
        let bad = !matches!(r.llm.as_str(), "ok" | "off");
        d.child(
            cell(LATENCY_W)
                .text_size(px(11.))
                .text_color(color(if bad { WARN_COLOR } else { 0x555555 }))
                .child(
                    div()
                        .truncate()
                        .when(delivered, |d| d.child(latency_line(r))),
                ),
        )
    })
    .child(
        cell(STATUS_W)
            .flex()
            .items_center()
            .gap(px(3.))
            .text_color(color(status_color))
            .when(r.edited.is_some() && delivered, |d| {
                d.child(icon("plaid/pencil.svg", 10., color(0x6b6b6b)))
            })
            .child(div().truncate().child(status)),
    )
    .child(
        cell(DURATION_W)
            .flex()
            .justify_end()
            .child(if r.audio_ms > 0 {
                format!("{:.1} s", r.audio_ms as f64 / 1000.)
            } else {
                "—".into()
            }),
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
        out.push(Line::Day(format!("{title} {n} 句")));
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
        "filter change clears the selection",
        !ui.has("history-detail"),
        &None,
    )?;

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
