//! `nested_memory` attachment（context.md §11.6）：上一步 Read / Write / Edit 成功触及的文件带出的
//! 子目录规则与命中的条件规则。
//!
//! 工具读写的路径是触发，下一步的当步 attachment 处理。
//! 这里从历史推导触发与去重，context 不保存会话状态。

use std::collections::HashSet;

use rust_cli_kernel_api::env::EnvFacts;
use rust_cli_kernel_api::env_shell;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, Message};
use serde_json::{Value, json};

use crate::instructions::{self, Announced};
#[cfg(test)]
use crate::render::attachments;
use crate::rules::{RuleFile, Scope};
use crate::rules_load::Worktree;

pub(crate) const SOURCE: &str = "nested_memory";

/// 会把路径记为触发的工具（Read、Write、Edit；NotebookEdit 不触发）。
const TRIGGERING: &[&str] = &["Read", "Write", "Edit"];

#[path = "nested_seen.rs"]
mod seen;
pub(crate) use seen::Seen;

/// 成功的 Read / Write / Edit 调用的原始 `file_path`，按调用顺序。只看 `messages` 范围内的调用。
fn touched_paths(messages: &[Message]) -> Vec<String> {
    let mut failed = HashSet::new();
    let mut answered = HashSet::new();
    for message in messages {
        let Message::User(user) = message else {
            continue;
        };
        for block in &user.content {
            if let ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                ..
            } = block
            {
                answered.insert(tool_use_id.clone());
                if *is_error {
                    failed.insert(tool_use_id.clone());
                }
            }
        }
    }
    let mut paths = Vec::new();
    for message in messages {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        for block in &assistant.content {
            let ContentBlock::ToolUse { id, name, input } = block else {
                continue;
            };
            let path = input.get("file_path").and_then(Value::as_str);
            if let Some(path) = path.filter(|path| !path.is_empty())
                && TRIGGERING.contains(&name.as_str())
                && answered.contains(id)
                && !failed.contains(id)
            {
                paths.push(path.to_owned());
            }
        }
    }
    paths
}

/// 这一步的触发：最近一条 assistant 消息里成功的调用（结果已在其后的历史里）。
fn triggers(history: &[Message]) -> Vec<String> {
    let Some(last) = history
        .iter()
        .rposition(|message| matches!(message, Message::Assistant(_)))
    else {
        return Vec::new();
    };
    let mut paths = touched_paths(&history[last..]);
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    paths
}

/// 读写过的文件与发过的 `nested_memory`（读取状态：在里面的不再发）。从头算的写法，
/// 是 [`Seen::known`] 的对照（`nested_tests.rs`）。
#[cfg(test)]
fn already_known(scope: &Scope<'_>, history: &[Message]) -> HashSet<String> {
    let mut known: HashSet<String> = touched_paths(history)
        .iter()
        .map(|path| scope.style.key(&scope.resolve(path)))
        .collect();
    for attachment in attachments(history, SOURCE) {
        if let Some(path) = attachment.data.get("path").and_then(Value::as_str) {
            known.insert(scope.style.key(path));
        }
    }
    known
}

impl<'a> Scope<'a> {
    /// 工具输入里的路径：与工具同一条规则（触发是工具解析后的路径：去首尾空白、
    /// NFC、只认 `~/`，tools-files.md §3.2），相对路径按 `cwd`（触发用的范围见 [`Scope::at`]）。
    ///
    /// 缺陷修复（F14）：以前用 `resolve`，工具改成去空白与 NFC 之后，同一个调用在这里会落到另一个路径上。
    pub(crate) fn resolve(&self, path: &str) -> String {
        self.style
            .resolve_user_path(&self.cwd, path, self.home.as_deref())
    }

    /// 同样的端口与事实，相对路径换成以 `cwd` 为基准。
    fn at(&self, cwd: String) -> Scope<'a> {
        Scope {
            env: self.env,
            workspace: self.workspace,
            style: self.style,
            cwd,
            home: self.home.clone(),
        }
    }
}

/// 这一步的 `nested_memory`，每个一条：`(text, data)`。`files` 是本会话当前的规则文件：已宣布且
/// 仍在其中的不再随 nested memory 发。
pub(crate) async fn announce(
    (scope, seen, session): (&Scope<'_>, &Seen, &SessionId),
    facts: &EnvFacts,
    history: &[Message],
    files: &[RuleFile],
) -> Vec<(String, Value)> {
    let triggers = triggers(history);
    if triggers.is_empty() {
        return Vec::new();
    }
    let style = scope.style;
    // 触发路径按会话的当前目录解析（工具调用时当前目录下的绝对路径）；范围与目录层仍按工作区根
    // （tools-files.md §3.3）。缺陷修复（F19）：以前按工作区根解析，Bash `cd` 之后的相对路径
    // 落到另一个文件上，工具读的是 A、这里找的是 B 的规则。
    let cwd = env_shell::current_dir(scope.env, scope.workspace, session).await;
    let based = scope.at(cwd.as_str().to_owned());
    let current = instructions::current(files);
    let announced: Vec<Announced> = instructions::announced(history)
        .unwrap_or_default()
        .into_iter()
        .filter(|old| current.iter().any(|file| file.path == old.path))
        .collect();
    // 工作目录：工作区根与附加目录。
    let mut roots = vec![scope.cwd.clone()];
    roots.extend(
        facts
            .additional_directories
            .iter()
            .map(|dir| dir.as_str().to_owned()),
    );
    // 只在有候选规则文件时才算，按会话增量算（修复线 F6，`nested_seen.rs`）。
    let mut known: Option<HashSet<String>> = None;
    let worktree: Option<Worktree> = scope.worktree().await;
    let mut out = Vec::new();
    for trigger in triggers {
        let target = based.resolve(&trigger);
        if !roots.iter().any(|root| style.is_within(&target, root)) {
            continue;
        }
        for file in scope
            .nested(&facts.platform, &target, worktree.as_ref())
            .await
        {
            let known = known.get_or_insert_with(|| seen.known(session, &based, history));
            if !known.insert(style.key(&file.path)) {
                continue;
            }
            let trimmed = file.content.trim();
            if announced
                .iter()
                .any(|old| old.path == file.path && old.content == trimmed)
            {
                continue;
            }
            out.push(render(scope, &file));
        }
    }
    out
}

fn render(scope: &Scope<'_>, file: &RuleFile) -> (String, Value) {
    let display = scope
        .style
        .relative(&scope.cwd, &file.path)
        .filter(|relative| !relative.is_empty())
        .map_or_else(
            || file.path.clone(),
            |relative| relative.replace('/', &scope.style.separator().to_string()),
        );
    let mut content = json!({
        "path": file.path,
        "type": file.kind.name(),
        "content": file.content,
    });
    if let Some(globs) = &file.globs {
        content["globs"] = json!(globs);
    }
    if let Some(parent) = &file.parent {
        content["parent"] = json!(parent);
    }
    (
        instructions::entry(&file.path, "", &file.content),
        json!({ "path": file.path, "content": content, "displayPath": display }),
    )
}

#[cfg(test)]
#[path = "nested_tests.rs"]
mod tests;
