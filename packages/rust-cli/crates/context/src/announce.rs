//! 模型身份、日期、剩余 token 与署名 attachment。

use rust_cli_kernel_api::goal;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::{user_command, user_input};
use rust_cli_prompts::models;
use rust_cli_prompts::texts::{self, render};
use serde_json::{Value, json};

use crate::config::Attribution;
use crate::render::attachments;
use crate::system::{TOTAL_TOKENS_BUDGET, total_tokens};

pub(crate) const MODEL: &str = "model";
pub(crate) const DATE: &str = "date";
pub(crate) const TOTAL_TOKENS: &str = "total_tokens_reminder";
/// Skill 正文消息的来源（ext 的 `SKILL_MESSAGE_SOURCE`）。
const SKILL_SOURCE: &str = "skill";
pub(crate) const ATTRIBUTION: &str = "remote_session_change";

/// 模型身份段：与压缩边界之后最近一次宣布的身份不同才发。
pub(crate) fn model(history: &[Message], selection: &ModelSelection) -> Option<(String, Value)> {
    let card = models::card(&selection.model);
    let name = card.and_then(|card| card.marketing_name.as_deref());
    let cutoff = card.and_then(|card| card.knowledge_cutoff.as_deref());
    let identity = json!({
        "modelId": selection.model,
        "marketingName": name,
        "knowledgeCutoff": cutoff,
    });
    let last = attachments(history, MODEL).next_back();
    if last.is_some_and(|attachment| attachment.data.get("identity") == Some(&identity)) {
        return None;
    }
    let mut sentences = vec![match name {
        Some(name) => render(texts::model(), &[("name", name), ("id", &selection.model)]),
        None => render(texts::model_unnamed(), &[("id", &selection.model)]),
    }];
    if let Some(cutoff) = cutoff {
        sentences.push(render(texts::model_cutoff(), &[("cutoff", cutoff)]));
    }
    Some((sentences.join(" "), json!({ "identity": identity })))
}

/// 日期：第一次说今天的日期，之后只在日期变了时说“日期变了”。
pub(crate) fn date(history: &[Message], today: &str) -> Option<(String, Value)> {
    let last = attachments(history, DATE)
        .next_back()
        .and_then(|attachment| attachment.data.get("date").and_then(Value::as_str));
    match last {
        Some(date) if date == today => None,
        Some(_) => Some((
            render(texts::date_changed(), &[("date", today)]),
            json!({"date": today, "changed": true}),
        )),
        None => Some((
            render(texts::date(), &[("date", today)]),
            json!({"date": today}),
        )),
    }
}

/// 最近一次模型响应占用的上下文。
fn context_tokens(messages: &[Message]) -> u64 {
    messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(assistant) => assistant.usage,
            _ => None,
        })
        .map_or(0, |usage| {
            usage.input_tokens
                + usage.cache_creation_input_tokens
                + usage.cache_read_input_tokens
                + usage.output_tokens
        })
}

/// 剩余 token（padded-countdown）：每条人类输入重新锚定，之后按上下文的增长扣减，只减不增。
/// 不用会话内的计数器，从历史重算：resume 后结果相同。
pub(crate) fn tokens_left(history: &[Message]) -> u64 {
    let anchor_at = history.iter().rposition(
        |message| matches!(message, Message::User(user) if user.origin == UserOrigin::Human),
    );
    let Some(anchor_at) = anchor_at else {
        return TOTAL_TOKENS_BUDGET;
    };
    tokens_left_since(history, anchor_at)
}

/// 从 `anchor_at` 起按上下文的增长扣减，只减不增。
fn tokens_left_since(history: &[Message], anchor_at: usize) -> u64 {
    let anchor = context_tokens(&history[..anchor_at]);
    let mut used = 0u64;
    for (index, message) in history.iter().enumerate().skip(anchor_at) {
        if matches!(message, Message::Assistant(_)) {
            used = used.max(context_tokens(&history[..=index]).saturating_sub(anchor));
        }
    }
    TOTAL_TOKENS_BUDGET.saturating_sub(used)
}

/// 这一步是否宣布剩余 token：最后一条非 attachment 消息是人类输入或工具结果时（普通的用户输入，
/// 或回合中途没有新输入）。
/// 通知等投递开启的回合、恢复重试的提示与 Stop hook 的反馈之后不宣布。
pub(crate) fn tokens_due(history: &[Message]) -> bool {
    // 修 bug（L17）：Skill 的正文是排在 tool_result 之后的 meta user 消息（同一批新消息），以前把它当作
    // “最后一条”，工具结果之后就不宣布了。它算在这次工具结果里，照常宣布，
    // 所以跳过它往前看。
    let last = history.iter().rev().find(|message| {
        !matches!(message, Message::Attachment(_))
            && !matches!(message, Message::User(user)
                if matches!(&user.origin, UserOrigin::Meta { source } if source == SKILL_SOURCE))
    });
    match last {
        // 修 bug（L22）：用户调用 skill 时人类输入是命令标签，不算普通的用户输入，这一步不宣布；
        // 以前按普通输入宣布了。
        Some(Message::User(user)) if user.origin == UserOrigin::Human => !user.content.iter().any(
            |block| matches!(block, ContentBlock::Text { text } if user_command::display(text).is_some()),
        ),
        Some(Message::User(user)) => matches!(user.origin, UserOrigin::ToolResult { .. }),
        _ => false,
    }
}

/// Stop hook 阻塞反馈的 meta 来源（hooks 的 `stop_feedback`）。
const STOP_FEEDBACK_SOURCE: &str = "stop_hook_feedback";

/// 这一步是否算当步 attachment 那一组（环境段、模型身份、清单、nested memory、计划模式、
/// 剩余 token 等，docs/specs/rust-cli/goal.md §8、context.md §4）。只在用户输入与工具结果之后算：本地命令
/// `/goal` 的说明（meta `goal`）与 Stop hook 阻塞后的反馈之后都不算；bash 模式的输出之后也不算。
///
/// 修 bug（修复线 F9）：bash 模式的回合执行完命令后直接返回，不组装
/// 当步 attachment；原来按普通输入宣布，环境段与清单提前到了 bash 回合，下一条普通输入只剩剩余 token。
pub(crate) fn inputs_due(history: &[Message]) -> bool {
    let last = history
        .iter()
        .rev()
        .find(|message| !matches!(message, Message::Attachment(_)));
    match last {
        Some(Message::User(user)) => match &user.origin {
            UserOrigin::Meta { source } => {
                source != goal::PROMPT_SOURCE && source != STOP_FEEDBACK_SOURCE
            }
            UserOrigin::Human => !user.content.iter().any(|block| {
                matches!(block, ContentBlock::Text { text } if user_input::output_parts(text).is_some())
            }),
            _ => true,
        },
        _ => true,
    }
}

/// 剩余 token 提醒的文本（是否宣布见 [`tokens_due`]）。
pub(crate) fn tokens(history: &[Message]) -> (String, Value) {
    (total_tokens(tokens_left(history)), Value::Null)
}

/// 子代理的剩余 token 提醒（修复线 F16，agents.md §4.3）：不重新锚定。只在主线程的人类输入那一步
/// 重新锚定，子代理的计数器（按 agentId）从它第一次提醒起只看历次响应占用的上下文，取最大值；
/// SendMessage 续跑的消息不让它从头算。
pub(crate) fn subagent_tokens(history: &[Message]) -> (String, Value) {
    (total_tokens(tokens_left_since(history, 0)), Value::Null)
}

/// 署名提醒：内容与上次宣布的不同才发。第一次宣布要求有署名，且模型能用 Bash 提交
/// （署名宣布与 git 说明的开关默认开，再看 Bash 工具）。
pub(crate) fn attribution(
    history: &[Message],
    attribution: &Attribution,
    has_bash: bool,
) -> Option<(String, Value)> {
    let data = json!({"commit": attribution.commit, "pr": attribution.pr});
    let last = attachments(history, ATTRIBUTION).next_back();
    match last {
        Some(last) if last.data == data => return None,
        None if !has_bash || (attribution.commit.is_none() && attribution.pr.is_none()) => {
            return None;
        }
        _ => {}
    }
    let mut lines = Vec::new();
    if let Some(commit) = &attribution.commit {
        lines.push(render(texts::attribution_commit(), &[("value", commit)]));
    }
    if let Some(pr) = &attribution.pr {
        lines.push(render(texts::attribution_pr(), &[("value", pr)]));
    }
    let text = if lines.is_empty() {
        texts::attribution_none().to_owned()
    } else {
        render(texts::attribution(), &[("lines", &lines.join("\n"))])
    };
    Some((text, data))
}

#[cfg(test)]
#[path = "announce_tests.rs"]
mod tests;
