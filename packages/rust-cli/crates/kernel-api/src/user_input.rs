//! 用户输入的 `@` 提及与 `!` bash 模式（docs/specs/rust-cli/tui-input.md §1.1、§2.2）。
//!
//! 唯一 owner 是工具能力（tools 实现 [`UserInputs`]，它有 Read 与 Bash 工具）；会话 actor 在回合任务里调用
//! 它，按结果决定消息 ID 并提交。这里只有形状与消息文字。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::message::{ContentBlock, UserMessage, UserOrigin};
use crate::tool::{ToolProgress, ToolUseContext};

/// 一条提及产生的 attachment（文字自带 `<system-reminder>` 包装；`source` 是 attachment type：
/// `file`、`directory`、`at_mention_reference`；`data` 是记录的字段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputAttachment {
    pub source: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

/// bash 模式一次执行的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum BashRun {
    /// 命令跑完（含表示失败的退出码）。两段已拼好并转义（`<persisted-output>` 预览不转义）。
    Completed { stdout: String, stderr: String },
    /// 执行本身出错（不是退出码）：原因原文，由 [`bash_output`] 转义。
    Failed { message: String },
    /// 被中断（esc / ctrl+c）。
    Interrupted,
}

#[async_trait]
pub trait UserInputs: Send + Sync {
    /// 普通输入里的 `@` 提及 → attachment，顺序见 tui-input.md §1.1。读文件经 Read 工具（同一个 readFileState），
    /// 判决链的规则拒绝的、不存在的、不是文本的跳过。
    async fn mentions(&self, input: &str, context: ToolUseContext) -> Vec<InputAttachment>;

    /// bash 模式：经 Bash 工具执行（不经判决链与 hooks，不用 sandbox；同一个 shell 状态与 cwd）。
    /// 取消令牌是本回合的中止信号。
    async fn bash(
        &self,
        command: &str,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> BashRun;
}

/// 没有输入能力（测试、嵌入）：不展开提及，bash 模式报错。
pub struct NoUserInputs;

#[async_trait]
impl UserInputs for NoUserInputs {
    async fn mentions(&self, _input: &str, _context: ToolUseContext) -> Vec<InputAttachment> {
        Vec::new()
    }

    async fn bash(
        &self,
        _command: &str,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> BashRun {
        BashRun::Failed {
            message: "bash mode is not available".to_owned(),
        }
    }
}

/// XML 转义：只换 `&` `<` `>`。
pub fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// bash 模式的输入消息（人类消息）。
pub fn bash_input(command: &str) -> String {
    format!("<bash-input>{command}</bash-input>")
}

/// bash 模式的输出消息（人类消息）；中断时没有（改发中断消息）。
pub fn bash_output(run: &BashRun) -> Option<String> {
    match run {
        BashRun::Completed { stdout, stderr } => Some(format!(
            "<bash-stdout>{stdout}</bash-stdout><bash-stderr>{stderr}</bash-stderr>"
        )),
        BashRun::Failed { message } => Some(format!(
            "<bash-stderr>Command failed: {}</bash-stderr>",
            escape_xml(message)
        )),
        BashRun::Interrupted => None,
    }
}

/// 中断时排在输入之前的 caveat（固定的文字）；meta 消息，来源是
/// [`crate::user_command::LOCAL_COMMAND_CAVEAT_SOURCE`]。
pub const BASH_CAVEAT: &str = "<local-command-caveat>The command below was run directly in ZCode, not sent to you as a request, and its output goes straight to the user. It's recorded here as context for later messages.</local-command-caveat>";

fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = start + text[start..].find(&close)?;
    Some(&text[start..end])
}

/// bash 模式的输入还原成用户输入的样子（`! 命令`；会话标题与首条提示同样还原）。
pub fn display(text: &str) -> Option<String> {
    if !text.starts_with("<bash-input>") {
        return None;
    }
    tag(text, "bash-input").map(|command| format!("! {command}"))
}

/// bash 模式的输出消息拆成 stdout 与 stderr（TUI 的输出行）；不是输出消息时为 None。
pub fn output_parts(text: &str) -> Option<(String, String)> {
    if !text.starts_with("<bash-stdout>") && !text.starts_with("<bash-stderr>") {
        return None;
    }
    let stdout = tag(text, "bash-stdout").unwrap_or_default().to_owned();
    let stderr = tag(text, "bash-stderr").unwrap_or_default().to_owned();
    Some((stdout, stderr))
}

/// 用户中断后补的那条 Human 消息的两种文字（普通中断与工具使用中的中断）。
pub const INTERRUPT_TEXTS: [&str; 2] = [
    "[Request interrupted by user]",
    "[Request interrupted by user for tool use]",
];

/// 只有一个文本块的消息的文字。
fn single_text(user: &UserMessage) -> Option<&str> {
    match user.content.as_slice() {
        [ContentBlock::Text { text }] => Some(text),
        _ => None,
    }
}

/// 中断后补的 Human 消息（存储按中断提示存，不开新的轮次）。
pub fn is_interrupt(user: &UserMessage) -> bool {
    single_text(user).is_some_and(|text| INTERRUPT_TEXTS.contains(&text))
}

/// bash 模式的输出消息（`<bash-stdout>` / `<bash-stderr>`，[`bash_output`]）：来源是 Human，但不是用户输入。
pub fn is_bash_output(user: &UserMessage) -> bool {
    single_text(user).is_some_and(|text| output_parts(text).is_some())
}

/// 开启新一轮的真实用户输入：Human 来源，不是中断提示，也不是 bash 模式的输出。存储按它划分轮次（Node
/// `semantics.origin = real_user`），会话状态的 owner 按它换当前轮（[`crate::store::TurnAnchor`]），两边用的是这一个判定。
pub fn opens_turn(user: &UserMessage) -> bool {
    user.origin == UserOrigin::Human && !is_interrupt(user) && !is_bash_output(user)
}

#[cfg(test)]
#[path = "user_input_tests.rs"]
mod tests;
