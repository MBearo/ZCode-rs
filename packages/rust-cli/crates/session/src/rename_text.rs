//! 不带参数的 `/rename` 的文本处理与请求（docs/specs/rust-cli/titles.md §8.3–§8.5）：对话文本、JSON 解析与
//! 名字的清理。

use rust_cli_kernel_api::history;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, ModelRequest, ModelSelection, OutputFormat, QuerySource,
    RequestOptions, SystemBlock,
};
use rust_cli_prompts::texts::{identity, render};
use rust_cli_prompts::{models, title};
use serde_json::Value;

/// 请求来源（`querySource`）。
const QUERY_SOURCE: &str = "rename_generate_name";
/// 对话文本只留末尾这么多 UTF-16 码元。
const TAIL_UNITS: usize = 1000;
/// 名字最多这么多个码点。
const MAX_NAME_CHARS: usize = 200;
/// 模型卡片里没有的模型用的输出上限（与主循环 `query::run` 相同，prompts 的一个常量，F58）。
const DEFAULT_MAX_TOKENS: u32 = rust_cli_prompts::models::UNKNOWN_MAX_OUTPUT_TOKENS;
/// 活动历史里的对话文本：user（去掉 meta）与 assistant 的 text 块，`\n` 连接，只留末尾 1000 个 UTF-16 码元。
pub(crate) fn conversation(messages: &[Message]) -> String {
    let mut parts = Vec::new();
    for message in history::active(messages) {
        let blocks = match message.as_ref() {
            // 跳过 `isMeta` 与非人类 `origin` 的 user；工具结果与压缩摘要没有非人类的 origin。
            Message::User(user) if !matches!(user.origin, UserOrigin::Meta { .. }) => &user.content,
            Message::Assistant(assistant) => &assistant.content,
            Message::User(_) | Message::Attachment(_) | Message::System(_) => continue,
        };
        parts.extend(blocks.iter().filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        }));
    }
    tail(&parts.join("\n"))
}

/// 末尾 1000 个 UTF-16 码元；截在代理对中间时丢掉那个字符（titles.md D10）。
fn tail(text: &str) -> String {
    let mut units = 0;
    let mut start = text.len();
    for (index, c) in text.char_indices().rev() {
        units += c.len_utf16();
        if units > TAIL_UNITS {
            break;
        }
        start = index;
    }
    text[start..].to_owned()
}

/// 去掉代码围栏，按 JSON 解析，是对象且 `name` 是字符串才取。
pub(crate) fn parse_name(reply: &str) -> Option<String> {
    let mut text = reply.trim();
    if let Some(fenced) = text.strip_prefix("```") {
        text = fenced
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .trim_start();
    }
    if let Some(open) = text.strip_suffix("```") {
        text = open.trim_end();
    }
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    value.get("name")?.as_str().map(str::to_owned)
}

/// 名字的清理：去首尾空白，连续的控制 / 格式字符与 U+2028、U+2029 换成一个空格，只留前 200 个码点，再去首尾
/// 空白；为空时 None。
pub(crate) fn clean_name(name: &str) -> Option<String> {
    let mut spaced = String::with_capacity(name.len());
    let mut in_run = false;
    for c in name.trim().chars() {
        let invisible = c.is_control() || is_format(c) || c == '\u{2028}' || c == '\u{2029}';
        if invisible {
            if !in_run {
                spaced.push(' ');
            }
        } else {
            spaced.push(c);
        }
        in_run = invisible;
    }
    let cut: String = spaced.chars().take(MAX_NAME_CHARS).collect();
    let cleaned = cut.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

/// Unicode 的格式字符（General_Category = Cf）。
fn is_format(c: char) -> bool {
    matches!(
        u32::from(c),
        0xAD | 0x600..=0x605
            | 0x61C
            | 0x6DD
            | 0x70F
            | 0x890..=0x891
            | 0x8E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// 起名请求（titles.md §8.4）：身份句与起名提示、一条 user，没有工具与 thinking；输出上限与 effort 按模型
/// 卡片（与主循环相同），structured output 用起名的 JSON schema。
/// `session`：发请求的会话（工作线 F52：`metadata.user_id` 的 `session_id`，model.md §3.3）。
pub(crate) fn request(
    selection: &ModelSelection,
    session: &SessionId,
    conversation: &str,
) -> ModelRequest {
    let card = models::card(&selection.model);
    let block = |text: &str| SystemBlock {
        text: text.to_owned(),
        cache_control: None,
    };
    let user = render(title::rename_user(), &[("conversation", conversation)]);
    ModelRequest {
        selection: selection.clone(),
        system: vec![block(identity()), block(title::rename_system())],
        messages: vec![ApiMessage {
            role: ApiRole::User,
            content: vec![ApiContent {
                block: ContentBlock::Text { text: user },
                cache_control: None,
            }],
            output_config: None,
        }],
        tools: Vec::new(),
        tool_choice: None,
        max_tokens: card.map_or(DEFAULT_MAX_TOKENS, |card| card.max_output_tokens),
        thinking: None,
        temperature: None,
        betas: Vec::new(),
        source: QuerySource::Side(QUERY_SOURCE.to_owned()),
        options: RequestOptions {
            // 修 bug（F23）：没有卡片的模型以前也写选择的档位（`card_effort` 的注释）。
            effort: models::card_effort(&selection.model, selection.reasoning.as_deref()),
            output_format: serde_json::from_value::<OutputFormat>(title::rename_format()).ok(),
            session_id: Some(session.clone()),
            ..RequestOptions::default()
        },
    }
}

#[cfg(test)]
#[path = "rename_text_tests.rs"]
mod tests;
