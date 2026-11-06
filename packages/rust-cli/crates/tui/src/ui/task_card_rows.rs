//! 任务卡的行（docs/specs/rust-cli/tui-tasks.md §3.2、§3.3）：种类一行、空一行、这个种类的行，与槽。画法照 Puffin 工作流
//! 的卡：种类用 line 色的 `  ·  ` 隔开，选中的 brand 粗体，其余 fg2、数 fg3，空的 quiet；Subagents 是子代理的树
//! （`agents.rs`），Commands 是在跑的命令与展开在轨上的输出，Todos 是整个清单（kit `todo_step`）。
use rust_cli_kernel_api::panels::ShellOutput;
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::fit::{MEASURE_MARGIN, MEASURE_MAX};
use rust_cli_tui_kit::app::widgets::{lists, todo_step};
use rust_cli_tui_kit::markdown::render::INDENT;
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};

use super::app::App;
use super::background::{ShellView, plain_output};
use super::i18n::Lang;
use super::i18n_panels as words;
use super::lines::truncate;
use super::task_card::{KINDS, TaskCard, TaskKind};

/// 种类一行下面的行至多这么多（Subagents 照它自己的 7 行窗口）。
pub const WINDOW: usize = 16;

/// 展开的输出至多这么多行。
const PEEK_LINES: usize = 5;

/// 卡排好的行、每个可点的行（行，种类里的下标）与槽。
pub struct CardLayer {
    pub lines: Vec<StyledLine>,
    pub targets: Vec<(usize, usize)>,
    pub slot: Vec<Span>,
}

fn pick(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

fn kind_name(kind: TaskKind, lang: Lang) -> &'static str {
    match kind {
        TaskKind::Subagents => pick(lang, "Subagents", "子代理"),
        TaskKind::Commands => pick(lang, "Commands", "命令"),
        TaskKind::Todos => pick(lang, "Todos", "待办"),
    }
}

fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(|s| string_width(&s.text)).sum()
}

/// 窗口：`items` 的第 `start` 行起至多 `max` 行；被藏起来的行用 fg3 的 `↑ N more` / `↓ N more` 代替窗口最上或最下的一行。
fn window(
    app: &App,
    items: Vec<(StyledLine, Option<usize>)>,
    start: usize,
    max: usize,
) -> Vec<(StyledLine, Option<usize>)> {
    let len = items.len();
    if len <= max {
        return items;
    }
    let start = start.min(len - max);
    let end = start + max;
    let theme = &app.ink.theme;
    let more = |text: String| {
        let row = lists::row(vec![Span::plain("  "), Span::new(text, theme.fg3)]);
        (
            Layout::lay(&row, usize::MAX / 4)
                .into_iter()
                .next()
                .unwrap_or_default(),
            None,
        )
    };
    let mut shown: Vec<_> = items.into_iter().skip(start).take(max).collect();
    if start > 0 {
        shown[0] = more(words::more_above(app.lang, start + 1));
    }
    if end < len {
        let last = shown.len() - 1;
        shown[last] = more(words::more(app.lang, len - end + 1).trim_start().to_owned());
    }
    shown
}

impl App {
    /// 卡的一层（`pane.rs`）；卡没开着时没有。
    pub fn tasks_layer(&self, width: usize) -> Option<CardLayer> {
        let card = self.task_card()?;
        let mut lines = self.kinds_row(card.kind, width);
        lines.push(StyledLine::default());
        let mut targets = Vec::new();
        let body: Vec<(StyledLine, Option<usize>)> = match card.kind {
            TaskKind::Subagents => {
                let (rows, items) = self.subagent_rows(self.agent_row(card), width);
                rows.into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let item = items.iter().find(|(row, _)| *row == i).map(|(_, x)| *x);
                        (line, item)
                    })
                    .collect()
            }
            TaskKind::Commands => {
                let items = self.command_rows(card, width);
                let focus = items
                    .iter()
                    .position(|(_, i)| *i == Some(card.command))
                    .unwrap_or(0);
                window(self, items, focus.saturating_sub(WINDOW / 2), WINDOW)
            }
            TaskKind::Todos => {
                let items = self.todo_rows(width);
                window(self, items, card.todo_top, WINDOW)
            }
        };
        for (line, item) in body {
            if let Some(item) = item {
                targets.push((lines.len(), item));
            }
            lines.push(line);
        }
        Some(CardLayer {
            lines,
            targets,
            slot: self.tasks_slot(card),
        })
    }

    /// 种类一行（§3.2）：第 3 列起，选中的 brand 粗体与数，其余 fg2 与 fg3 的数，空的 quiet；Todos 选中时右沿是 `3 of 8 done`。
    /// 放不下时 `  ·  ` 换成两个空格，再放不下去掉数。
    fn kinds_row(&self, selected: TaskKind, width: usize) -> Vec<StyledLine> {
        let theme = &self.ink.theme;
        let counts = self.task_counts();
        let tasks = self.view.tasks();
        let done = tasks
            .iter()
            .filter(|t| t.status == rust_cli_kernel_api::agents::TaskStatus::Completed)
            .count();
        let count = |kind: TaskKind| match kind {
            TaskKind::Subagents => (counts.running > 0).then(|| counts.running.to_string()),
            TaskKind::Commands => Some(counts.commands().to_string()),
            TaskKind::Todos => Some(format!("{done}/{}", tasks.len())),
        };
        let build = |glue: &str, with_counts: bool| {
            let mut spans = vec![Span::plain(INDENT)];
            for (i, kind) in KINDS.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::new(glue.to_owned(), theme.line));
                }
                let name = kind_name(*kind, self.lang);
                if !self.kind_has(*kind) {
                    spans.push(Span::new(name, theme.quiet));
                    continue;
                }
                let on = *kind == selected;
                let brand = Style::fg(theme.brand);
                spans.push(Span::new(
                    name,
                    if on {
                        brand.with_attr(attr::BOLD)
                    } else {
                        theme.fg2
                    },
                ));
                if with_counts && let Some(n) = count(*kind) {
                    spans.push(Span::new(
                        format!(" {n}"),
                        if on { brand } else { theme.fg3 },
                    ));
                }
                if *kind == TaskKind::Subagents && counts.waiting > 0 {
                    spans.push(Span::new(" ●", Style::fg(theme.code)));
                }
            }
            spans
        };
        let edge = width.saturating_sub(1);
        let right = (selected == TaskKind::Todos).then(|| {
            let text = match self.lang {
                Lang::En => format!("{done} of {} done", tasks.len()),
                Lang::Zh => format!("{done}/{} 完成", tasks.len()),
            };
            Span::new(text, theme.fg3)
        });
        let mut row = [("  ·  ", true), ("  ", true), ("  ", false)]
            .into_iter()
            .map(|(glue, with_counts)| build(glue, with_counts))
            .find(|spans| width_of(spans) <= edge)
            .unwrap_or_else(|| build("  ", false));
        if let Some(right) = right {
            let used = width_of(&row) + string_width(&right.text);
            if used + 2 <= edge {
                row.push(Span::plain(" ".repeat(edge - used)));
                row.push(right);
            }
        }
        vec![StyledLine::new(row)]
    }

    /// Commands 的行：每个在跑的命令一行，选中的那个展开着输出时下面是它的输出（§3.2）。
    fn command_rows(&self, card: &TaskCard, width: usize) -> Vec<(StyledLine, Option<usize>)> {
        let theme = &self.ink.theme;
        let measure = width
            .saturating_sub(MEASURE_MARGIN)
            .min(MEASURE_MAX)
            .saturating_sub(string_width(INDENT));
        let mut out = Vec::new();
        for (index, shell) in self.shell_rows().into_iter().enumerate() {
            let mut tags = Vec::new();
            if let Some(machine) = shell.machine.remote() {
                tags.push(Span::plain(" "));
                tags.push(super::machines::tag(&self.ink, machine));
            }
            if shell.reconnecting {
                tags.push(Span::new(
                    format!(" · {}", super::i18n_machines::reconnecting(self.lang)),
                    theme.fg3,
                ));
            }
            let first = shell.description.lines().next().unwrap_or_default();
            let room = measure.saturating_sub(2 + width_of(&tags));
            let label = truncate(&plain_output(first), room);
            let style = if index == card.command {
                self.ink.selection()
            } else {
                theme.fg2
            };
            let mut spans = vec![
                Span::new("○", theme.fg3),
                Span::plain(" "),
                Span::new(label, style),
            ];
            spans.extend(tags);
            out.extend(lay(lists::row(spans), width, Some(index)));
            if let Some(peek) = card.peek.as_ref().filter(|p| p.id == shell.id) {
                out.extend(self.peek_rows(peek, measure, width));
            }
        }
        out
    }

    /// 展开的输出：第 5 列起 line 色的 `│` 与一格，输出的最后至多 5 行（fg3），最后一行是 quiet 的说明（§3.2）。
    fn peek_rows(
        &self,
        peek: &ShellView,
        measure: usize,
        width: usize,
    ) -> Vec<(StyledLine, Option<usize>)> {
        let theme = &self.ink.theme;
        let lang = self.lang;
        let rail = |text: String, style: Style| {
            let text = truncate(&text, measure.saturating_sub(4));
            lists::row(vec![
                Span::plain("  "),
                Span::new("│", theme.line),
                Span::plain(" "),
                Span::new(text, style),
            ])
        };
        let (shown, note): (Vec<String>, String) = match peek.last.as_ref().map(|l| &l.output) {
            None => (Vec::new(), self.strings.loading.to_owned()),
            Some(ShellOutput::Tail { lines, .. }) if lines.is_empty() => (
                Vec::new(),
                pick(lang, "No output yet", "还没有输出").to_owned(),
            ),
            Some(ShellOutput::Tail { lines, total }) => {
                let tail: Vec<String> = lines
                    .iter()
                    .skip(lines.len().saturating_sub(PEEK_LINES))
                    .map(|l| plain_output(l))
                    .collect();
                let n = tail.len();
                let total = (*total as usize).max(n);
                let note = match lang {
                    Lang::En if total <= n => format!("{n} line{}", if n == 1 { "" } else { "s" }),
                    Lang::En => format!("showing {n} of {total} lines"),
                    Lang::Zh if total <= n => format!("{n} 行"),
                    Lang::Zh => format!("显示 {total} 行中的 {n} 行"),
                };
                (tail, note)
            }
            Some(ShellOutput::TooLarge { .. }) => (
                Vec::new(),
                pick(lang, "Output too large to preview", "输出太大，不预览").to_owned(),
            ),
            Some(ShellOutput::Unavailable) => (
                Vec::new(),
                pick(lang, "Output unavailable", "读不到输出").to_owned(),
            ),
        };
        // 在跑时说明前面先写用时（快照没有命令的开始时刻，用时只在读到的详情里）。
        let elapsed = peek
            .last
            .as_ref()
            .filter(|_| !peek.gone)
            .and_then(|l| l.elapsed_ms)
            .map(|ms| format!("{} · ", super::cells::duration(ms)))
            .unwrap_or_default();
        let mut out = Vec::new();
        for text in shown {
            out.extend(lay(rail(text, theme.fg3), width, None));
        }
        out.extend(lay(
            rail(format!("{elapsed}{note}"), theme.quiet),
            width,
            None,
        ));
        out
    }

    /// Todos 的行：整个清单，每步一行（kit `todo_step`，同以前的坞）；没有选中的行。
    fn todo_rows(&self, width: usize) -> Vec<(StyledLine, Option<usize>)> {
        let theme = &self.ink.theme;
        super::tasks::steps(self.view.tasks(), self.lang)
            .iter()
            .flat_map(|(status, text)| {
                let spans = todo_step::step(theme, todo_step::status(status), text, false);
                lay(lists::row(spans), width, None)
            })
            .collect()
    }

    /// 槽（§3.3）：两个以上有东西的种类时先 `←→ kind`；然后 Subagents 能查看时 `⏎ view`、Commands `⏎ output` /
    /// `⏎ close output`。至多两对；`^X stop` 在 `?` 按键表里。
    fn tasks_slot(&self, card: &TaskCard) -> Vec<Span> {
        let lang = self.lang;
        let mut pairs: Vec<(&str, &str)> = Vec::new();
        if KINDS.iter().filter(|k| self.kind_has(**k)).count() > 1 {
            pairs.push(("←→", pick(lang, "kind", "种类")));
        }
        match card.kind {
            TaskKind::Subagents => {
                // 选中的不是正在看的那一个（main 也是）：⏎ 才有事做。
                let viewing = self.viewing.as_ref().map(|v| v.agent.clone());
                let viewable = viewing != self.selected_agent(card);
                if viewable {
                    pairs.push(("⏎", pick(lang, "view", "查看")));
                }
            }
            TaskKind::Commands => {
                let open = card.peek.as_ref().is_some_and(|p| {
                    self.shell_rows()
                        .get(card.command)
                        .is_some_and(|s| s.id == p.id)
                });
                pairs.push((
                    "⏎",
                    if open {
                        pick(lang, "close output", "收起输出")
                    } else {
                        pick(lang, "output", "输出")
                    },
                ));
            }
            TaskKind::Todos => {}
        }
        super::interaction_view::board::slot(&self.ink.theme, &pairs)
    }
}

/// 一个元素排成的行，每行带同一个目标（只有第一行可点）。
fn lay(
    row: rust_cli_tui_kit::app::element::Element,
    width: usize,
    item: Option<usize>,
) -> Vec<(StyledLine, Option<usize>)> {
    Layout::lay(&row, width)
        .into_iter()
        .enumerate()
        .map(|(i, line)| (line, if i == 0 { item } else { None }))
        .collect()
}
