//! 请求在正文里（docs/puffin-tui.md §4.2「请求」的「预览」；Puffin `shell/view/tail.rs` 的 `asked` 与 `own`、
//! `shell/asks.rs` 的 `ask_preview` 与 `ask_mark`；docs/specs/rust-cli/tui-kit.md §4.2「权限询问、提问、计划审批」）：
//!
//! - 问的那一行的 `●` 从 line 色淡入到 amber（[`Marks`]，cells 画行首的记号时查它）；
//! - 在回答的权限请求的预览挂在它的行下面（[`previews`]，`transcript_lines::lines` 放进正文），不截断：命令与说明、改动的
//!   行、新文件的全文、地址、字段。只有权限请求把行标成在问（Puffin 同）：有行的提问与计划在正文里没有预览；
//! - 没有行的请求的预览：提问是问题与选中的回答的预览，计划是计划的全文；
//! - 这个会话里没有行的请求（子代理的、旧会话的）画自己的一行 `● 名字  摘要`：在启动它的 Agent 行下面缩进 2 列，找不到时
//!   在正文末尾；
//! - 旧界面多出的（remote-exec.md §21.6，照预览的轨放）：在远端机器上执行的权限请求（输入的 `machine`，或 MCP 服务器绑定
//!   的机器）在轨上写 fg3 的 `runs on` 与强调色的机器名，在「谁在问」之后。
//!
//! 以前预览在板的上面、截到 12 行，问的行下面写 `Waiting for permission…`。
use std::time::Instant;

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_tui_kit::app::element::{Element, br, single_line};
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::asks::data::{Ask, AskBody};
use rust_cli_tui_kit::app::widgets::asks::{self, QuestionUi, preview as kit_preview};
use rust_cli_tui_kit::app::widgets::fade;
use rust_cli_tui_kit::markdown::document::Document;
use rust_cli_tui_kit::markdown::render::{self as markdown, INDENT};
use rust_cli_tui_kit::render::{Span, Style, StyledLine};

use super::app::App;
use super::i18n_asks as words;
use super::interaction::ticks_since;
use super::interaction_view::open_asks;

/// 预览挂在哪：`Row` 是内容里有这个工具调用的块之后；`End` 是最后一个块之后、尾巴之前。
pub enum Anchor {
    Row(ToolUseId),
    End,
}

/// 排好的预览（按屏宽）。没有工具行的请求自己的 `● Name summary` 行也在里面。
pub struct Preview {
    pub anchor: Anchor,
    pub lines: Vec<StyledLine>,
}

/// 等你回答的行的记号的墨，按工具调用（Puffin `ask_mark`）。
#[derive(Default)]
pub struct Marks {
    by_tool: Vec<(ToolUseId, Style)>,
}

impl Marks {
    /// 这一帧开着的请求的行的记号：从第一次出现起 [`fade::TICKS`] 个 tick 内从 line 色淡入到 amber。
    pub fn of(app: &App, now: Instant) -> Marks {
        let by_tool = open_asks(app)
            .into_iter()
            .filter_map(|(id, request)| {
                let ask = app.kit_ask(id, request);
                (!ask.item_id.is_empty() && asks::is_permission(&ask))
                    .then(|| (ToolUseId::new(&ask.item_id), mark(app, &ask.id, now)))
            })
            .collect();
        Marks { by_tool }
    }

    /// 这一行在等你回答时它的记号的墨。
    pub fn mark(&self, tool: &ToolUseId) -> Option<Style> {
        self.by_tool
            .iter()
            .find(|(id, _)| id == tool)
            .map(|(_, style)| *style)
    }
}

/// 请求 `ask` 的行的 `●`：从 line 色淡入到 amber。
fn mark(app: &App, ask: &str, now: Instant) -> Style {
    let theme = &app.ink.theme;
    let p = app
        .asks
        .seen
        .iter()
        .find(|(id, _)| id.as_str() == ask)
        .map_or(1.0, |(_, at)| {
            ticks_since(*at, now) as f64 / fade::TICKS as f64
        });
    asks::fade_in(theme, Style::fg(theme.code), p)
}

impl App {
    /// 请求的回答在点亮、行的记号在淡入、同伴消息的回答在点亮：按动画节拍重画到落定（Puffin 的 tick 一直在走）。修的
    /// 问题：以前只有回合在跑时才定时重画，回合结束后到的请求（后台子代理的、同伴消息）回答一直是 line 色，直到下一次按键。
    pub fn asks_moving(&self, now: Instant) -> bool {
        let arming = self
            .head_ask()
            .is_some_and(|(id, _, ui)| !self.asks.answered.contains(id) && !ui.armed(now));
        let fading = self
            .asks
            .seen
            .values()
            .any(|at| ticks_since(*at, now) < fade::TICKS);
        let peer = self.peer_asking().is_some() && self.peer_armedness(now) < 1.0;
        arming || fading || peer
    }

    /// 一个开着的请求整理成 kit 的形状（在回答的那个带它改过的前缀）。
    pub fn kit_ask(
        &self,
        id: &rust_cli_kernel_api::ids::InteractionId,
        request: &rust_cli_kernel_api::orchestration::InteractionRequest,
    ) -> Ask {
        let seed = self
            .interaction
            .as_ref()
            .filter(|ui| ui.id == *id)
            .and_then(|ui| ui.seed.as_deref());
        super::interaction_ask::ask_of(&self.view, id, request, seed)
    }

    /// 在回答的请求在它的行下面显示的（§4.2「预览」）。`indent`：子代理的请求往里缩的列。
    fn ask_preview(&self, ask: &Ask, row_summary: &str, indent: usize) -> Vec<Element> {
        let theme = &self.ink.theme;
        let Some(ui) = self.interaction.as_ref() else {
            return Vec::new();
        };
        match &ask.body {
            AskBody::Plan { plan } => {
                vec![br(), markdown::answer(&Document::parse(plan), None, theme)]
            }
            AskBody::Question { .. } => {
                let q = QuestionUi {
                    page: ui.page,
                    choice: ui.choice,
                    picks: &ui.picks,
                    own: &ui.own,
                };
                kit_preview::question_preview(theme, ask, &q, self.ask_typed(), indent)
            }
            AskBody::Permission { .. } => {
                let who = ask
                    .origin
                    .as_ref()
                    .map(|o| words::who_asks(self.lang, &o.who, ui.origin.as_deref()));
                let mut out = kit_preview::permission_preview_said(
                    theme,
                    ask,
                    row_summary,
                    indent,
                    who.as_deref(),
                );
                let machine = ask.machine.as_deref().or(ui.bound_machine.remote());
                if let Some(machine) = machine {
                    let (before, after) = super::i18n_machines::runs_on(self.lang);
                    let line = single_line(
                        vec![
                            Span::plain(format!("{INDENT}{}", " ".repeat(indent))),
                            Span::new("│", theme.line),
                            Span::plain(" "),
                            Span::new(before, theme.fg3),
                            Span::new(machine.to_owned(), Style::fg(theme.brand)),
                            Span::new(after, theme.fg3),
                        ],
                        false,
                    );
                    out.insert(usize::from(who.is_some()), line);
                }
                out
            }
        }
    }
}

/// 请求在正文里的块：在回答的那个的预览（挂在它的行下面），与没有行的请求自己的行（在回答的那个带预览）。只读查看子代理
/// 时没有。
pub fn previews(app: &App, width: usize, now: Instant) -> Vec<Preview> {
    if app.viewing.is_some() {
        return Vec::new();
    }
    let theme = &app.ink.theme;
    let head = app.head_ask().map(|(id, _, _)| id.clone());
    let lay = |elements: Vec<Element>| -> Vec<StyledLine> {
        elements
            .iter()
            .flat_map(|e| Layout::lay(e, width))
            .collect()
    };
    let rows_of = |tool: &str| {
        app.view
            .rows()
            .iter()
            .filter_map(|row| row.tool())
            .any(|t| t.id.as_str() == tool)
    };
    let mut out = Vec::new();
    for (id, request) in open_asks(app) {
        let ask = app.kit_ask(id, request);
        let is_head = head.as_ref() == Some(id);
        if !ask.item_id.is_empty() {
            // 只有权限请求把它的行标成在问（Puffin `host/fold/asks.rs` 的 `asked` 只给权限请求 `mark_asking`）：提问与
            // 计划有行时正文里没有预览，它们的字在板上。
            if is_head && asks::is_permission(&ask) {
                let lines = lay(app.ask_preview(&ask, &ask.summary, 0));
                if !lines.is_empty() {
                    out.push(Preview {
                        anchor: Anchor::Row(ToolUseId::new(&ask.item_id)),
                        lines,
                    });
                }
            }
            continue;
        }
        // 没有行的请求：在启动它的 Agent 行下面缩进 2 列，找不到时在正文末尾。
        let parent = ask
            .origin
            .as_ref()
            .map(|o| o.parent_item_id.clone())
            .filter(|p| !p.is_empty() && rows_of(p));
        let indent = if parent.is_some() { 2 } else { 0 };
        let mut rows = vec![kit_preview::own_row(
            theme,
            &ask,
            mark(app, &ask.id, now),
            indent,
        )];
        if is_head {
            rows.extend(app.ask_preview(&ask, &ask.summary, indent));
        }
        out.push(Preview {
            anchor: parent.map_or(Anchor::End, |p| Anchor::Row(ToolUseId::new(&p))),
            lines: lay(rows),
        });
    }
    // 扣住的同伴消息（`peer_view.rs`）：没有请求开着时在正文末尾。
    if let Some(lines) = super::peer_view::preview(app, width) {
        out.push(Preview {
            anchor: Anchor::End,
            lines,
        });
    }
    out
}
