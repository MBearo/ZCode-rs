//! 子代理的文本（docs/specs/rust-cli/agents.md §2–§5）：内置 agent 的提示词与清单描述、子代理
//! system prompt 的固定段落、前台结果、后台启动结果与完成通知。
//!
//! `prompts/agents/` 下的文件由 `scripts/rust-cli/prompts/agents.mjs` 生成并 `--check`；Plan 的提示词手工维护
//! （`plan.txt`）；fork 子代理的三个文件（`fork_directive.txt`、`fork_started.txt`、`fork_tool.json`，修复线 L-A）取自 TUI
//! 基线，手工维护。

use rust_cli_base::include_lf;

use crate::texts::render;
use crate::trim;

macro_rules! agent_texts {
    ($($name:ident => $file:literal,)*) => {
        $(
            #[doc = concat!("`prompts/agents/", $file, ".txt`")]
            pub fn $name() -> &'static str {
                trim(include_lf!(concat!("../../../prompts/agents/", $file, ".txt")))
            }
        )*

        #[cfg(test)]
        pub(crate) const ALL: &[(&str, fn() -> &'static str)] = &[$(($file, $name)),*];
    };
}

agent_texts! {
    general_purpose => "general_purpose",
    explore => "explore",
    plan => "plan",
    when_general_purpose => "when_general_purpose",
    when_explore => "when_explore",
    when_plan => "when_plan",
    authority => "authority",
    notes => "notes",
    handback => "handback",
    handback_trailer => "handback_trailer",
    launched => "launched",
    notification_preamble => "notification_preamble",
    notification => "notification",
    notification_result => "notification_result",
    notification_usage => "notification_usage",
    stranded_message => "stranded_message",
    fork_started => "fork_started",
}

/// fork 子代理收到的那段话（修复线 L-A，docs/specs/rust-cli/agents.md §8.1.2）：固定的
/// `<fork-boilerplate>` 段落之后是 `Your directive: <prompt>`。取自探测场景的基线（T-FORK），手工维护。
pub fn fork_directive(directive: &str) -> String {
    let raw = include_lf!("../../../prompts/agents/fork_directive.txt");
    render(trim(raw), &[("directive", directive)])
}

/// fork 打开时 Agent 工具的描述与 schema（agents.md §8.1.1）：`Agent.json` 换成对照实现的 fork 写法，与 T-S0 的 TUI 基线核对。
#[derive(Debug, serde::Deserialize)]
pub struct ForkTool {
    pub description: String,
    pub input_schema: serde_json::Value,
}

pub fn fork_tool() -> Option<&'static ForkTool> {
    static TOOL: std::sync::OnceLock<Option<ForkTool>> = std::sync::OnceLock::new();
    TOOL.get_or_init(|| {
        serde_json::from_str(include_lf!("../../../prompts/agents/fork_tool.json")).ok()
    })
    .as_ref()
}

/// 后台子代理完成通知的全文（`<system-reminder>` 包装）。`vars` 是 `notification.txt` 的变量；
/// `body` 是按需出现的 `<result>` 与 `<usage>`（各自前面一个换行）。
pub fn notification_text(vars: &[(&str, &str)]) -> String {
    notification_wrap(&render(notification(), vars))
}

/// 给一条或多条 `<task-notification>` 加上说明与 `<system-reminder>` 包装。多条时用空行连接、说明
/// 只出现一次（续跑停放的子代理时把同时到达的通知合成一条消息）。
pub fn notification_wrap(envelopes: &str) -> String {
    crate::reminder::wrap(&format!("{}{envelopes}", notification_preamble()))
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;
