//! 用户输入的斜杠命令：`/<skill 名> [参数]` 的展开（docs/specs/rust-cli/ext.md §2.7；prompt 命令的分支）。
//!
//! 展开的唯一 owner 是 skill 能力（ext 实现 [`UserCommands`]）；会话 actor 在回合任务里调用它，按结果提交
//! 消息。这里只有形状：展开给出的是文字与种类，消息 ID 与提交由会话决定。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::env::WorkspaceRef;
use crate::tool::ToolUseContext;

/// `user-invocable: false` 的结果消息的来源（meta，TUI 显示为用户行下面的一行 dim 文字）。
pub const LOCAL_COMMAND_SOURCE: &str = "local-command";
/// 本地命令之前的 caveat 消息的来源（meta，不显示）。
pub const LOCAL_COMMAND_CAVEAT_SOURCE: &str = "local-command-caveat";

/// 用户可调用的命令（TUI 的斜杠补全）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserCommand {
    /// 调用名（skill 名；插件 skill 是 `<插件名>:<名字>`）。
    pub name: String,
    pub description: String,
    /// frontmatter 的 `argument-hint`：输入 `/<名字> ` 之后显示的参数提示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    /// 不在补全里列出（`user-invocable: false`），但仍是命令：用户输入时会话给出提示。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// 展开出的一条消息的种类。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExpandedKind {
    /// 用户输入：这一回合的 prompt 消息（会话用预先生成的 prompt 消息 ID）。
    Input,
    /// 只给模型看的消息；`source` 是来源标签（skill 正文是 `skill`）。
    Meta { source: String },
    /// attachment（文字自带 `<system-reminder>` 包装）；`source` 是 attachment type（`-p` 下未知命令的
    /// `unknown_command_fallback`，ext.md §2.7）。排在用户输入之后提交，发给模型前随 attachment 规则上浮。
    Attachment { source: String },
}

/// 展开出的一条消息：文字与种类。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedMessage {
    pub text: String,
    #[serde(flatten)]
    pub kind: ExpandedKind,
}

/// 展开的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Expansion {
    /// 不是可调用的命令（未知名字、`/` 开头的路径）：原文按普通输入发给模型（`-p` 的行为）。
    NotCommand,
    /// 展开成的消息，按提交顺序；`query` 为假时只提交、不跑 UserPromptSubmit、不调模型
    /// （`user-invocable: false`）。
    Messages {
        messages: Vec<ExpandedMessage>,
        query: bool,
    },
}

#[async_trait]
pub trait UserCommands: Send + Sync {
    /// 这个工作区里的命令，按 skill 的优先级顺序（ext.md §2.1）；`user-invocable: false` 的标为 `hidden`。
    async fn commands(&self, workspace: &WorkspaceRef) -> Vec<UserCommand>;

    /// 展开一条以 `/` 开头的输入。`context` 是 `!` 命令经 Bash 与判决链执行时用的工具上下文
    /// （取消令牌是本回合的中止信号）。
    async fn expand(&self, input: &str, context: ToolUseContext) -> Expansion;
}

/// 没有斜杠命令的能力（测试、嵌入）：没有命令，一切输入照常发送。
pub struct NoUserCommands;

#[async_trait]
impl UserCommands for NoUserCommands {
    async fn commands(&self, _workspace: &WorkspaceRef) -> Vec<UserCommand> {
        Vec::new()
    }

    async fn expand(&self, _input: &str, _context: ToolUseContext) -> Expansion {
        Expansion::NotCommand
    }
}

/// 用户调用的命令写进对话的标签（`<command-message>` / `<command-name>` / `<command-args>`）。
pub fn command_tags(name: &str, args: &str) -> String {
    let mut text =
        format!("<command-message>{name}</command-message>\n<command-name>/{name}</command-name>");
    if !args.is_empty() {
        text.push_str(&format!("\n<command-args>{args}</command-args>"));
    }
    text
}

fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = start + text[start..].find(&close)?;
    Some(&text[start..end])
}

/// 展开失败时写进对话的命令输入（标签顺序与 [`command_tags`] 不同，续行的缩进固定）。
pub fn command_input(name: &str, args: &str) -> String {
    format!(
        "<command-name>/{name}</command-name>\n            <command-message>{name}</command-message>\n            <command-args>{args}</command-args>"
    )
}

/// 标签还原成用户输入的样子：`/<名字>[ <参数>]`（TUI 的用户行、`/rewind` 放回的文字、会话标题的输入；
/// 首条提示同样把 `<command-name>` 还原）。不是命令标签时为 None。
pub fn display(text: &str) -> Option<String> {
    if !text.starts_with("<command-message>") && !text.starts_with("<command-name>") {
        return None;
    }
    let name = tag(text, "command-name")?;
    match tag(text, "command-args").map(str::trim) {
        Some(args) if !args.is_empty() => Some(format!("{name} {args}")),
        _ => Some(name.to_owned()),
    }
}

#[cfg(test)]
#[path = "user_command_tests.rs"]
mod tests;
