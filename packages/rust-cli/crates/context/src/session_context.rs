//! `session_context` attachment（context.md §12）：会话开始时的 git 状态。
//!
//! git 的原始值由执行环境给（`Git::git_status`），文本在这里渲染。

use rust_cli_kernel_api::env::{EnvFacts, WorkspaceRef};
use rust_cli_kernel_api::env_git::{GitSnapshot, GitStatus, GitStatusRequest};
use rust_cli_kernel_api::local_services::Git;
use rust_cli_kernel_api::message::Message;
use rust_cli_prompts::texts::{self, render};
use serde_json::{Value, json};

use crate::render::attachments;
use crate::system::BASH_TOOL;

pub(crate) const SOURCE: &str = "session_context";

/// git status 超过这么多 UTF-16 单位时截断。
const STATUS_LIMIT: usize = 2000;

/// JS `substring(0, limit)`：按 UTF-16 单位截。
fn truncate_utf16(text: &str, limit: usize) -> Option<&str> {
    let mut units = 0;
    for (at, c) in text.char_indices() {
        if units + c.len_utf16() > limit {
            return Some(&text[..at]);
        }
        units += c.len_utf16();
    }
    None
}

/// gitStatus 段：各部分空一行连接。
pub(crate) fn git_text(snapshot: &GitSnapshot) -> String {
    let value = |template: &str, value: &str| render(template, &[("value", value)]);
    let status = match truncate_utf16(&snapshot.status, STATUS_LIMIT) {
        Some(head) => render(
            texts::git_status_truncated(),
            &[("status", head), ("tool", BASH_TOOL)],
        ),
        None if snapshot.status.is_empty() => texts::git_status_clean().to_owned(),
        None => snapshot.status.clone(),
    };
    let mut parts = vec![
        texts::git_status().to_owned(),
        value(texts::git_status_branch(), &snapshot.branch),
        value(texts::git_status_main(), &snapshot.default_branch),
    ];
    if let Some(user) = &snapshot.user_name {
        parts.push(value(texts::git_status_user(), user));
    }
    parts.push(value(texts::git_status_status(), &status));
    parts.push(value(texts::git_status_commits(), &snapshot.recent_commits));
    parts.join("\n\n")
}

/// 首次宣布的形状。
fn wrap(sections: &[(&str, &str)]) -> String {
    let sections: Vec<String> = sections
        .iter()
        .map(|(name, value)| {
            render(
                texts::session_context_section(),
                &[("name", name), ("value", value)],
            )
        })
        .collect();
    render(
        texts::session_context(),
        &[("sections", &sections.join("\n"))],
    )
}

/// 这一步的 `session_context`：`(text, data)`，text 未包 `<system-reminder>`。
///
/// 压缩边界之后已经宣布过时沿用它（gitStatus 取已宣布的，内容不变就不重发），
/// 所以一个边界窗口只发一次；没有宣布过且工作区是 git 仓库时才向执行环境取 git 状态。
pub(crate) async fn announce(
    git: &dyn Git,
    workspace: &WorkspaceRef,
    facts: &EnvFacts,
    history: &[Message],
) -> Option<(String, Value)> {
    if attachments(history, SOURCE).next().is_some() || !facts.is_git_repo {
        return None;
    }
    let request = GitStatusRequest {
        cwd: workspace.root.clone(),
    };
    let snapshot = match git.git_status(workspace, request).await {
        Ok(GitStatus::Repository(snapshot)) => snapshot,
        Ok(GitStatus::NotRepository) => return None,
        Err(error) => {
            tracing::debug!(%error, "git status unavailable; skipping the session context");
            return None;
        }
    };
    let git = git_text(&snapshot);
    Some((
        wrap(&[("gitStatus", &git)]),
        json!({ "context": { "gitStatus": git } }),
    ))
}

/// `/context` 的 gitStatus 段（context-usage.md §3）：边界之后已经宣布过的沿用它的文本，否则是
/// git 仓库时向执行环境取一次。
pub(crate) async fn git_section(
    git: &dyn Git,
    workspace: &WorkspaceRef,
    facts: &EnvFacts,
    history: &[Message],
) -> Option<String> {
    if let Some(found) = attachments(history, SOURCE).next() {
        return found.data["context"]["gitStatus"]
            .as_str()
            .map(str::to_owned);
    }
    announce(git, workspace, facts, history)
        .await
        .and_then(|(_, data)| data["context"]["gitStatus"].as_str().map(str::to_owned))
}

#[cfg(test)]
#[path = "session_context_tests.rs"]
mod tests;
