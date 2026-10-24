//! `-p` 下未知斜杠命令的提醒（docs/specs/rust-cli/ext.md §2.7）。
//!
//! 只在单次的打印会话里：输入照常发给模型，后面跟一条 `unknown_command_fallback` attachment。

use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::StatFile;
use rust_cli_kernel_api::machine::Files;

use super::Skill;
use super::closest::closest;
use super::names::nameable;

/// attachment 的来源（attachment type，user 角色）。
pub(crate) const SOURCE: &str = "unknown_command_fallback";
/// 最多列出的命令数。
const LISTED: usize = 40;

/// 像命令名（不像路径或一句话）。
fn looks_like_command(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-'))
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

/// 提醒的正文。可调用的命令是模型能用的 skill（不含 `disable-model-invocation` 与 `hidden`），
/// 命令总数是用户能输入的 skill 数（本工程 `-p` 下没有别的斜杠命令）。
pub(crate) fn text(name: &str, skills: &[Arc<Skill>]) -> String {
    let available: Vec<&str> = skills
        .iter()
        .filter(|skill| skill.user_invocable && !skill.disable_model_invocation)
        .map(|skill| skill.name.as_str())
        .collect();
    let total = skills.iter().filter(|skill| skill.user_invocable).count();
    let listed: Vec<&str> = available
        .iter()
        .copied()
        .filter(|name| nameable(name))
        .take(LISTED)
        .collect();
    let more = total.saturating_sub(listed.len());
    let mut parts = vec![if nameable(name) {
        format!(
            "The user's message starts with the slash command /{name}, but no command with that name is available in this session, so it did not run."
        )
    } else {
        "The user's message starts with a slash command, but no command with that name is available in this session, so it did not run.".to_owned()
    }];
    if let Some(suggestion) = closest(name, available.iter().copied()).filter(|name| nameable(name))
    {
        parts.push(format!(
            "The closest available command is /{suggestion}. Ask the user before running it."
        ));
    }
    parts.push(if listed.is_empty() {
        if more > 0 {
            format!(
                "This session has {more} slash {}, but none can be named here.",
                plural(more, "command")
            )
        } else {
            "No slash commands are available in this session.".to_owned()
        }
    } else {
        let names: Vec<String> = listed.iter().map(|name| format!("/{name}")).collect();
        let tail = if more > 0 {
            format!(", and {more} more")
        } else {
            String::new()
        };
        format!(
            "Slash commands available in this session: {}{tail}.",
            names.join(", ")
        )
    });
    parts.push("Treat the message as a plain request and do the task with the tools you have. If the task needs that command, tell the user it is not installed in this session. The user can add it as an organization plugin or a project skill. Do not give installation steps you are not sure of. Do not say the command ran.".to_owned());
    parts.join(" ")
}

/// 要不要加提醒：像命令名，且 `/<名字>` 不是执行环境里已有的路径（先 `stat` 它）。
pub(crate) async fn applies(files: &dyn Files, workspace: &WorkspaceRef, name: &str) -> bool {
    if !looks_like_command(name) {
        return false;
    }
    let request = StatFile {
        path: EnvPath::new(format!("/{name}")),
    };
    !matches!(files.stat(workspace, request).await, Ok(Some(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_names_are_recognized() {
        assert!(looks_like_command("nope"));
        assert!(looks_like_command("plugin:skill-x_1"));
        assert!(!looks_like_command("-x"));
        assert!(!looks_like_command("tmp/notes.md"));
        assert!(!looks_like_command("a.b"));
        assert_eq!(closest("lop", ["loop", "greet"]), Some("loop"));
        assert_eq!(closest("nope", ["loop", "greet"]), None);
    }
}
