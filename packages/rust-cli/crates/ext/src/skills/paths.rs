//! `paths` 条件 skill（docs/specs/rust-cli/ext.md §2.6）：历史里成功的 Read / Write / Edit 触及的文件命中模式后激活，
//! 之后一直可用。
//!
//! 激活不记在进程状态里，而是从历史推导（与 nested memory 相同，context.md §11.6），
//! resume 后结果相同。

use std::collections::HashSet;

use rust_cli_base::glob::Globs;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::message::{ContentBlock, Message};
use serde_json::Value;

use super::Skill;

/// 触发激活的工具。
const TRIGGERING: &[&str] = &["Read", "Write", "Edit"];

/// 历史里成功的 Read / Write / Edit 触及的文件，换成相对 cwd 的 `/` 路径；越出 cwd 的不算。
pub(crate) fn touched(history: &[Message], cwd: &str) -> Vec<String> {
    let mut answered = HashSet::new();
    let mut failed = HashSet::new();
    for message in history {
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
    let mut out = Vec::new();
    for message in history {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        for block in &assistant.content {
            let ContentBlock::ToolUse { id, name, input } = block else {
                continue;
            };
            if !TRIGGERING.contains(&name.as_str()) || !answered.contains(id) || failed.contains(id)
            {
                continue;
            }
            if let Some(relative) = input
                .get("file_path")
                .and_then(Value::as_str)
                .and_then(|path| relative_to(path, cwd))
            {
                out.push(relative);
            }
        }
    }
    out
}

/// 相对 cwd 的 `/` 路径（`path.relative(cwd, file)`）；在 cwd 外面或就是 cwd 时为 None。
/// 路径规则是 kernel-api `env_path`（R1-9：以前按“含不含 `\`”猜风格、只做前缀比较）。
fn relative_to(path: &str, cwd: &str) -> Option<String> {
    let relative = PathStyle::infer(cwd)
        .relative(cwd, path)?
        .replace('\\', "/");
    (!relative.is_empty() && !relative.starts_with("..")).then_some(relative)
}

/// 无条件的 skill 总是可用；条件 skill 在触及的文件命中它的模式之后可用。
pub(crate) fn active(skill: &Skill, touched: &[String]) -> bool {
    let Some(patterns) = &skill.paths else {
        return true;
    };
    let globs = Globs::new(patterns);
    touched.iter().any(|path| globs.matches(path))
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
