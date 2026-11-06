// 改写自 apps/zcode-host/crates/tui/src/ui/interaction_view.rs（feat/rs 79b6691c55）：输入换成
// `InteractionRequest`；改动预览从工具输入算。
//! 请求在板上的行（docs/specs/rust-cli/tui-kit.md §4.2「权限询问、提问、计划审批」；docs/puffin-tui.md §4.2「请求」
//! 「提问」「计划」，壳的 `shell/asks.rs` 的 `ask_rows`、`ask_placeholder`、`ask_slot`）：板头的问题与右边的
//! `N more waiting`、编号的回答（kit `widgets::asks`），提问是页名与这一页的回答。行在板的顶上，下面是栈与真的输入框
//! （占位是请求的一句，槽是请求的键）；要做的事在正文里问的那一行下面（`ask_body.rs`）。
//! 以前请求替换整个底部区域：预览在板上面、截到 12 行，回答是 Yes / No，有自己的输入行。
use std::time::Instant;

use rust_cli_kernel_api::ids::InteractionId;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::asks::data::{Ask, AskBody, AskOptionKind, AskPreview};
use rust_cli_tui_kit::app::widgets::asks::{self, Answer, QuestionUi};
use rust_cli_tui_kit::render::StyledLine;
use rust_cli_tui_kit::theme::theme::Theme;

use super::app::App;
use super::i18n::Lang;
use super::i18n_asks as words;
use super::ink::Ink;
use super::interaction::Interaction;
use super::interaction_ask::ask_of;

#[path = "interaction_board.rs"]
pub(crate) mod board;

use board::{Laid, lay_all};

/// 视图排版所需的上下文（同伴消息、确认框、后台面板也用）。
pub struct View<'a> {
    /// 墨色（tui-kit.md §3.4）：这几个视图只经它取色。
    pub ink: &'a Ink,
    pub lang: Lang,
    pub width: usize,
}

impl View<'_> {
    pub(super) fn theme(&self) -> &Theme {
        &self.ink.theme
    }
}

/// 选项所在的行：（行号，选项下标）。
pub(super) type Rows = Vec<(usize, usize)>;
/// 终端光标：（行号，列）。
pub(super) type At = Option<(usize, usize)>;

/// 开着的请求：当前会话的（按到达顺序），然后是旧会话的询问。
pub fn open_asks(app: &App) -> Vec<(&InteractionId, &InteractionRequest)> {
    app.view
        .pending_all()
        .iter()
        .map(|(id, request)| (id, request))
        .chain(
            app.detached
                .iter()
                .map(|ask| (&ask.interaction, &ask.request)),
        )
        .collect()
}

/// 路径类的预览的文件（板头写它的名字）。
fn preview_path(ask: &Ask) -> &str {
    match &ask.body {
        AskBody::Permission {
            preview: AskPreview::Diff { path, .. } | AskPreview::File { path, .. },
            ..
        } => path,
        _ => "",
    }
}

impl App {
    /// 在回答的请求（最早的那个，Puffin `head_ask`）与它的界面状态。
    pub fn head_ask(&self) -> Option<(&InteractionId, &InteractionRequest, &Interaction)> {
        let (id, request) = self.pending()?;
        let ui = self.interaction.as_ref().filter(|ui| ui.id == *id)?;
        Some((id, request, ui))
    }

    /// 有请求开着：它的键先拿，命令架、`?`、历史不开（Puffin `asking`）。
    pub fn asking(&self) -> bool {
        self.head_ask().is_some()
    }

    /// 在回答的请求整理成 kit 的形状。
    pub fn head_kit_ask(&self) -> Option<Ask> {
        let (id, request, ui) = self.head_ask()?;
        Some(ask_of(&self.view, id, request, ui.seed.as_deref()))
    }

    /// 输入框里有字，且不是“不再询问”的前缀：⏎ 是 Deny / Keep planning，回答的说明跟着变。
    pub(super) fn ask_typed(&self) -> bool {
        !self.composer.area.text().trim().is_empty()
            && !self.interaction.as_ref().is_some_and(|ui| ui.editing)
    }

    /// 全部允许会回答的请求：与在回答的那个同一工具的主 agent 的权限请求，两个以上、都能允许一次时（§4.2「全部允许」）。
    pub fn allow_all_asks(&self) -> Vec<InteractionId> {
        let Some(head) = self
            .head_kit_ask()
            .filter(|a| asks::is_permission(a) && a.origin.is_none())
        else {
            return Vec::new();
        };
        let same: Vec<Ask> = self
            .view
            .pending_all()
            .iter()
            .map(|(id, request)| ask_of(&self.view, id, request, None))
            .filter(|a| {
                asks::is_permission(a) && a.origin.is_none() && a.tool_name == head.tool_name
            })
            .collect();
        let all_once = same.iter().all(|a| {
            asks::options_of(a)
                .iter()
                .any(|o| o.kind == AskOptionKind::AllowOnce)
        });
        if same.len() >= 2 && all_once {
            same.iter().map(|a| InteractionId::new(&a.id)).collect()
        } else {
            Vec::new()
        }
    }

    /// 请求的回答，按现在的草稿与模式（字照界面语言）。
    pub fn answers_of(&self, ask: &Ask) -> Vec<Answer> {
        let theme = &self.ink.theme;
        let workspace = self
            .interaction
            .as_ref()
            .map_or_else(|| self.workspace().to_owned(), |ui| ui.workspace.clone());
        let mode = self.view.mode();
        let list = asks::answers(
            theme,
            ask,
            &workspace,
            self.ask_typed(),
            self.allow_all_asks().len(),
            Some(words::kit_mode(mode)),
        );
        words::answers(self.lang, theme, list, mode)
    }

    /// 还有几个请求在等（在回答的之外，含旧会话的询问）。
    fn more_waiting(&self) -> usize {
        open_asks(self).len().saturating_sub(1)
    }

    /// 请求开着时板上的行：板头，然后是回答；提问是页名与这一页的回答。
    pub fn ask_rows(&self, now: Instant) -> Vec<Element> {
        let (Some(ask), Some((_, _, ui))) = (self.head_kit_ask(), self.head_ask()) else {
            return Vec::new();
        };
        let theme = &self.ink.theme;
        let typed = self.ask_typed();
        let more = words::more_waiting(self.lang, self.more_waiting());
        let armed = if self.asks.answered.contains(&ui.id) {
            1.0
        } else {
            ui.armedness(now)
        };
        if asks::is_question(&ask) {
            let q = QuestionUi {
                page: ui.page,
                choice: ui.choice,
                picks: &ui.picks,
                own: &ui.own,
            };
            return asks::question_rows_said(
                theme,
                &ask,
                &q,
                typed,
                armed,
                more.as_deref(),
                words::recommended(self.lang),
            );
        }
        let head = words::head(self.lang, &ask, preview_path(&ask));
        let risky = asks::risky(&ask).then(|| words::risky(self.lang));
        let mut rows = vec![asks::head_row_said(theme, &head, risky, more.as_deref())];
        rows.extend(asks::answer_rows(
            theme,
            &self.answers_of(&ask),
            ui.choice,
            armed,
        ));
        rows
    }

    /// 请求开着时输入框的灰字。
    pub fn ask_placeholder(&self) -> Option<&'static str> {
        self.head_kit_ask()
            .map(|ask| words::placeholder(self.lang, &ask))
    }

    /// 请求开着时槽里的键（§4.2「请求」「提问」「计划」，Puffin `ask_slot`）；`below`：预览在视图下面接着。打了字时先说字：
    /// 它们决定 ⏎ 做什么。提问的 AFK 倒计时在 `PgDn read on` 的位置（spec §11.4）。
    pub fn ask_slot(&self, below: bool) -> Vec<(String, String)> {
        let (Some(ask), Some((_, _, ui))) = (self.head_kit_ask(), self.head_ask()) else {
            return Vec::new();
        };
        let lang = self.lang;
        let pair = |k: &str, v: &'static str| (k.to_owned(), words::slot(lang, v).to_owned());
        let stop = pair("esc", "stop");
        let up_to = |n: usize| {
            if n <= 1 {
                "1".to_owned()
            } else {
                format!("1–{}", n.min(9))
            }
        };
        // 旧界面多出的：输入框里是“不再询问”的前缀时 ⏎ 记住它（自拟，tui-kit.md §4.3）。
        if ui.editing && !self.composer.area.text().trim().is_empty() {
            return vec![pair("⏎", "always allow"), stop];
        }
        if self.ask_typed() {
            return if asks::is_plan(&ask) {
                vec![pair("⏎", "keep planning")]
            } else if asks::is_question(&ask) {
                // 提问页上的字是这一页自己的回答：数字与空格都是字。
                vec![stop]
            } else {
                vec![pair("⏎", "deny and send")]
            };
        }
        if let Some(seconds) = ui.countdown.filter(|_| asks::is_question(&ask)) {
            return vec![(String::new(), words::countdown(lang, seconds)), stop];
        }
        if below {
            return vec![pair("PgDn", "read on"), stop];
        }
        if asks::is_question(&ask) {
            let pages = asks::pages_of(&ask);
            let mut keys: Vec<(String, String)> = pages
                .get(ui.page)
                .map(|p| {
                    if p.multi {
                        pair("space", "tick")
                    } else {
                        (up_to(p.options.len()), words::slot(lang, "pick").to_owned())
                    }
                })
                .into_iter()
                .collect();
            keys.push(if pages.len() > 1 {
                pair("←→", "page")
            } else {
                stop
            });
            return keys;
        }
        let answers = self.answers_of(&ask);
        // 选中 Always allow 时槽照旧是 Puffin 的 `1–N answer  esc stop`（团队决定 2026-10-31）：旧界面多出的 ⇥ 改规则
        // 照旧能用，只列在 `?` 面板里（tui-kit.md §4.3）。以前这时槽换成 `⇥ edit rule  esc stop`，与 Puffin 不同。
        vec![
            (up_to(answers.len()), words::slot(lang, "answer").to_owned()),
            stop,
        ]
    }

    /// Bash 询问“不再询问”的前缀（改过的，或询问给的种子；工作线 F72）。
    pub(super) fn ask_seed(&self) -> Option<String> {
        let (_, request, ui) = self.head_ask()?;
        ui.seed
            .clone()
            .or_else(|| super::interaction::remember::seed(request))
    }

    /// 视图还钉在在回答的请求的行上（Puffin `AskUi.pinned`）。
    pub fn ask_pinned(&self) -> bool {
        self.head_ask().is_some_and(|(_, _, ui)| ui.pinned)
    }

    /// 滚动了：放开在回答的请求的钉（Puffin 任何滚动都清掉 `pinned`；`scroll_by` 调它）。
    pub fn unpin_ask(&mut self) {
        if let Some(ui) = self.interaction.as_mut() {
            ui.pinned = false;
        }
    }

    /// 请求开着时板上的一块（`pane.rs` 放上板）：只有板上的行；草稿行照常画（占位与槽是请求的），预览在正文里。返回
    /// 行、回答所在的行（点击选它）与光标（没有：光标在输入框里）。`below`：预览在视图下面接着，槽是 `PgDn read on`。
    pub fn ask_laid(&self, width: usize, now: Instant, below: bool) -> Option<(Laid, Rows, At)> {
        let ask = self.head_kit_ask()?;
        let rows = self.ask_rows(now);
        let lines: Vec<StyledLine> = lay_all(&rows, width);
        // 回答的行：板头之后一行一个；提问的页名之后一行一个。
        let count = if asks::is_question(&ask) {
            let ui = self.interaction.as_ref()?;
            asks::pages_of(&ask)
                .get(ui.page)
                .map_or(0, |p| p.options.len())
        } else {
            self.answers_of(&ask).len()
        };
        let first = lines.len().saturating_sub(count);
        let options = (0..count).map(|i| (first + i, i)).collect();
        let slot = board::slot_pairs(&self.ink.theme, &self.ask_slot(below));
        // 板上的行从 0 起到末尾（`input_at`），没有自己的输入行。
        let laid = Laid {
            input_at: lines.len(),
            lines,
            slot,
            answer: Some(self.ask_placeholder()?.to_owned()),
            ..Laid::default()
        };
        Some((laid, options, None))
    }
}
