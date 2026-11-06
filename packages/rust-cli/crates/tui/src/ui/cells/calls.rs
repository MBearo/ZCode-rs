//! 旧界面的行 → kit 的中性数据（docs/specs/rust-cli/tui-kit.md §4.4 K3）：工具调用是 [`Call`]，一段工作的成员是
//! [`Member`]。工具行、收拢的句子、名册都由 kit 画（`app::widgets::{tools, tool_row, gather, roster}`），这里只换数据。
//!
//! Puffin 的投影在 host 里算的事实（名字、摘要、改动的 diff），旧界面的视图没有，在这里按同一规则从输入与输出算：
//! [`display_name`]、[`summary_of`]、[`diff_of`] 照 Puffin `host/fold/tools.rs`（两套界面不能互相依赖，§4.4）。
use rust_cli_tui_kit::app::widgets::gather::{Member, Thought};
use rust_cli_tui_kit::app::widgets::tools::{
    Call, Changes, Current, Facts, NumberedLine, Sign, Status, mcp_of_name,
};
use serde_json::Value;

use crate::view::{Activity, Body, Row, ToolRow, ToolStatus};

/// 摘要的最大字数（超出时以 `…` 结尾）。
const SUMMARY_MAX_CHARS: usize = 120;
/// 摘要按次序取输入里第一个有内容的字段。
const SUMMARY_INPUT_KEYS: [&str; 8] = [
    "description",
    "command",
    "file_path",
    "path",
    "pattern",
    "url",
    "query",
    "prompt",
];
/// 改动前后保留的上下文行数。
const CONTEXT: usize = 3;

/// 一个调用要借给 kit 的、视图里没有现成的字：条目 id、名字、摘要、改动的行、子代理此刻的调用。
pub(crate) struct Owned {
    id: String,
    name: String,
    summary: String,
    diff: Option<(u64, u64, Vec<NumberedLine>)>,
}

/// 一段工作的一个成员要借给 kit 的：思考的 id，或调用。
pub(crate) enum Held<'a> {
    Thought {
        id: String,
        text: &'a str,
        ended: bool,
        duration_ms: Option<u64>,
    },
    Call(&'a ToolRow, Owned),
}

impl<'a> Held<'a> {
    /// 视图的一行作为一段工作的成员；不是思考或调用时为 None。
    pub(crate) fn of(row: &'a Row) -> Option<Self> {
        match &row.body {
            Body::Reasoning {
                text,
                streaming,
                duration_ms,
            } => Some(Held::Thought {
                id: member_id(row),
                text,
                ended: !*streaming,
                duration_ms: *duration_ms,
            }),
            Body::Tool(tool) => Some(Held::Call(tool, owned(row, tool))),
            _ => None,
        }
    }

    /// kit 的成员（借着这一项）。
    pub(crate) fn member(&self) -> Member<'_> {
        match self {
            Held::Thought {
                id,
                text,
                ended,
                duration_ms,
            } => Member::Thought(Thought {
                id,
                text,
                ended: *ended,
                duration_ms: *duration_ms,
            }),
            Held::Call(tool, owned) => Member::Call(call(tool, owned)),
        }
    }
}

/// 一段工作里成员的 id：视图行的 id（播放头与动效按它找回行）。
pub(crate) fn member_id(row: &Row) -> String {
    format!("r{}", row.id)
}

/// 成员的 id 写回视图行的 id。
pub(crate) fn row_of(id: &str) -> Option<u64> {
    id.strip_prefix('r')?.parse().ok()
}

/// 一个调用的字（[`call`] 借它）。
pub(crate) fn owned(row: &Row, tool: &ToolRow) -> Owned {
    Owned {
        id: member_id(row),
        name: display_name(&tool.name),
        summary: summary_of(&tool.name, &tool.input),
        diff: (tool.status == ToolStatus::Success)
            .then(|| diff_of(&tool.name, &tool.input))
            .flatten(),
    }
}

/// 视图的状态在 kit 里的写法。
pub(crate) fn status(status: ToolStatus) -> Status {
    match status {
        ToolStatus::Pending => Status::Queued,
        ToolStatus::Approval => Status::Asking,
        ToolStatus::Running => Status::Running,
        ToolStatus::Success => Status::Done,
        ToolStatus::Error => Status::Error,
        ToolStatus::Cancelled => Status::Cancelled,
    }
}

/// 一个调用作为 kit 的 [`Call`]（Puffin `tools::of_tool`）。
pub(crate) fn call<'a>(tool: &'a ToolRow, owned: &'a Owned) -> Call<'a> {
    let failed = tool.status == ToolStatus::Error;
    Call {
        id: &owned.id,
        name: &owned.name,
        summary: &owned.summary,
        status: status(tool.status),
        response: &tool.response,
        ended: !tool.status.is_live(),
        facts: Some(Facts {
            tool: &tool.name,
            input: &tool.input,
            output: if failed { "" } else { &tool.output },
            error: if failed { &tool.output } else { "" },
            diff: owned.diff.as_ref().map(|(added, removed, lines)| Changes {
                added: *added,
                removed: *removed,
                lines,
            }),
            preview: &tool.preview,
            writing: false,
            started_at: tool.started_ms,
            ended_at: tool.ended_ms,
            progress: None,
            backgrounded: tool.input["run_in_background"].as_bool().unwrap_or(false),
            mcp: mcp_of_name(&tool.name),
            task_status: "",
            command: "",
            current: current(tool),
        }),
    }
}

/// 在跑的子代理此刻的调用：它最后开始的工具（Puffin 记子代理最新的 `ToolStarted`）。
fn current(tool: &ToolRow) -> Option<Current<'_>> {
    tool.activity
        .as_ref()?
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            Activity::Tool { name, input, .. } => Some(Current { tool: name, input }),
            _ => None,
        })
}

fn clip(line: &str) -> String {
    let one = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > SUMMARY_MAX_CHARS {
        let head: String = one.chars().take(SUMMARY_MAX_CHARS - 1).collect();
        format!("{head}…")
    } else {
        one
    }
}

/// 工具行的名字：问你事情的工具按它问的事命名（Puffin `display_name`）。
pub(crate) fn display_name(tool: &str) -> String {
    match tool {
        "AskUserQuestion" | "McpElicitation" => "Question".to_owned(),
        "ExitPlanMode" => "Plan".to_owned(),
        other => other.to_owned(),
    }
}

/// 工具行的摘要（Puffin `summary_of`）。
pub(crate) fn summary_of(tool: &str, input: &Value) -> String {
    let text = |key: &str| input[key].as_str().unwrap_or_default();
    match tool {
        "AskUserQuestion" => match input["questions"].as_array().map_or(0, Vec::len) {
            0 => String::new(),
            1 => "1 question".to_owned(),
            n => format!("{n} questions"),
        },
        "ExitPlanMode" => String::new(),
        "Skill" => clip(text("skill")),
        "Bash" => clip(if text("command").is_empty() {
            text("description")
        } else {
            text("command")
        }),
        _ => SUMMARY_INPUT_KEYS
            .iter()
            .map(|key| text(key))
            .find(|value| !value.trim().is_empty())
            .map(clip)
            .unwrap_or_default(),
    }
}

/// 两段文字的行级 diff：相同的开头与结尾各留 [`CONTEXT`] 行上下文，中间是删除与新增。删掉的行按旧文本、其余按新
/// 文本编号（从片段的第一行算起：内核不给文件里的行号）。
fn diff_text(old: &str, new: &str, out: &mut Vec<NumberedLine>) {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let mut push = |sign, text: &str, at: usize| {
        out.push(NumberedLine {
            sign,
            text: text.to_owned(),
            line: at as u64 + 1,
        });
    };
    let head = prefix.saturating_sub(CONTEXT);
    for (at, text) in new.iter().enumerate().take(prefix).skip(head) {
        push(Sign::Context, text, at);
    }
    for (at, text) in old.iter().enumerate().take(old.len() - suffix).skip(prefix) {
        push(Sign::Remove, text, at);
    }
    for (at, text) in new.iter().enumerate().take(new.len() - suffix).skip(prefix) {
        push(Sign::Add, text, at);
    }
    for (at, text) in new
        .iter()
        .enumerate()
        .skip(new.len() - suffix)
        .take(CONTEXT)
    {
        push(Sign::Context, text, at);
    }
}

/// 文件类工具的改动：加、减的行数与差异的行；不是文件类工具时为 None（Puffin `diff_of`）。
pub(crate) fn diff_of(tool: &str, input: &Value) -> Option<(u64, u64, Vec<NumberedLine>)> {
    let text = |value: &Value, key: &str| value[key].as_str().unwrap_or_default().to_owned();
    let mut lines = Vec::new();
    match tool {
        "Edit" => diff_text(
            &text(input, "old_string"),
            &text(input, "new_string"),
            &mut lines,
        ),
        "MultiEdit" => {
            for edit in input["edits"].as_array().into_iter().flatten() {
                diff_text(
                    &text(edit, "old_string"),
                    &text(edit, "new_string"),
                    &mut lines,
                );
            }
        }
        "Write" => {
            for (at, line) in text(input, "content").lines().enumerate() {
                lines.push(NumberedLine {
                    sign: Sign::Add,
                    text: line.to_owned(),
                    line: at as u64 + 1,
                });
            }
        }
        _ => return None,
    }
    let count = |sign| lines.iter().filter(|line| line.sign == sign).count() as u64;
    Some((count(Sign::Add), count(Sign::Remove), lines))
}
