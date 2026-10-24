// 改写自 apps/zcode-host/crates/tui/src/ui/interaction_view.rs（feat/rs 79b6691c55）：输入换成
// `InteractionRequest`；改动预览用 transcript 的 diff（从工具输入算）。
//! 审批的排版：强调色的 `─` 线、粗体标题，内容放在两条 dim 虚线之间，然后是原因、问题与选项，最后一行
//! 提示。计划审批与提问的排版见 `interaction_view_plan.rs`、`interaction_view_question.rs`。
//!
//! 选项的焦点行（docs/specs/rust-cli/tui.md §7.6、§7.7）：权限询问、计划审批、扣住的同伴消息与提问视图同一种画法，
//! 不铺底色条，焦点行是强调色的 `❯ ` 与名字，序号 dim；输入行（“否”行的意见、继续计划、提问的“其他”）聚焦时
//! [`Interaction::cursor`] 给出终端光标的位置。工作线 F76（用户决定 2026-10-23）：前三种以前用选择列表组件的底色条，
//! 底色条盖住输入、看不到光标。
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use serde_json::Value;

use super::cells::{self, Ctx};
use super::highlight;
use super::i18n::{Lang, Strings};
use super::interaction::Interaction;
use super::palette::Palette;
use super::text::{truncate_line, width, wrap, wrap_text};

/// 改动预览与计划最多显示的行数。
pub(super) const PREVIEW: usize = 12;

/// 视图排版所需的上下文。
pub struct View<'a> {
    pub palette: &'a Palette,
    pub strings: &'a Strings,
    pub lang: Lang,
    pub width: usize,
    /// 终端的行数（带预览的题按它限制预览框的行数，tools-more.md §1.1）。
    pub height: usize,
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

/// 焦点行的指针。
pub(super) const POINTER: &str = "❯ ";

/// 选项所在的行：（行号，选项下标）。
pub(super) type Rows = Vec<(usize, usize)>;
/// 终端光标：（行号，列）。
pub(super) type At = Option<(usize, usize)>;

/// 一行选项的开头：缩进、指针（焦点行是强调色的 `❯ `，其余两个空格）与 dim 的序号。
pub(super) fn prefix(focused: bool, number: usize, view: &View<'_>) -> Vec<Span<'static>> {
    let pointer = if focused {
        Span::styled(POINTER, Style::default().fg(view.palette.accent))
    } else {
        Span::raw("  ")
    };
    vec![
        Span::raw("  "),
        pointer,
        Span::styled(format!("{number}. "), dim()),
    ]
}

/// 选项的名字：焦点行是强调色，其余不上色。
pub(super) fn named(text: impl Into<String>, focused: bool, view: &View<'_>) -> Span<'static> {
    if focused {
        Span::styled(text.into(), Style::default().fg(view.palette.accent))
    } else {
        Span::raw(text.into())
    }
}

/// 方框：勾上是成功色的 `[✔]`，没勾是 `[ ]`，后面一个空格（提问视图的多选，工作线 F74；切到完全访问的确认框，F87）。
pub(super) fn checkbox(checked: bool, view: &View<'_>) -> [Span<'static>; 2] {
    let mark = if checked {
        Span::styled("[✔]", view.palette.success())
    } else {
        Span::raw("[ ]")
    };
    [mark, Span::raw(" ")]
}

/// 一行选项（放不下时截断）：不铺底色条（工作线 F76，同提问视图；以前按选择列表组件画 `› ` 加底色条）。
pub(super) fn option(
    focused: bool,
    number: usize,
    label: Vec<Span<'static>>,
    view: &View<'_>,
) -> Line<'static> {
    let mut spans = prefix(focused, number, view);
    spans.extend(label);
    truncate_line(Line::from(spans), view.width)
}

/// 输入行：`first` 是行首（缩进、指针、序号，多选还有方框），`content` 是文字或 dim 的占位文字（`empty` 时）；按宽度
/// 折行，续行与文字对齐。`at` 是这一行在视图里的行号。返回行与聚焦时终端光标的位置：文字末尾；空的时候在占位文字的
/// 第一个字上；最后一行正好占满时停在这一行的最后一格（tui.md §7.6）。
pub(super) fn input_row(
    first: Vec<Span<'static>>,
    content: Vec<Span<'static>>,
    empty: bool,
    at: usize,
    view: &View<'_>,
) -> (Vec<Line<'static>>, (usize, usize)) {
    let indent = [Span::raw(" ".repeat(width(&first)))];
    let rows = wrap(&content, view.width, &first, &indent);
    let (row, column) = if empty {
        (at, width(&first))
    } else {
        (at + rows.len() - 1, rows.last().map_or(0, Line::width))
    };
    (rows, (row, column.min(view.width.saturating_sub(1))))
}

/// 底部的按键提示（`{键}` 模板，按键提示组件画，tui.md §7.4）。
pub(super) fn hint(template: &str, view: &View<'_>) -> Line<'static> {
    truncate_line(super::key_hint::line(2, template, view.palette), view.width)
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// 输入行的文字：有文字时显示文字，否则 dim 的占位文字（焦点由行首的指针表示）。
pub(super) fn input(text: &str, placeholder: &str) -> Vec<Span<'static>> {
    if text.is_empty() {
        vec![Span::styled(placeholder.to_owned(), dim())]
    } else {
        vec![Span::raw(text.to_owned())]
    }
}

/// 子代理发起的询问：标题行后接 dim 的 `· ` 与来源（docs/specs/rust-cli/agents.md §4.1.1，修复线 F67）；旧会话的
/// 询问另标出会话（§4.1.2，工作线 F72）。
fn asked_from(
    title: &Line<'static>,
    name: &str,
    origin: Option<&str>,
    view: &View<'_>,
) -> Line<'static> {
    let mut spans = title.spans.clone();
    let from = match origin {
        Some(session) => super::i18n::asked_by_session(view.lang, name, session),
        None => super::i18n::asked_by(view.lang, name),
    };
    spans.extend([Span::raw(" "), Span::styled("· ", dim()), Span::raw(from)]);
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
    ) -> (Vec<Line<'static>>, Rows) {
        let (lines, options, _) = self.layout(request, view);
        (lines, options)
    }

    /// 终端光标在视图里的（行号，列）：焦点在输入行时有（权限询问在写意见、计划审批在“继续计划”、提问在“其他”行，
    /// tui.md §7.6、§7.7），其余时候隐藏光标。
    pub fn cursor(&self, request: &InteractionRequest, view: &View<'_>) -> At {
        self.layout(request, view).2
    }

    /// 排好的视图：行、选项所在的行与终端光标。
    fn layout(
        &self,
        request: &InteractionRequest,
        view: &View<'_>,
    ) -> (Vec<Line<'static>>, Rows, At) {
        match request {
            InteractionRequest::Permission {
                tool,
                input,
                message,
                suggestions,
                asked_by,
                ..
            } => {
                let always = self.always_label(tool, suggestions, view);
                let editing = self.editing_seed(suggestions);
                let mut layout = self.permission_lines(tool, input, message, always, editing, view);
                if let Some(name) = asked_by {
                    layout.0[1] = asked_from(&layout.0[1], name, self.origin.as_deref(), view);
                }
                layout
            }
            InteractionRequest::PlanApproval { plan, .. } => self.plan_lines(plan, view),
            InteractionRequest::Question { .. } => self.question_layout(request, view),
        }
    }

    /// 有建议规则时“不再询问”一行的文字：Bash 有种子时是 `… for: <种子>`，没有种子时列出建议规则（工作线 F72，
    /// shell-permissions.md §4.10）；别的工具沿用 Host TUI 的 allowAlways。
    fn always_label(
        &self,
        tool: &str,
        suggestions: &[rust_cli_kernel_api::permission::PermissionRule],
        view: &View<'_>,
    ) -> Option<String> {
        if suggestions.is_empty() {
            return None;
        }
        let zh = view.lang == Lang::Zh;
        let edit = matches!(tool, "Edit" | "MultiEdit" | "NotebookEdit" | "Write");
        Some(match (tool, &self.seed) {
            ("Bash", Some(seed)) => {
                format!(
                    "{}{seed}",
                    super::interaction::remember::seed_label(view.lang)
                )
            }
            ("Bash", None) => super::interaction::remember::commands_label(
                suggestions,
                &self.workspace,
                view.lang,
            ),
            _ if edit && zh => "是，以后这些文件不再询问".to_owned(),
            _ if edit => "Yes, and don't ask again for these files".to_owned(),
            _ if zh => "是，以后这个工具不再询问".to_owned(),
            _ => "Yes, and don't ask again for this tool".to_owned(),
        })
    }

    /// `always`：判决链给了建议规则时多一行"不再询问"的文字；`editing`：焦点在这一行的可编辑前缀上。
    fn permission_lines(
        &self,
        tool: &str,
        detail: &Value,
        message: &str,
        always: Option<String>,
        editing: bool,
        view: &View<'_>,
    ) -> (Vec<Line<'static>>, Rows, At) {
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
        let mut options = Vec::new();
        let mut cursor = None;
        let mut rows = vec![if zh { "是" } else { "Yes" }.to_owned()];
        rows.extend(always);
        rows.push(if zh { "否" } else { "No" }.to_owned());
        let last = rows.len() - 1;
        for (index, text) in rows.into_iter().enumerate() {
            let focused = index == self.cursor;
            options.push((lines.len(), index));
            // 输入意见时 No 行就是输入框；写意见时（`typing`，焦点一定在这一行）显示光标。
            if index == last && (self.typing || !self.text.is_empty()) {
                let placeholder = if zh {
                    "告诉 ZCode 该怎么做"
                } else {
                    "tell ZCode what to do differently"
                };
                let content = input(&self.text, placeholder);
                let first = prefix(focused, index + 1, view);
                let (rows, at) = input_row(first, content, self.text.is_empty(), lines.len(), view);
                lines.extend(rows);
                cursor = cursor.or(self.typing.then_some(at));
            } else if focused && editing && index == 1 {
                // 焦点在“不再询问”的可编辑前缀上（工作线 F72）：键入改前缀，光标停在末尾。
                let content = vec![named(text, true, view)];
                let (rows, at) =
                    input_row(prefix(true, 2, view), content, false, lines.len(), view);
                lines.extend(rows);
                cursor = Some(at);
            } else {
                lines.push(option(
                    focused,
                    index + 1,
                    vec![named(text, focused, view)],
                    view,
                ));
            }
        }
        lines.push(Line::default());
        let text = match (self.typing, zh) {
            (true, false) => "{Enter} to send · {Esc} to go back",
            (true, true) => "{enter} 发送 · {esc} 返回",
            (false, false) => "{Esc} to cancel · {Tab} to amend",
            (false, true) => "{esc} 取消 · {tab} 补充意见",
        };
        lines.push(hint(text, view));
        (lines, options, cursor)
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
#[path = "interaction_view_preview.rs"]
mod preview;
#[path = "interaction_view_question.rs"]
mod question;
