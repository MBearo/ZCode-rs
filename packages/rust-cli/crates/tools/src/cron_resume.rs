//! resume 复原会话任务（docs/specs/rust-cli/tools-more.md §18.2）。
//!
//! 唯一数据源是已有的消息记录：CronCreate 的调用（`cron`、`prompt`，创建时间取所在 assistant 消息 ID 的
//! 毫秒）、它的结果（任务 ID 与种类）与 CronDelete 的调用。持久任务（结果里有 `Persisted to`）的事实源是
//! 文件，跳过。loop 唤醒不复原（只在后台会话里复原）。

use std::collections::{HashMap, HashSet};

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent};

use crate::cron_board::{CronBoard, Job, JobKind};
use crate::cron_expr::{self, RECURRING_MAX_AGE_MS};
use crate::cron_tools::{CREATE, DELETE};

/// 一次 CronCreate 调用：输入与创建时间（取不到时为 None）。
struct Call {
    cron: String,
    prompt: String,
    created_at: Option<u64>,
}

/// CronCreate 结果里的任务 ID 与是否循环；持久任务与认不出的结果为 None。
fn scheduled(text: &str) -> Option<(String, bool)> {
    if text.contains("Persisted to ") {
        return None;
    }
    let (rest, recurring) = match text.strip_prefix("Scheduled recurring job ") {
        Some(rest) => (rest, true),
        None => (text.strip_prefix("Scheduled one-shot task ")?, false),
    };
    let id = rest.split(' ').next()?;
    (!id.is_empty()).then(|| (id.to_owned(), recurring))
}

fn text_of(content: &[ToolResultContent]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ToolResultContent::Text { text } => Some(text.as_str()),
            ToolResultContent::Image { .. }
            | ToolResultContent::Document { .. }
            | ToolResultContent::ToolReference { .. } => None,
        })
        .collect()
}

impl CronBoard {
    /// 把历史里还该存在的会话任务加回任务表（跳过表里已有的 ID），返回加回的个数。
    pub(crate) fn restore(&self, owner: &SessionId, history: &[Message]) -> usize {
        let mut calls: HashMap<String, Call> = HashMap::new();
        let mut deleted: HashSet<String> = HashSet::new();
        let mut found: Vec<(String, bool, Call)> = Vec::new();
        for message in history {
            match message {
                Message::Assistant(assistant) => {
                    for block in &assistant.content {
                        let ContentBlock::ToolUse { id, name, input } = block else {
                            continue;
                        };
                        let field = |key: &str| input.get(key).and_then(|v| v.as_str());
                        if name == CREATE
                            && let (Some(cron), Some(prompt)) = (field("cron"), field("prompt"))
                        {
                            let call = Call {
                                cron: cron.to_owned(),
                                prompt: prompt.to_owned(),
                                created_at: assistant.id.created_ms(),
                            };
                            calls.insert(id.as_str().to_owned(), call);
                        } else if name == DELETE
                            && let Some(job) = field("id")
                        {
                            deleted.insert(job.to_owned());
                        }
                    }
                }
                Message::User(user) => {
                    for block in &user.content {
                        let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error: false,
                        } = block
                        else {
                            continue;
                        };
                        if let Some((job, recurring)) = scheduled(&text_of(content))
                            && let Some(call) = calls.remove(tool_use_id.as_str())
                        {
                            found.push((job, recurring, call));
                        }
                    }
                }
                Message::Attachment(_) | Message::System(_) => {}
            }
        }
        let now = self.now_ms();
        let offset = self.offset();
        let jobs: Vec<Job> = found
            .into_iter()
            .filter(|(job, ..)| !deleted.contains(job))
            .filter_map(|(id, recurring, call)| {
                let (created_at, next_fire) = if recurring {
                    // 取不到创建时间时按现在（任务保留）。
                    let created_at = call.created_at.unwrap_or(now);
                    if now.saturating_sub(created_at) >= RECURRING_MAX_AGE_MS {
                        return None;
                    }
                    let next = cron_expr::recurring_next(&call.cron, now, &id, &offset);
                    (created_at, next)
                } else {
                    let created_at = call.created_at?;
                    let next = cron_expr::one_shot_next(&call.cron, created_at, &id, &offset)
                        .filter(|at| *at >= now)?;
                    (created_at, Some(next))
                };
                Some(Job {
                    id,
                    cron: call.cron,
                    prompt: call.prompt,
                    recurring,
                    created_at,
                    kind: JobKind::Cron,
                    next_fire,
                })
            })
            .collect();
        let mut state = self.state();
        let list = state.jobs.entry(owner.clone()).or_default();
        let mut added = 0;
        for job in jobs {
            if list.iter().any(|known| known.id == job.id) {
                continue;
            }
            list.push(job);
            added += 1;
        }
        if added > 0 {
            state.enabled = true;
        }
        drop(state);
        if added > 0 {
            self.changed();
        }
        added
    }
}

#[cfg(test)]
#[path = "cron_resume_tests.rs"]
mod tests;
