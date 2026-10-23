// 改写自 apps/zcode-host/crates/tui/src/ui/interaction_view.rs（feat/rs 79b6691c55）：输入换成
// `InteractionRequest`；改动预览用 transcript 的 diff（从工具输入算）。
//! 审批的排版：强调色的 `─` 线、粗体标题，内容放在两条 dim 虚线之间，然后是原因、问题与选项，最后一行
//! 提示。计划审批与提问的排版见 `interaction_view_plan.rs`、`interaction_view_question.rs`。
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use serde_json::Value;

use super::cells::{self, Ctx};
use super::highlight;
use super::i18n::{Lang, Strings};
use super::interaction::Interaction;
use super::palette::Palette;
use super::text::{truncate_line, wrap, wrap_text};

/// 改动预览与计划最多显示的行数。
pub(super) const PREVIEW: usize = 12;

/// 视图排版所需的上下文。
pub struct View<'a> {
    pub palette: &'a Palette,
    pub strings: &'a Strings,
    pub lang: Lang,
    pub width: usize,
    pub now_ms: u64,
}

pub(super) fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

pub(super) fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// 视图的开头：强调色的线与粗体标题。
pub(super) fn frame(title: &str, view: &View<'_>) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "─".repeat(view.width),
            Style::default().fg(view.palette.accent),
        )),
        truncate_line(
            Line::from(Span::styled(
                format!("  {title}"),
                bold().fg(view.palette.accent),
            )),
            view.width,
        ),
    ]
}

pub(super) fn dashed(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {}", "╌".repeat(width.saturating_sub(4))),
        dim(),
    ))
}

/// 一行选项，按选择列表组件画（`› ` 加底色条，tui.md §7.4，修复线 F26；原来是强调色的 `❯ `）。
pub(super) fn option(
    focused: bool,
    number: usize,
    label: Vec<Span<'static>>,
    view: &View<'_>,
) -> Line<'static> {
    let mut spans = vec![
        Span::raw("  "),
        super::pick::pointer(focused),
        Span::raw(format!("{number}. ")),
    ];
    spans.extend(label);
    super::pick::paint(
        truncate_line(Line::from(spans), view.width),
        focused,
        view.palette,
        view.width,
    )
}

/// 底部的按键提示（`{键}` 模板，按键提示组件画，tui.md §7.4）。
pub(super) fn hint(template: &str, view: &View<'_>) -> Line<'static> {
    truncate_line(super::key_hint::line(2, template, view.palette), view.width)
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// 输入行的文字：有文字时显示文字，否则 dim 的占位文字（焦点由 `›` 表示）。
pub(super) fn input(text: &str, placeholder: &str) -> Vec<Span<'static>> {
    if text.is_empty() {
        vec![Span::styled(placeholder.to_owned(), dim())]
    } else {
        vec![Span::raw(text.to_owned())]
    }
}

/// 子代理发起的询问：标题行后接 dim 的 `· ` 与来源（docs/specs/rust-cli/agents.md §4.1.1，修复线 F67）。
fn asked_from(title: &Line<'static>, name: &str, view: &View<'_>) -> Line<'static> {
    let mut spans = title.spans.clone();
    spans.extend([
        Span::raw(" "),
        Span::styled("· ", dim()),
        Span::raw(super::i18n::asked_by(view.lang, name)),
    ]);
    truncate_line(Line::from(spans), view.width)
}

/// 参数的一行摘要（`键: 值, …`）。
fn summary(input: &Value) -> String {
    match input {
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

impl Interaction {
    /// 视图的行与选项所在的行：（行号，选项下标）。
    pub fn lines(
        &self,
        request: &InteractionRequest,
        view: &View<'_>,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        match request {
            InteractionRequest::Permission {
                tool,
                input,
                message,
                suggestions,
                asked_by,
                ..
            } => {
                let mut lines =
                    self.permission_lines(tool, input, message, !suggestions.is_empty(), view);
                if let Some(name) = asked_by {
                    lines.0[1] = asked_from(&lines.0[1], name, view);
                }
                lines
            }
            InteractionRequest::PlanApproval { plan, .. } => self.plan_lines(plan, view),
            InteractionRequest::Question { .. } => self.question_lines(request, view),
        }
    }

    /// `remember`：判决链给了建议规则，多一行"不再询问"（文案沿用 Host TUI 的 allowAlways）。
    fn permission_lines(
        &self,
        tool: &str,
        detail: &Value,
        message: &str,
        remember: bool,
        view: &View<'_>,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        let zh = view.lang == Lang::Zh;
        let path = detail["file_path"]
            .as_str()
            .or_else(|| detail["notebook_path"].as_str())
            .unwrap_or_default();
        let name = file_name(path).to_owned();
        let edit = matches!(tool, "Edit" | "MultiEdit" | "NotebookEdit");
        let (title, question) = match tool {
            "Bash" => (if zh { "Bash 命令" } else { "Bash command" }, None),
            _ if edit => (
                if zh { "编辑文件" } else { "Edit file" },
                Some(if zh {
                    ("要对 ", " 做这处修改吗？")
                } else {
                    ("Do you want to make this edit to ", "?")
                }),
            ),
            "Write" => (
                if zh { "写入文件" } else { "Write file" },
                Some(if zh {
                    ("要写入 ", " 吗？")
                } else {
                    ("Do you want to write ", "?")
                }),
            ),
            "WebFetch" => (if zh { "获取网页" } else { "Fetch" }, None),
            _ => (if zh { "使用工具" } else { "Tool use" }, None),
        };
        let mut lines = frame(title, view);
        // 说明：工具参数里的 description（Bash 的命令说明）。
        if let Some(description) = detail["description"]
            .as_str()
            .filter(|d| !d.trim().is_empty())
        {
            lines.extend(wrap_text(
                description.trim(),
                dim(),
                view.width,
                &[Span::raw("  ")],
                &[Span::raw("  ")],
            ));
        }
        lines.push(dashed(view.width));
        let bar = || Span::styled("  │ ", dim());
        if let Some(command) = detail["command"].as_str() {
            // 命令完整显示，按宽度换行（续行也带 `│`）。
            let highlighted = highlight::highlight(command, "bash", view.palette);
            for (index, raw) in command.lines().enumerate() {
                let spans = highlighted
                    .as_ref()
                    .and_then(|lines| lines.get(index).cloned())
                    .unwrap_or_else(|| vec![Span::raw(raw.to_owned())]);
                lines.extend(wrap(&spans, view.width, &[bar()], &[bar()]));
            }
        } else if !path.is_empty() {
            lines.push(truncate_line(
                Line::from(vec![bar(), Span::styled(path.to_owned(), bold())]),
                view.width,
            ));
            lines.extend(self.diff_preview(tool, detail, view));
        } else {
            let target = detail["url"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{tool}({})", summary(detail)));
            lines.extend(wrap(&[Span::raw(target)], view.width, &[bar()], &[bar()]));
        }
        lines.push(dashed(view.width));
        // 原因：为什么要审批（正常颜色）。
        if !message.trim().is_empty() {
            lines.extend(wrap_text(
                message.trim(),
                Style::default(),
                view.width,
                &[Span::raw("  ")],
                &[Span::raw("  ")],
            ));
        }
        lines.push(Line::default());
        lines.push(Line::from(match question {
            Some((before, after)) => vec![
                Span::raw(format!("  {before}")),
                Span::styled(name, bold()),
                Span::raw(after),
            ],
            None => vec![Span::raw(format!(
                "  {}",
                if zh {
                    "要继续吗？"
                } else {
                    "Do you want to proceed?"
                }
            ))],
        }));
        let accent = Style::default().fg(view.palette.accent);
        let mut options = Vec::new();
        let always = match (tool, zh) {
            ("Bash", false) => "Yes, and don't ask again for this command",
            ("Bash", true) => "是，以后这条命令不再询问",
            (_, false) if edit || tool == "Write" => "Yes, and don't ask again for these files",
            (_, true) if edit || tool == "Write" => "是，以后这些文件不再询问",
            (_, false) => "Yes, and don't ask again for this tool",
            (_, true) => "是，以后这个工具不再询问",
        };
        let mut rows = vec![if zh { "是" } else { "Yes" }];
        if remember {
            rows.push(always);
        }
        rows.push(if zh { "否" } else { "No" });
        let last = rows.len() - 1;
        for (index, text) in rows.into_iter().enumerate() {
            let focused = index == self.cursor;
            // 输入意见时 No 行就是输入框。
            let label = if index == last && (self.typing || !self.text.is_empty()) {
                input(
                    &self.text,
                    if zh {
                        "告诉 ZCode 该怎么做"
                    } else {
                        "tell ZCode what to do differently"
                    },
                )
            } else if focused {
                vec![Span::styled(text, accent)]
            } else {
                vec![Span::raw(text)]
            };
            options.push((lines.len(), index));
            lines.push(option(focused, index + 1, label, view));
        }
        lines.push(Line::default());
        let text = match (self.typing, zh) {
            (true, false) => "{Enter} to send · {Esc} to go back",
            (true, true) => "{enter} 发送 · {esc} 返回",
            (false, false) => "{Esc} to cancel · {Tab} to amend",
            (false, true) => "{esc} 取消 · {tab} 补充意见",
        };
        lines.push(hint(text, view));
        (lines, options)
    }

    /// 改动预览：与 transcript 里的 diff 相同，最多 [`PREVIEW`] 行。
    fn diff_preview(&self, tool: &str, input: &Value, view: &View<'_>) -> Vec<Line<'static>> {
        let ctx = Ctx {
            palette: view.palette,
            lang: view.lang,
            strings: view.strings,
            width: view.width,
            expanded: false,
            elapsed: std::time::Duration::ZERO,
            now_ms: view.now_ms,
            workspace: "",
            workspace_label: "",
            version: "",
            markdown: None,
        };
        let rows = cells::patch_rows(tool, input);
        let (mut lines, hidden) = cells::diff_lines(&rows, &ctx, PREVIEW);
        if hidden > 0 {
            lines.push(Line::from(Span::styled(
                format!("     … +{hidden} lines"),
                dim(),
            )));
        }
        lines
    }
}

#[path = "interaction_view_plan.rs"]
mod plan;
#[path = "interaction_view_question.rs"]
mod question;
