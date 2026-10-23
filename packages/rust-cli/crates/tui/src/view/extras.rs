//! L28 的视图投影（docs/specs/rust-cli/tui-display.md §2）：任务清单的快照、子代理的活动、每回合的文件改动、
//! 按模型累计的用量。都是内核事件的投影，随视图一起丢弃与重建；这里不保存内核以外的事实。
use std::collections::BTreeMap;

use rust_cli_kernel_api::agents::{TaskRecord, TaskStatus};
use rust_cli_kernel_api::ids::{ToolUseId, TurnId};
use rust_cli_kernel_api::message::Usage;
use rust_cli_kernel_api::model::BlockDelta;
use rust_cli_kernel_api::orchestration::QueryEvent;
use rust_cli_kernel_api::session_api::FileChange;
use serde_json::Value;

use super::{Notice, ToolStatus, Transcript};

/// 子代理活动里的一项（进度消息里的一条 assistant 内容）。
#[derive(Debug, Clone, PartialEq)]
pub enum Activity {
    Tool {
        id: ToolUseId,
        name: String,
        input: Value,
        status: ToolStatus,
    },
    /// 同一次响应、同一个内容块的文字增量拼成一项。
    Text {
        message: usize,
        index: usize,
        text: String,
    },
    /// 推理块的增量，拼法同文字（L35，stall-visibility.md §4.2；显示为 `∴ <推理文字>`）。
    Thinking {
        message: usize,
        index: usize,
        text: String,
    },
}

/// 前台子代理的活动（Agent 工具的 `ToolProgress{agentId, event}`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentActivity {
    pub items: Vec<Activity>,
    /// 子代理的模型请求还在生效的重试或停顿通知（L35，stall-visibility.md §3.2）。
    pub notice: Option<Notice>,
}

impl AgentActivity {
    /// 子代理调用过的工具数（进度的 tool use 计数）。
    pub fn tool_uses(&self) -> usize {
        self.items
            .iter()
            .filter(|item| matches!(item, Activity::Tool { .. }))
            .count()
    }

    /// 折叠一个子循环事件；认不出的事件忽略。流增量与工具开始清掉重试或停顿通知。
    pub(super) fn apply(&mut self, event: QueryEvent, now_ms: u64) {
        if matches!(
            event,
            QueryEvent::Delta { .. } | QueryEvent::ToolStarted { .. }
        ) {
            self.notice = None;
        }
        match event {
            QueryEvent::ToolStarted {
                tool_use,
                name,
                input,
            } => self.items.push(Activity::Tool {
                id: tool_use,
                name,
                input,
                status: ToolStatus::Running,
            }),
            QueryEvent::ToolFinished { tool_use, output } => {
                for item in &mut self.items {
                    if let Activity::Tool { id, status, .. } = item
                        && *id == tool_use
                    {
                        *status = if output.is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Success
                        };
                    }
                }
            }
            QueryEvent::Delta {
                message,
                index,
                delta: BlockDelta::Text(text),
            } => self.stream(message, index, text, false),
            QueryEvent::Delta {
                message,
                index,
                delta: BlockDelta::Thinking(text),
            } => self.stream(message, index, text, true),
            QueryEvent::ModelNotice { notice } => Notice::fold(&mut self.notice, notice, now_ms),
            QueryEvent::Delta { .. } | QueryEvent::ToolProgress { .. } => {}
        }
    }
}

impl AgentActivity {
    /// 文字或推理的增量：接在同一次响应、同一个块的最后一项后面，否则另起一项。
    fn stream(&mut self, message: usize, index: usize, text: String, thinking: bool) {
        let last = match self.items.last_mut() {
            Some(Activity::Text {
                message: m,
                index: i,
                text: shown,
            }) if !thinking => Some((*m, *i, shown)),
            Some(Activity::Thinking {
                message: m,
                index: i,
                text: shown,
            }) if thinking => Some((*m, *i, shown)),
            _ => None,
        };
        if let Some((m, i, shown)) = last
            && m == message
            && i == index
        {
            shown.push_str(&text);
            return;
        }
        self.items.push(if thinking {
            Activity::Thinking {
                message,
                index,
                text,
            }
        } else {
            Activity::Text {
                message,
                index,
                text,
            }
        });
    }
}

/// 一个回合的文件改动（同一路径相加，按首次出现的顺序）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnChanges {
    pub turn: TurnId,
    /// 这个回合的第一条用户输入（`/diff` 的回合标题）。
    pub prompt: String,
    pub files: Vec<FileChange>,
    /// 这条用户输入的行（`/diff` 从它之后的文件工具算出回合的 diff，tui-manage.md §4.1）。
    pub start: Option<u64>,
}

/// 视图里 L28 的投影；`Transcript` 持有一份。
#[derive(Debug, Clone, Default)]
pub(super) struct Extras {
    pub(super) tasks: Vec<TaskRecord>,
    pub(super) turn_changes: Vec<TurnChanges>,
    pub(super) usage: BTreeMap<String, Usage>,
    /// 后台工作的终态（`BackgroundWork{work, status}`）：Shell 详情在内核已不认识它时用（tui-panels.md §3.3）。
    pub(super) ended: BTreeMap<String, String>,
}

impl Transcript {
    /// 任务清单的最后一份快照（`TasksChanged`；冷加载没有，tui-display.md D2）。
    pub fn tasks(&self) -> &[TaskRecord] {
        &self.extras.tasks
    }

    /// 后台工作（命令或子代理）在通知里的终态：`completed`、`failed`、`killed`。
    pub fn ended_work(&self, work: &str) -> Option<&str> {
        self.extras.ended.get(work).map(String::as_str)
    }

    pub(super) fn background_ended(&mut self, work: String, status: String) {
        self.extras.ended.insert(work, status);
    }

    /// 本次视图里每个有改动的回合（`FileChanges`）。
    pub fn turn_changes(&self) -> &[TurnChanges] {
        &self.extras.turn_changes
    }

    /// 按模型累计的用量（assistant 消息的 `usage`）。
    pub fn usage(&self) -> &BTreeMap<String, Usage> {
        &self.extras.usage
    }

    /// 第 `n` 新的一条回复（1 是最后一条，`/copy N`）。
    pub fn nth_reply(&self, n: usize) -> Option<&str> {
        self.replies().nth(n.checked_sub(1)?)
    }

    /// 回复的条数。
    pub fn reply_count(&self) -> usize {
        self.replies().count()
    }

    fn replies(&self) -> impl Iterator<Item = &str> {
        self.rows.iter().rev().filter_map(|row| match &row.body {
            super::Body::Agent { text, .. } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
    }

    pub(super) fn tasks_changed(&mut self, tasks: Value) {
        // 解析失败时保留上一份（内核给的是完整清单，不会是半份）。
        if let Ok(tasks) = serde_json::from_value::<Vec<TaskRecord>>(tasks) {
            self.extras.tasks = tasks;
        }
    }

    pub(super) fn file_changes(&mut self, turn: TurnId, files: Vec<FileChange>) {
        let (start, prompt) = self.last_user();
        let changes = &mut self.extras.turn_changes;
        if changes.last().is_none_or(|last| last.turn != turn) {
            changes.push(TurnChanges {
                turn,
                prompt,
                files: Vec::new(),
                start,
            });
        }
        let Some(current) = changes.last_mut() else {
            return;
        };
        for file in files {
            match current.files.iter_mut().find(|f| f.path == file.path) {
                Some(known) => {
                    known.additions += file.additions;
                    known.deletions += file.deletions;
                }
                None => current.files.push(file),
            }
        }
    }

    pub(super) fn add_usage(&mut self, model: &str, usage: Option<Usage>) {
        let Some(usage) = usage else {
            return;
        };
        let total = self.extras.usage.entry(model.to_owned()).or_default();
        total.input_tokens += usage.input_tokens;
        total.output_tokens += usage.output_tokens;
        total.cache_creation_input_tokens += usage.cache_creation_input_tokens;
        total.cache_read_input_tokens += usage.cache_read_input_tokens;
    }

    /// Agent 工具的进度：`{agentId, event}` 折叠进工具行的活动。返回是否认出了它。
    pub(super) fn agent_progress(
        &mut self,
        tool_use: &ToolUseId,
        data: &Value,
        now_ms: u64,
    ) -> bool {
        if data.get("agentId").is_none() {
            return false;
        }
        let Ok(event) = serde_json::from_value::<QueryEvent>(data["event"].clone()) else {
            return true;
        };
        self.update_tool(tool_use, |tool| {
            tool.activity
                .get_or_insert_with(AgentActivity::default)
                .apply(event, now_ms);
        });
        true
    }

    fn last_user(&self) -> (Option<u64>, String) {
        self.rows
            .iter()
            .rev()
            .find_map(|row| match &row.body {
                super::Body::User { text } => Some((Some(row.id), text.clone())),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// 第 `index` 个回合里改了 `path` 的文件工具（名字与输入，按出现顺序）：从这一回合的用户输入之后，到下一个
    /// 有改动的回合的用户输入之前（`/diff` 的回合详情，tui-manage.md §4.2）。
    pub fn turn_edits(&self, index: usize, path: &str) -> Vec<(String, Value)> {
        let changes = &self.extras.turn_changes;
        let Some(turn) = changes.get(index) else {
            return Vec::new();
        };
        let start = turn.start.unwrap_or(0);
        let end = changes
            .get(index + 1)
            .and_then(|next| next.start)
            .unwrap_or(u64::MAX);
        let target = |input: &Value| {
            let file = input["file_path"]
                .as_str()
                .or_else(|| input["notebook_path"].as_str())
                .unwrap_or_default();
            !file.is_empty() && (file == path || file.ends_with(&format!("/{path}")))
        };
        self.rows
            .iter()
            .filter(|row| row.id > start && row.id < end)
            .filter_map(|row| row.tool())
            .filter(|tool| {
                matches!(
                    tool.name.as_str(),
                    "Edit" | "MultiEdit" | "Write" | "NotebookEdit"
                ) && tool.status == ToolStatus::Success
                    && target(&tool.input)
            })
            .map(|tool| (tool.name.clone(), tool.input.clone()))
            .collect()
    }
}

/// 任务状态的统计：（完成，进行中，待办）。
pub fn task_counts(tasks: &[TaskRecord]) -> (usize, usize, usize) {
    let done = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Completed)
        .count();
    let pending = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Pending)
        .count();
    (done, tasks.len() - done - pending, pending)
}

#[cfg(test)]
#[path = "extras_tests.rs"]
mod tests;
