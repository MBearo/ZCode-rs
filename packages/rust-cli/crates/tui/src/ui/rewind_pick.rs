//! 挑选回退的点（docs/puffin-tui.md §4.2「回退（/rewind）」；Puffin `shell/rewind.rs` 的 `RewindUi`、`pick_at`、
//! `rewind_key`、`go_back`、`rewound`、`rewound_into`、`rewind_rows`、`rewind_slot`、`chevron`；画法在 kit 的
//! `rewind_rows`）。
//!
//! 挑选是 TUI 唯一的一份本地状态：点、各点改过的草稿、⏎ 之后的阶段。看到的一切都来自视图与点：切线插在点的你的那块
//! 之前，它和下面的全部变暗（`transcript_lines.rs`）；视图停在切线上方留 [`ABOVE`] 行（`draw.rs`）；输入框的 `↶`、
//! 占位与 Also / Files 两行（`input_box.rs`、`pane.rs`）；槽（`footer.rs`）。
//!
//! 点来自 `/rewind` 的点（tui-pages 的列表，`app.points`）：这里经 [`kit_points`]、[`point_text`]、[`point_row`] 读它们。
use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::entries::Compaction;
use rust_cli_tui_kit::app::widgets::rewind_rows::{self, Point, PointFrom};
use rust_cli_tui_kit::render::StyledLine;

use super::app::{Action, App};
use super::i18n::Lang;
use crate::view::Body;

pub use rust_cli_tui_kit::app::widgets::rewind_rows::ABOVE;

/// 这里的字：英文是 Puffin 的原文，中文是译文。
fn say(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// agent 没有开出新会话时输入框上方安静的一句（Puffin `REWIND_REFUSED`）。
fn refused(lang: Lang) -> &'static str {
    say(
        lang,
        "Couldn't go back: the agent did not start the new session. Nothing was changed.",
        "没能回去：agent 没有开出新会话。什么都没有改。",
    )
}

/// 挑选时草稿为空的占位（Puffin `composer`）。
pub fn placeholder(lang: Lang) -> &'static str {
    say(lang, "Nothing will be sent", "什么都不会发出")
}

/// ⏎ 之后走到哪一步（Puffin `RewindPhase`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Picking,
    /// ⏎ 请内核 fork：除了退出的键每个键都停着，屏幕不变。
    Forking,
    /// fork 出了会话 `id`：等它的视图（第一帧）。
    Opening(String),
}

/// 挑选回退的点：选中的点（点的序号）、每个点上改过的草稿（esc 或 ⏎ 之后作废）、阶段；`said` 是到头时槽里说的一句，到
/// 下一个键为止（没有计时器）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Picking {
    pub point: usize,
    pub drafts: BTreeMap<usize, String>,
    pub phase: Phase,
    pub said: Option<&'static str>,
    /// 视图停在切线上（换点时回到这里）；滚动过之后停在滚到的地方，滚到底停在最后一屏，^End 回到切线。
    pub at_cut: bool,
    /// 离开的会话的标题（⏎ 时记下，痕迹行说出它，Puffin `titles`）；没有标题时 None。
    pub left: Option<String>,
}

// ---- 点（`/rewind` 的点列表）----

/// 点在 kit 里的样子（Puffin host 的 `points`）：上面有回答时在它上面 fork，第一条没有时回到开头，其余回不去；`at` 是你的那块
/// 在视图里出现的时刻（没有那一行时为 0）。
pub fn kit_points(app: &App) -> Vec<Point> {
    app.rewind
        .iter()
        .enumerate()
        .map(|(k, entry)| {
            let (added, removed) = entry
                .option
                .files
                .iter()
                .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
            let at = point_row(app, k)
                .and_then(|id| app.view.rows().iter().find(|row| row.id == id))
                .map_or(0, |row| row.created_ms);
            Point {
                at,
                from: match (&entry.answer, k) {
                    (Some(_), _) => PointFrom::Answer,
                    (None, 0) => PointFrom::Start,
                    (None, _) => PointFrom::None,
                },
                added,
                removed,
            }
        })
        .collect()
}

/// 点 `k` 的消息原文（草稿从它开始）。
fn point_text(app: &App, k: usize) -> String {
    app.rewind
        .get(k)
        .map(|entry| entry.text.clone())
        .unwrap_or_default()
}

/// 点 `k` 的消息。
fn point_message(app: &App, k: usize) -> Option<MessageId> {
    app.rewind.get(k).map(|entry| entry.option.message.clone())
}

/// 点 `k` 的你的那块在视图里的行 id。
pub fn point_row(app: &App, k: usize) -> Option<u64> {
    app.view
        .rows()
        .iter()
        .filter(|row| matches!(row.body, Body::User { .. }))
        .nth(k)
        .map(|row| row.id)
}

/// 视图行 `row` 是哪个点的你的那块。
pub fn point_at_row(app: &App, row: u64) -> Option<usize> {
    (0..app.rewind.len()).find(|k| point_row(app, *k) == Some(row))
}

// ---- 画法 ----

/// 挑选时切线在哪一块之前（点的行 id）与宽 `width` 的切线。
pub fn cut(app: &App, width: usize) -> Option<(u64, StyledLine)> {
    let k = app.picking_point()?;
    let points = kit_points(app);
    let row = point_row(app, k)?;
    let words = super::i18n_kit::rewind(app.lang);
    Some((
        row,
        rewind_rows::cut_line_said(&app.ink.theme, &points, k, width, words),
    ))
}

/// 挑选时变暗的画法（kit `dimmer`）。
pub fn dimmer(app: &App) -> impl Fn(StyledLine) -> StyledLine + Send + Sync + 'static {
    rewind_rows::dimmer(&app.ink.theme)
}

/// 点 `k` 下面随之回去或留下的：第一次目标检查的轮（目标回到那时），完成的压缩（留在后面）（Puffin `words::also`）。
fn also(app: &App, k: usize) -> Vec<String> {
    let rows = app.view.rows();
    let Some(at) = point_row(app, k).and_then(|id| rows.iter().position(|row| row.id == id)) else {
        return Vec::new();
    };
    let below = &rows[at + 1..];
    let first_round = below
        .iter()
        .filter_map(|row| match &row.body {
            Body::Goal(goal) => Some(goal.iterations.unwrap_or(1)),
            _ => None,
        })
        .min();
    let zh = app.lang == Lang::Zh;
    let goal = first_round.map(|r| match (r <= 1, zh) {
        (true, false) => "no goal: it was set below".to_owned(),
        (true, true) => "没有目标：它是在下面设的".to_owned(),
        (false, false) => format!("the goal as it was at round {r}"),
        (false, true) => format!("目标回到第 {r} 轮时的样子"),
    });
    let compact = below
        .iter()
        .any(|row| matches!(&row.body, Body::Compact(c) if c.status == Compaction::Success))
        .then(|| {
            say(
                app.lang,
                "the turns above come along in full: the compaction stays behind",
                "上面的回合完整地带过去：压缩留在后面",
            )
            .to_owned()
        });
    goal.into_iter().chain(compact).collect()
}

/// 挑选时输入框里输入行上面的两行：Also 与 Files（回不去的点没有）。
pub fn rows(app: &App) -> Vec<Element> {
    match app.picking_point() {
        Some(k) => rewind_rows::rows_said(
            &app.ink.theme,
            &kit_points(app),
            k,
            &also(app, k),
            super::i18n_kit::rewind(app.lang),
        ),
        None => Vec::new(),
    }
}

/// 挑选时的槽（Puffin `rewind_slot`）：到头时说的一句；草稿为空、点回得去时 `⏎ go back only`；否则 ↑ ↓ 去哪里。
/// `None`：不在挑选。
pub enum Slot {
    Says(&'static str),
    Keys(Vec<(&'static str, &'static str)>),
}

pub fn slot(app: &App) -> Option<Slot> {
    let k = app.picking_point()?;
    if let Some(says) = app.picking.as_ref().and_then(|p| p.said) {
        return Some(Slot::Says(says));
    }
    let points = kit_points(app);
    let (earlier, later) = (k > 0, k + 1 < points.len());
    let lang = app.lang;
    let keys = if app.composer.is_empty() && points[k].from != PointFrom::None {
        vec![("⏎", say(lang, "go back only", "只回去"))]
    } else if earlier && later {
        vec![("↑↓", say(lang, "point", "换点"))]
    } else if earlier {
        vec![("↑", say(lang, "earlier", "更早"))]
    } else if later {
        vec![("↓", say(lang, "later", "更晚"))]
    } else {
        Vec::new()
    };
    Some(Slot::Keys(keys))
}

// ---- 状态 ----

impl App {
    /// 在挑的点，在有的点之内。
    pub fn picking_point(&self) -> Option<usize> {
        let n = self.rewind.len();
        self.picking
            .as_ref()
            .filter(|_| n > 0)
            .map(|p| p.point.min(n - 1))
    }

    /// 挑点 `k`：它的草稿是在那里留下的，否则是它的消息；视图回到切线（Puffin `pick_at`）。列表的 ⏎、esc 连按两次、
    /// ↑ ↓ 与点击都从这里进。
    pub fn pick_point(&mut self, k: usize) {
        let mut picking = match self.picking.take() {
            Some(mut p) => {
                p.drafts
                    .insert(p.point, self.composer.area.text().to_owned());
                p
            }
            None => Picking::default(),
        };
        let draft = picking
            .drafts
            .get(&k)
            .cloned()
            .unwrap_or_else(|| point_text(self, k));
        picking.point = k;
        picking.said = None;
        picking.at_cut = true;
        self.picking = Some(picking);
        self.composer.set_text(&draft);
        self.port.scroll = 0;
        self.pointer.selection = None;
        self.notice = None;
    }

    /// esc 离开挑选：什么都没发生，输入框是空的（Puffin `unpicked`）。
    fn unpick(&mut self) {
        self.picking = None;
        self.composer.clear();
        self.port.scroll = 0;
        self.pointer.selection = None;
    }

    /// 挑选拿的键；`None` 把键传下去（打字编辑草稿）。⏎ 之后（fork 中、等新会话）除了退出的键都停着。
    pub(super) fn picking_key(&mut self, key: &KeyEvent) -> Option<Option<Action>> {
        let k = self.picking_point()?;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let phase = self.picking.as_ref().map(|p| p.phase.clone())?;
        if phase != Phase::Picking {
            let quits = ctrl && matches!(key.code, KeyCode::Char('c' | 'd' | 'q'));
            return (!quits).then_some(None);
        }
        if let Some(p) = self.picking.as_mut() {
            p.said = None;
        }
        let n = self.rewind.len();
        let width = self.composer_width;
        let up = match key.code {
            KeyCode::Up => Some(true),
            KeyCode::Down => Some(false),
            KeyCode::Char('p') if ctrl => Some(true),
            KeyCode::Char('n') if ctrl => Some(false),
            _ => None,
        };
        match (key.code, up) {
            (KeyCode::Esc, _) => self.unpick(),
            (KeyCode::Enter, _) if key.modifiers.is_empty() => return Some(self.go_back(k)),
            // ↑ 在草稿的第一个屏幕行、↓ 在最后一个换点，别的行照常移动光标；到头了槽里说一句。
            (_, Some(up)) => {
                let edge = if up {
                    self.composer.area.at_first_row(width)
                } else {
                    self.composer.area.at_last_row(width)
                };
                if !edge {
                    self.composer.area.vertical(width, !up);
                } else if up && k > 0 {
                    self.pick_point(k - 1);
                } else if !up && k + 1 < n {
                    self.pick_point(k + 1);
                } else {
                    let said = if up {
                        say(
                            self.lang,
                            "This is your first message.",
                            "这是你的第一条消息。",
                        )
                    } else {
                        say(
                            self.lang,
                            "This is your last message.",
                            "这是你的最后一条消息。",
                        )
                    };
                    if let Some(p) = self.picking.as_mut() {
                        p.said = Some(said);
                    }
                }
            }
            _ => return None,
        }
        Some(None)
    }

    /// 挑选时点在 transcript 的行 `row`（块的键）上：是一个点的你的那块时换到那个点（Puffin `rewind_key` 的点击）。
    pub(super) fn pick_at_row(&mut self, row: u64) -> bool {
        let at = self
            .picking
            .as_ref()
            .filter(|p| p.phase == Phase::Picking)
            .and_then(|_| point_at_row(self, row));
        match at {
            Some(k) => {
                self.pick_point(k);
                true
            }
            None => false,
        }
    }

    /// 挑选时的 ⏎（Puffin `go_back`）：第一个点像 `/new` 一样回到开头，草稿不为空时作为第一条消息发出；别的能回去的点在
    /// 它上面的回答上 fork；回不去的点什么也不做。
    fn go_back(&mut self, k: usize) -> Option<Action> {
        let point = *kit_points(self).get(k)?;
        match point.from {
            PointFrom::None => None,
            PointFrom::Start => {
                self.picking = None;
                if self.composer.area.text().trim().is_empty() {
                    self.composer.clear();
                    return Some(Action::Clear);
                }
                // 草稿先取出（发送清空输入框），`/new` 之后它作为新会话的第一条消息发出。
                let send = self.submit();
                Some(match send {
                    Some(send) => Action::Many(vec![Action::Clear, send]),
                    None => Action::Clear,
                })
            }
            PointFrom::Answer => {
                let message = point_message(self, k)?;
                let title = self.view.title().trim().to_owned();
                if let Some(p) = self.picking.as_mut() {
                    p.phase = Phase::Forking;
                    p.left = (!title.is_empty()).then_some(title);
                }
                self.said.quiet = None;
                Some(Action::RewindFork { message })
            }
        }
    }

    /// 内核对 fork 的回答（Puffin `rewound`）：开出了新会话就等它的视图；没有时回到挑选，草稿还在，输入框上方说一句。
    pub fn rewound(&mut self, result: Result<String, String>) {
        let Some(p) = self.picking.as_mut().filter(|p| p.phase == Phase::Forking) else {
            return;
        };
        match result {
            Ok(id) => p.phase = Phase::Opening(id),
            Err(reason) => {
                tracing::warn!(%reason, "rewind: the agent did not fork");
                p.phase = Phase::Picking;
                self.said.quiet = Some(refused(self.lang).to_owned());
            }
        }
    }

    /// fork 出的会话的视图到了（Puffin `rewound_into`）：挑选结束；视图第一行的痕迹行（`Body::Forked`，来自会话的
    /// `meta.parent`）说出离开的会话的标题（Puffin `titles`）；草稿像任何消息一样作为它的第一条消息发出。
    pub fn rewound_into(&mut self) -> Option<Action> {
        let picking = self
            .picking
            .take_if(|p| matches!(p.phase, Phase::Opening(_)))?;
        self.view.name_fork(picking.left);
        self.said.quiet = None;
        if self.composer.area.text().trim().is_empty() {
            self.composer.clear();
            return None;
        }
        self.submit()
    }
}

#[cfg(test)]
#[path = "rewind_pick_tests.rs"]
mod tests;
