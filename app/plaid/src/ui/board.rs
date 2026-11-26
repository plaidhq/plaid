//! 鲸喷榜: a leaderboard of Plaid Cloud users over the last 7 or 30 days, from `GET /v1/board`. Two boards: 喷量
//! (characters delivered) and 喷速 (characters per minute of audio, for users past a minimum of audio). The server
//! keeps counts only; leaving the board hides the caller and shows them nobody else.
use super::Model;
use super::mav::*;
use super::overview::thousands;
use crate::{config, secret};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use plaid_core::cloud::{self, Board as Data, BoardRow};

const RANGES: [i64; 2] = [7, 30];
const RANGE_NAMES: [&str; 2] = ["近 7 天", "近 30 天"];

/// Audio a user needs in the range before they are ranked by speed: a few short bursts are not a speed.
fn speed_floor_ms(days: i64) -> i64 {
    if days <= 7 { 10 * 60_000 } else { 30 * 60_000 }
}

/// Book-length yardsticks for "相当于…", largest first: (characters, count word, name).
const YARDSTICKS: [(i64, &str, &str); 5] = [
    (730_000, "部", "《红楼梦》"),
    (120_000, "本", "《活着》"),
    (30_000, "本", "《小王子》"),
    (800, "篇", "高考作文"),
    (140, "条", "微博"),
];

fn days() -> i64 {
    config::value("boardDays")
        .and_then(|s| s.parse::<f64>().ok())
        .map(|v| v as i64)
        .filter(|v| RANGES.contains(v))
        .unwrap_or(7)
}

fn speed(r: &BoardRow) -> f64 {
    if r.audio_ms <= 0 {
        return 0.;
    }
    r.chars as f64 * 60_000. / r.audio_ms as f64
}

/// "相当于 2.3 本《小王子》"; None under one 微博.
fn yardstick(chars: i64) -> Option<String> {
    let (size, word, name) = YARDSTICKS.into_iter().find(|(n, _, _)| chars >= *n)?;
    let n = chars as f64 / size as f64;
    Some(if n >= 10. || size < 30_000 {
        format!("相当于 {} {word}{name}", thousands(n as i64))
    } else {
        format!("相当于 {n:.1} {word}{name}")
    })
}

fn volume_title(rank: usize) -> &'static str {
    match rank {
        1 => "蓝鲸",
        2 => "座头鲸",
        3 => "虎鲸",
        _ => "海豚",
    }
}

fn speed_title(rank: usize) -> Option<&'static str> {
    match rank {
        1 => Some("机关枪"),
        2 => Some("连珠炮"),
        3 => Some("快嘴"),
        _ => None,
    }
}

/// Medal colors for the top three, then plain.
fn rank_color(rank: usize) -> u32 {
    match rank {
        1 => AMBER,
        2 => CYAN,
        3 => VIOLET,
        _ => TEXT3,
    }
}

pub struct Board {
    model: Entity<Model>,
    data: Option<Result<Data, String>>,
    busy: bool,
    /// Snapshot runs: whether the fixture user takes part (no server to ask).
    #[cfg_attr(not(feature = "snapshot"), allow(dead_code))]
    fixture_joined: bool,
}

impl Board {
    pub fn new(model: Entity<Model>, _: &mut Window, cx: &mut Context<Self>) -> Board {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        Board {
            model,
            data: None,
            busy: false,
            fixture_joined: true,
        }
    }

    /// The page came into view: standings move between visits.
    pub fn shown(&mut self, cx: &mut Context<Self>) {
        self.fetch(cx);
    }

    fn token(&self, cx: &App) -> Option<String> {
        secret::get("cloudToken")
            .ok()
            .flatten()
            .filter(|t| !t.is_empty())
            .filter(|_| self.model.read(cx).facts.cloud_token)
    }

    fn fetch(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let days = days();
        if self.model.read(cx).facts.fixture {
            #[cfg(feature = "snapshot")]
            {
                self.data = Some(Ok(fixture(days, self.fixture_joined)));
            }
            return cx.notify();
        }
        let Some(token) = self.token(cx) else {
            self.data = None;
            return cx.notify();
        };
        let base = config::cloud_url();
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move { cloud::board(&base, &token, days) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                // Keep the last standings on a failed refresh; show the error only when there is nothing else.
                match r {
                    Ok(b) => this.data = Some(Ok(b)),
                    Err(e) if !matches!(this.data, Some(Ok(_))) => this.data = Some(Err(e)),
                    Err(e) => crate::trace::log(&format!("board: {e}")),
                }
                if days != self::days() {
                    this.fetch(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn join(&mut self, join: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.model.read(cx).facts.fixture {
            self.fixture_joined = join;
            return self.fetch(cx);
        }
        let Some(token) = self.token(cx) else {
            return;
        };
        let base = config::cloud_url();
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move { cloud::board_join(&base, &token, join) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(()) => this.fetch(cx),
                    Err(e) => this.data = Some(Err(e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Centered message card with an optional button.
    fn notice(&self, title: &str, sub: String, button: Option<Stateful<Div>>) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .h(px(200.))
            .rounded(px(12.))
            .bg(hexa(GLASS))
            .border_1()
            .border_color(hexa(LINE))
            .child(div().font_weight(bold()).child(title.to_string()))
            .child(hint(sub).text_size(px(12.)))
            .children(button.map(|b| b.mt(px(6.))))
    }

    /// The caller's card: rank and title, characters, speed, a yardstick and the chase line.
    fn hero(&self, b: &Data, days: i64) -> AnyElement {
        let rows = &b.rows;
        let Some(i) = rows.iter().position(|r| r.name == b.me) else {
            return self
                .notice(
                    "还没上榜",
                    format!("近 {days} 天还没有 Cloud 听写。按住说话键喷一段，就会出现在这里。"),
                    None,
                )
                .into_any_element();
        };
        let me = &rows[i];
        let rank = i + 1;
        let chase = if rank > 1 {
            let above = &rows[i - 1];
            format!(
                "再喷 {} 字超过 {}",
                thousands(above.chars - me.chars + 1),
                above.name
            )
        } else if let Some(next) = rows.get(1) {
            format!(
                "领先 {} {} 字，稳坐鲸王",
                next.name,
                thousands(me.chars - next.chars)
            )
        } else {
            "目前榜上只有你，一鲸独喷".into()
        };
        let floor = speed_floor_ms(days);
        let speed_line = if me.audio_ms >= floor {
            let mut fast: Vec<&BoardRow> = rows.iter().filter(|r| r.audio_ms >= floor).collect();
            fast.sort_by(|a, b| speed(b).total_cmp(&speed(a)));
            let k = fast
                .iter()
                .position(|r| r.name == b.me)
                .map_or(0, |k| k + 1);
            format!("喷速第 {k} 名")
        } else {
            format!("说满 {} 分钟上喷速榜", floor / 60_000)
        };
        let stat = |label: &str, value: String, unit: &str| {
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(hint(label.to_string()).text_size(px(11.)))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(3.))
                        .child(
                            div()
                                .font(mono())
                                .text_size(px(22.))
                                .line_height(px(28.))
                                .font_weight(medium())
                                .child(value),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(hex(TEXT2))
                                .child(unit.to_string()),
                        ),
                )
        };
        let color = rank_color(rank);
        hit("board-me")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(28.))
            .px(px(24.))
            .py(px(18.))
            .rounded(px(12.))
            .bg(linear_gradient(
                100.,
                linear_color_stop(tint(color, 0x24), 0.),
                linear_color_stop(hexa(GLASS), 0.55),
            ))
            .border_1()
            .border_color(tint(color, 0x40))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .w(px(96.))
                    .child(hint("你的名次").text_size(px(11.)))
                    .child(
                        div()
                            .font(mono())
                            .text_size(px(44.))
                            .line_height(px(52.))
                            .font_weight(bold())
                            .text_color(hex(color))
                            .child(format!("#{rank}")),
                    )
                    .child(chip(volume_title(rank), color)),
            )
            .child(div().w(px(1.)).h(px(84.)).bg(hexa(LINE2)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .gap(px(36.))
                            .child(stat("喷量", thousands(me.chars), "字"))
                            .child(stat("喷速", format!("{:.0}", speed(me)), "字/分"))
                            .child(stat("开喷", thousands(me.dictations), "次")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_x(px(14.))
                            .text_size(px(12.))
                            .text_color(hex(TEXT2))
                            .children(yardstick(me.chars))
                            .child(div().text_color(hex(TEXT)).child(chase))
                            .child(speed_line),
                    ),
            )
            .into_any_element()
    }

    /// One board: rank, name and title, a spout bar against the leader, the value.
    fn board(
        id: &'static str,
        title: &str,
        note: String,
        rows: Vec<(&BoardRow, f64, String, Option<&'static str>)>,
        me: &str,
        empty: String,
        accent: u32,
    ) -> Div {
        let top = rows.first().map_or(1., |r| r.1.max(1e-9));
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .p(px(8.))
            .rounded(px(12.))
            .bg(hexa(GLASS))
            .border_1()
            .border_color(hexa(LINE));
        if rows.is_empty() {
            body = body.child(
                div()
                    .h(px(120.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(hint(empty)),
            );
        }
        for (i, (r, v, label, title)) in rows.iter().enumerate() {
            let rank = i + 1;
            let mine = r.name == me;
            let frac = (*v / top).clamp(0.02, 1.) as f32;
            body = body.child(
                hit(SharedString::from(format!("{id}-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(36.))
                    .px(px(10.))
                    .rounded(px(8.))
                    .when(mine, |d| {
                        d.bg(tint(CYAN, 0x14))
                            .border_1()
                            .border_color(tint(CYAN, 0x38))
                    })
                    .child(
                        div()
                            .w(px(22.))
                            .font(mono())
                            .font_weight(bold())
                            .text_color(hex(rank_color(rank)))
                            .child(rank.to_string()),
                    )
                    .child(
                        div()
                            .w(px(132.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .overflow_hidden()
                            .child(
                                div()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .when(mine, |d| d.font_weight(bold()))
                                    .child(r.name.clone()),
                            )
                            .children(title.map(|t| chip(t, rank_color(rank)))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(8.))
                            .rounded_full()
                            .bg(hexa(GLASS_WELL))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(frac))
                                    .rounded_full()
                                    .bg(hgrada((accent << 8) | 0x66, (accent << 8) | 0xff))
                                    .when(rank == 1, |d| d.shadow(vec![glow(accent, 10., 0x80)])),
                            ),
                    )
                    .child(
                        div()
                            .w(px(64.))
                            .flex()
                            .justify_end()
                            .font(mono())
                            .text_size(px(13.))
                            .text_color(hex(if mine { TEXT } else { TEXT2 }))
                            .child(label.clone()),
                    ),
            );
        }
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .justify_between()
                    .mx(px(6.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(medium())
                            .child(title.to_string()),
                    )
                    .child(hint(note).text_size(px(11.))),
            )
            .child(body)
    }
}

/// Small colored label: rank titles.
fn chip(text: &str, color: u32) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .rounded(px(4.))
        .text_size(px(10.))
        .line_height(px(16.))
        .text_color(hex(color))
        .bg(tint(color, 0x1f))
        .child(text.to_string())
}

impl Render for Board {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = super::page_body("board").gap(px(16.));
        let days = days();
        let m = self.model.clone();
        let this = cx.entity().downgrade();
        let seg = segmented(
            "board-range",
            &RANGE_NAMES,
            RANGES.iter().position(|r| *r == days).unwrap_or(0),
            move |i, _, cx| {
                m.update(cx, |m, cx| m.set("boardDays", RANGES[i], cx));
                let _ = this.update(cx, |b, cx| b.fetch(cx));
            },
        );
        let head = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(10.))
                            .child(div().text_size(px(20.)).font_weight(bold()).child("鲸喷榜"))
                            .when(self.busy, |d| d.child(hint("刷新中…").text_size(px(11.)))),
                    )
                    .child(
                        hint("Plaid Cloud 用户谁喷得最多、最快。说得越多，鲸越大。")
                            .text_size(px(12.)),
                    ),
            )
            .child(seg);
        let page = page.child(head);
        if !self.model.read(cx).facts.cloud_token {
            return page.child(self.notice(
                "登录 Plaid Cloud 后上榜",
                "鲸喷榜只统计 Plaid Cloud 的听写。在 设置 › 服务 › Cloud 用邀请码登录。".into(),
                None,
            ));
        }
        let b = match &self.data {
            None => return page.child(self.notice("正在潜水取数…", String::new(), None)),
            Some(Err(e)) => {
                let this = cx.entity().downgrade();
                return page.child(self.notice(
                    "榜单没取到",
                    e.clone(),
                    Some(small_button("board-retry", "重试", move |_, _, cx| {
                        let _ = this.update(cx, |b, cx| b.fetch(cx));
                    })),
                ));
            }
            Some(Ok(b)) => b.clone(),
        };
        if !b.joined {
            let this = cx.entity().downgrade();
            return page.child(self.notice(
                "你已隐身",
                "隐身期间不在榜上，也看不到别人。听写照常统计，回来时直接上榜。".into(),
                Some(default_button(
                    "board-join",
                    "重新上榜",
                    move |_, _, cx| {
                        let _ = this.update(cx, |b, cx| b.join(true, cx));
                    },
                )),
            ));
        }
        let floor = speed_floor_ms(days);
        let volume: Vec<_> = b
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                (
                    r,
                    r.chars as f64,
                    thousands(r.chars),
                    Some(volume_title(i + 1)),
                )
            })
            .collect();
        let mut fast: Vec<&BoardRow> = b.rows.iter().filter(|r| r.audio_ms >= floor).collect();
        fast.sort_by(|x, y| speed(y).total_cmp(&speed(x)));
        let fast: Vec<_> = fast
            .into_iter()
            .enumerate()
            .map(|(i, r)| (r, speed(r), format!("{:.0}", speed(r)), speed_title(i + 1)))
            .collect();
        let divers: Vec<&str> = b
            .rows
            .iter()
            .filter(|r| r.audio_ms < floor)
            .map(|r| r.name.as_str())
            .collect();
        let floor_min = floor / 60_000;
        let this = cx.entity().downgrade();
        page.child(self.hero(&b, days))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_start()
                    .gap(px(14.))
                    .child(Self::board(
                        "board-volume",
                        "喷量榜",
                        "字".into(),
                        volume,
                        &b.me,
                        "还没有人开喷".into(),
                        CYAN,
                    ))
                    .child(Self::board(
                        "board-speed",
                        "喷速榜",
                        format!("字/分 · 说满 {floor_min} 分钟入榜"),
                        fast,
                        &b.me,
                        format!("还没有人说满 {floor_min} 分钟"),
                        VIOLET,
                    )),
            )
            .when(!divers.is_empty(), |d| {
                d.child(
                    hint(format!("潜水区（不足 {floor_min} 分钟，暂不排喷速）：{}", divers.join("、")))
                        .text_size(px(11.)),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap(px(16.))
                    .child(hint(
                        "只统计 Plaid Cloud 的听写，从上线起算；服务器只记字数和时长，不存文字。名字是邀请名。",
                    ).text_size(px(11.)))
                    .child(small_button("board-leave", "隐身", move |_, _, cx| {
                        let _ = this.update(cx, |b, cx| b.join(false, cx));
                    })),
            )
    }
}

#[cfg(feature = "snapshot")]
fn fixture(days: i64, joined: bool) -> Data {
    let k = if days > 7 { 4 } else { 1 };
    let row = |name: &str, chars: i64, min: f64, n: i64| BoardRow {
        name: name.into(),
        chars: chars * k,
        audio_ms: (min * 60_000.) as i64 * k,
        dictations: n * k,
    };
    Data {
        days,
        me: "neo".into(),
        joined,
        rows: if joined {
            vec![
                row("amy", 48_210, 196., 412),
                row("neo", 36_904, 118., 355),
                row("老王", 21_377, 101., 160),
                row("kiki", 9_820, 52., 97),
                row("小鱼", 2_416, 6.5, 31),
                row("bob", 380, 1.2, 4),
            ]
        } else {
            vec![]
        },
    }
}

#[cfg(feature = "snapshot")]
pub(super) fn check(ui: &mut super::snapshot::Ui) -> Result<(), String> {
    use super::snapshot::expect;
    let facts = super::Facts {
        cloud_token: true,
        ..super::overview::fixture_facts()
    };
    ui.open(super::Page::Board, facts)?;
    expect(
        "鲸喷榜 tab shown for Cloud users; 6 by volume, 4 past the 10-minute speed floor",
        ui.has("board")
            && ui.has("board-me")
            && ui.has("board-volume-5")
            && !ui.has("board-volume-6")
            && ui.has("board-speed-3")
            && !ui.has("board-speed-4"),
        &None,
    )?;
    ui.shot("board")?;
    ui.click("board-range-1")?;
    expect(
        "30 days: boardDays stored, 小鱼 still under the 30-minute speed floor",
        config::value("boardDays").as_deref() == Some("30")
            && ui.has("board-speed-3")
            && !ui.has("board-speed-4"),
        &config::value("boardDays"),
    )?;
    ui.shot("board-30")?;
    ui.click("board-leave")?;
    expect(
        "隐身 hides the board",
        ui.has("board-join") && !ui.has("board-me") && !ui.has("board-volume-0"),
        &None,
    )?;
    ui.shot("board-hidden")?;
    ui.click("board-join")?;
    expect("重新上榜 brings it back", ui.has("board-me"), &None)?;
    ui.close()?;
    let facts = super::overview::fixture_facts();
    ui.open(super::Page::Overview, facts)?;
    expect("no 鲸喷榜 tab without Cloud", !ui.has("board"), &None)?;
    ui.close()
}
