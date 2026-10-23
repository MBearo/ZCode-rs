//! 子代理的文本（docs/specs/rust-cli/agents.md §2–§5）：内置 agent 的提示词与清单描述、子代理
//! system prompt 的固定段落、前台结果、后台启动结果与完成通知。
//!
//! `prompts/agents/` 下的文件由 `scripts/rust-cli/prompts/agents.mjs` 生成并 `--check`；Plan 的提示词手工维护
//! （`plan.txt`）。

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
