//! rewind 的纯计算（docs/specs/rust-cli/session-lifecycle.md §5.1、§5.2）：可选消息、要撤销的
//! checkpoint、文件恢复计划与改动统计。不做 IO。

use std::collections::HashMap;

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_rewind::{RestoreFile, content_hash};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::session_api::FileChange;
use rust_cli_kernel_api::store::WorkspaceCheckpoint;
use serde::Deserialize;

/// 中断文本（合成的消息）；这两条不是可选的 user 消息（kernel-api 的定义，F55）。
const INTERRUPTS: [&str; 2] = rust_cli_kernel_api::user_input::INTERRUPT_TEXTS;
/// 可选消息排除的非用户内容标签。
const NOT_USER_TAGS: [&str; 7] = [
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<bash-stdout>",
    "<bash-stderr>",
    "<task-notification>",
    "<tick>",
    "<teammate-message",
];

fn last_text(user: &UserMessage) -> Option<&str> {
    match user.content.last()? {
        ContentBlock::Text { text } => Some(text.trim()),
        _ => None,
    }
}

/// 可选的消息：人类输入，不是工具结果 / meta / 压缩摘要 / 中断文本。
pub(crate) fn selectable(message: &Message) -> Option<&UserMessage> {
    let Message::User(user) = message else {
        return None;
    };
    if user.origin != UserOrigin::Human {
        return None;
    }
    let text = last_text(user).unwrap_or("");
    if INTERRUPTS.contains(&text) || NOT_USER_TAGS.iter().any(|tag| text.contains(tag)) {
        return None;
    }
    Some(user)
}

/// esc 撤回的判断（修复线 F38，spec §5.5）：输入之后的这些消息里有没有
/// 回答。有非空白文字或 tool_use 的 assistant、别的人类输入（含 `<bash-stdout>`）、压缩摘要算回答；中断标记、工具结果、
/// meta、attachment、system、只有 thinking 或空白文字的 assistant 不算。
pub(crate) fn answered(after: &[Message]) -> bool {
    after.iter().any(|message| match message {
        Message::Assistant(assistant) => assistant.content.iter().any(|block| match block {
            ContentBlock::Text { text } => !text.trim().is_empty(),
            ContentBlock::ToolUse { .. } => true,
            _ => false,
        }),
        Message::User(user) => match &user.origin {
            UserOrigin::Human => !INTERRUPTS.contains(&last_text(user).unwrap_or("")),
            UserOrigin::CompactSummary => true,
            UserOrigin::ToolResult { .. } | UserOrigin::Meta { .. } => false,
        },
        Message::Attachment(_) | Message::System(_) => false,
    })
}

/// target 起要撤销的 checkpoint：`prompt` 是活动历史里 target 或之后的消息（Node
/// `activeSuffixMessageIdsForRewind` + `selectCheckpointsForMessages`），按写入顺序。
pub(crate) fn checkpoints_from<'a>(
    messages: &[Message],
    checkpoints: &'a [WorkspaceCheckpoint],
    target: &MessageId,
) -> Vec<&'a WorkspaceCheckpoint> {
    let Some(index) = messages.iter().position(|m| m.id() == target) else {
        return Vec::new();
    };
    let suffix: Vec<&MessageId> = messages[index..].iter().map(Message::id).collect();
    checkpoints
        .iter()
        .filter(|c| suffix.contains(&&c.prompt) || suffix.contains(&&c.assistant))
        .collect()
}

/// checkpoint artifact 里本工程用到的字段（Node `workspaceCheckpointArtifactSchema`）。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Artifact {
    pub files: Vec<ArtifactFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArtifactFile {
    pub path: String,
    pub existed_before: bool,
    pub before_content: Option<String>,
    pub after_content: Option<String>,
}

/// 恢复计划：按写入的逆序、按路径模拟（Node `buildWorkspaceFileRewindPlan`）。每个路径期望磁盘上是
/// 最后一次写入后的内容，恢复成最早一次写入前的内容；中间两次写入接不上（被别的方式改过）时报冲突。
pub(crate) fn plan(artifacts: &[Artifact]) -> Result<Vec<RestoreFile>, String> {
    let mut order: Vec<String> = Vec::new();
    // 路径 → (期望的磁盘内容 hash, 当前模拟到的“写入前”内容)
    let mut files: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    for file in artifacts.iter().rev().flat_map(|a| a.files.iter().rev()) {
        let Some(after) = &file.after_content else {
            return Err(format!("the checkpoint of {} has no content", file.path));
        };
        let before = file.before_content.clone().filter(|_| file.existed_before);
        match files.get_mut(&file.path) {
            None => {
                order.push(file.path.clone());
                files.insert(
                    file.path.clone(),
                    (Some(content_hash(after.as_bytes())), before),
                );
            }
            Some((_, simulated)) => {
                if simulated.as_deref() != Some(after.as_str()) {
                    return Err(conflict(&file.path));
                }
                *simulated = before;
            }
        }
    }
    Ok(order
        .into_iter()
        .filter_map(|path| {
            let (expected, restore) = files.remove(&path)?;
            Some(RestoreFile {
                path: EnvPath::new(path),
                expected,
                restore: restore.map(String::into_bytes),
            })
        })
        .collect())
}

/// 冲突的原因（每个文件一行）。
pub(crate) fn conflict(path: &str) -> String {
    format!("Cannot restore code: {path} was changed after ZCode edited it")
}

/// 一组写入合起来的改动（每个路径：最早的写入前 → 最后的写入后），按路径排序。
pub(crate) fn changes(artifacts: &[Artifact]) -> Vec<FileChange> {
    let mut first: Vec<(String, String, String)> = Vec::new();
    for file in artifacts.iter().flat_map(|a| a.files.iter()) {
        let after = file.after_content.clone().unwrap_or_default();
        match first.iter_mut().find(|(path, _, _)| *path == file.path) {
            Some(entry) => entry.2 = after,
            None => first.push((
                file.path.clone(),
                file.before_content.clone().unwrap_or_default(),
                after,
            )),
        }
    }
    let mut out: Vec<FileChange> = first
        .into_iter()
        .map(|(path, before, after)| {
            let (additions, deletions) = line_changes(&before, &after);
            FileChange {
                path,
                additions,
                deletions,
            }
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// 编辑距离超过它时不再求最小 diff，按去掉公共前后缀后的行数算。
const MAX_EDITS: usize = 10_000;

/// 增删行数：先去掉公共前后缀，再用 Myers 求最小编辑距离 D（与 jsdiff 的统计相同）；
/// LCS = (N + M - D) / 2，新增 = M - LCS，删除 = N - LCS。
fn line_changes(before: &str, after: &str) -> (u64, u64) {
    let split = |text: &str| -> Vec<String> {
        if text.is_empty() {
            return Vec::new();
        }
        let text = text.strip_suffix('\n').unwrap_or(text);
        text.split('\n').map(str::to_owned).collect()
    };
    let (old, new) = (split(before), split(after));
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let a = &old[prefix..old.len() - suffix];
    let b = &new[prefix..new.len() - suffix];
    let (n, m) = (a.len(), b.len());
    let edits = myers(a, b).unwrap_or(n + m);
    let common = (n + m - edits) / 2;
    ((m - common) as u64, (n - common) as u64)
}

/// Myers 贪心算法的最小编辑距离；超过 [`MAX_EDITS`] 时为 None。
fn myers(a: &[String], b: &[String]) -> Option<usize> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m).min(MAX_EDITS as isize);
    let offset = max + 1;
    let mut v = vec![0_isize; (2 * max + 3) as usize];
    for d in 0..=max {
        let mut k = -d;
        while k <= d {
            let index = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
                v[index + 1]
            } else {
                v[index - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[index] = x;
            if x >= n && y >= m {
                return Some(d as usize);
            }
            k += 2;
        }
    }
    None
}

#[cfg(test)]
#[path = "rewind_plan_tests.rs"]
mod tests;
