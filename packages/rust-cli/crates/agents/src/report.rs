//! 给模型看的子代理文本：system prompt、前台结果、后台启动结果、完成通知（agents.md §2、§4.3、§5）。
//! 文本都在 `prompts/agents/`，这里只拼接。

use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock, Usage};
use rust_cli_kernel_api::model::{CacheControl, SystemBlock};
use rust_cli_prompts::agents as texts;
use rust_cli_prompts::texts::{self as context_texts, render};

/// `<total_tokens>` 的预算（padded-countdown 的默认值；与 context 的主线程相同）。
const TOTAL_TOKENS_BUDGET: &str = "15000000";

/// 子代理没有输出时的报告。
pub(crate) const EMPTY_REPORT: &str = "(Subagent completed but returned no output.)";

fn ephemeral(text: String) -> SystemBlock {
    SystemBlock {
        text,
        cache_control: Some(CacheControl {
            kind: "ephemeral".to_owned(),
            ttl: None,
            scope: None,
        }),
    }
}

/// 子代理的 system prompt（`system[1..]`）：身份句；agent 提示词、来自父 agent
/// 的消息说明、Notes 与 `<total_tokens>`，空行连接。
pub(crate) fn system(prompt: &str) -> Vec<SystemBlock> {
    let tokens = render(
        context_texts::total_tokens(),
        &[("tokens", TOTAL_TOKENS_BUDGET)],
    );
    let body = [prompt, texts::authority(), texts::notes(), &tokens]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    vec![
        ephemeral(context_texts::identity().to_owned()),
        ephemeral(body),
    ]
}

/// 子代理最后一条 assistant 消息的文本块（空行连接）。
pub(crate) fn final_text(last: Option<&AssistantMessage>) -> String {
    let Some(last) = last else {
        return String::new();
    };
    last.content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 用量：最后一次响应的输入（含缓存读写）加输出。
pub(crate) fn tokens(usage: Option<Usage>) -> u64 {
    usage.map_or(0, |usage| {
        usage.input_tokens
            + usage.cache_creation_input_tokens
            + usage.cache_read_input_tokens
            + usage.output_tokens
    })
}

/// 子代理一次运行的结果摘要。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) text: String,
    pub(crate) tokens: u64,
    pub(crate) tool_uses: usize,
    pub(crate) duration_ms: u64,
}

/// 前台结果（A1、A4）：hand-back 说明、每行缩进两格的报告、可选的 agentId 与用量尾注。
pub(crate) fn handback(agent: &str, report: &Report, trailer: bool) -> String {
    let body = if report.text.trim().is_empty() {
        EMPTY_REPORT
    } else {
        report.text.as_str()
    };
    let indented = body
        .split('\n')
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut text = format!("{}\n{indented}", texts::handback());
    if trailer {
        let tokens = report.tokens.to_string();
        let tool_uses = report.tool_uses.to_string();
        let duration = report.duration_ms.to_string();
        text.push('\n');
        text.push_str(&render(
            texts::handback_trailer(),
            &[
                ("id", agent),
                ("tokens", &tokens),
                ("tool_uses", &tool_uses),
                ("duration", &duration),
            ],
        ));
    }
    text
}

/// 后台启动结果（A2，去掉 `output_file` 两行）。
pub(crate) fn launched(agent: &str) -> String {
    render(texts::launched(), &[("id", agent)])
}

/// 后台子代理的终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    Completed,
    Failed,
    /// 被 TaskStop 停止（后台子代理只有这一个中止来源；`killedBy: "parent"`）。
    Killed,
    /// 停放时被 TaskStop 停止（`killedBy` 缺省为 `"user"`，通知带最后的报告与用量）。
    KilledParked,
}

impl Ending {
    pub(crate) fn status(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Killed | Self::KilledParked => "killed",
        }
    }

    /// `<summary>` 里描述之后的部分。停止者写产品名 `ZCode`（与 Cron 文字相同，agents.md §5）。
    fn summary(self, error: Option<&str>) -> String {
        match self {
            Self::Completed => "finished".to_owned(),
            Self::Failed => format!(
                "failed: {}",
                error
                    .filter(|error| !error.is_empty())
                    .unwrap_or("Unknown error")
            ),
            Self::Killed => "was stopped by ZCode".to_owned(),
            Self::KilledParked => "was stopped by user".to_owned(),
        }
    }
}

/// `<summary>` 与 `<result>` 里的 `&`、`<`、`>` 转义。
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 完成通知的 `<task-notification>` 信封（不带说明与包装）。`<result>` 只在有最后的文本时出现，
/// `<usage>` 只在完成（与停放时被停止）时出现（A2、A5；agents.md §5、§5.1）。`error` 是失败的原因。
pub(crate) fn envelope(
    agent: &str,
    tool_use: Option<&str>,
    description: &str,
    ending: Ending,
    report: &Report,
    error: Option<&str>,
) -> String {
    let summary = escape(&format!(
        "Agent \"{description}\" {}",
        ending.summary(error)
    ));
    let mut body = String::new();
    if !report.text.is_empty() {
        body.push('\n');
        body.push_str(&render(
            texts::notification_result(),
            &[("result", &escape(&report.text))],
        ));
    }
    // 用量：完成时有；停放时被停止也有，运行中失败或被停止时没有。
    if matches!(ending, Ending::Completed | Ending::KilledParked) {
        let tokens = report.tokens.to_string();
        let tool_uses = report.tool_uses.to_string();
        let duration = report.duration_ms.to_string();
        body.push('\n');
        body.push_str(&render(
            texts::notification_usage(),
            &[
                ("tokens", &tokens),
                ("tool_uses", &tool_uses),
                ("duration", &duration),
            ],
        ));
    }
    let text = render(
        texts::notification(),
        &[
            ("id", agent),
            ("tool_use", tool_use.unwrap_or_default()),
            ("status", ending.status()),
            ("summary", &summary),
            ("body", &body),
        ],
    );
    // 没有工具调用时不写 `<tool-use-id>` 这一行（续跑的通知）。
    match tool_use {
        Some(_) => text,
        None => text.replace("\n<tool-use-id></tool-use-id>", ""),
    }
}

/// 完成通知的全文（A2、A5）：信封加说明与 `<system-reminder>` 包装（运行时由 `routing` 按投递位置
/// 包装信封）。
#[cfg(test)]
pub(crate) fn notification(
    agent: &str,
    tool_use: Option<&str>,
    description: &str,
    ending: Ending,
    report: &Report,
    error: Option<&str>,
) -> String {
    texts::notification_wrap(&envelope(
        agent,
        tool_use,
        description,
        ending,
        report,
        error,
    ))
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg(feature = "parity")]
#[rustfmt::skip]
#[path = "report_parity_tests.rs"]
mod parity_tests;
