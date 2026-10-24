//! 发请求前：assistant 的块顺序（tool_use 移到末尾）与工具结果的配对兜底
//! （docs/specs/rust-cli/query.md §1.7、§1.8，context.md §3 第 11、12 条）。
use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::{ApiMessage, ApiRole};
use serde_json::json;

use super::*;

const OPTIONS: NormalizeOptions = NormalizeOptions {
    system_turns: true,
    turn_effort: None,
    skip_cache_write: false,
};

fn text(value: &str) -> ContentBlock {
    ContentBlock::Text {
        text: value.to_owned(),
    }
}

fn thinking(value: &str) -> ContentBlock {
    ContentBlock::Thinking {
        thinking: value.to_owned(),
        signature: Some("sig".to_owned()),
    }
}

fn call(id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: ToolUseId::new(id),
        name: "Read".to_owned(),
        input: json!({}),
    }
}

fn human(id: &str, value: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![text(value)],
        origin: UserOrigin::Human,
    })
}

fn assistant(id: &str, content: Vec<ContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content,
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn result(id: &str, owner: &str, calls: &[&str]) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: calls
            .iter()
            .map(|call| ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new(*call),
                content: vec![ToolResultContent::Text {
                    text: format!("out {call}"),
                }],
                is_error: false,
            })
            .collect(),
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new(owner),
        },
    })
}

/// 每块的简写：文字 `t:<文字>`、thinking `th:<文字>`、调用 `use:<id>`、结果 `res:<id>:<文字>[:err]`。
fn shape(message: &ApiMessage) -> Vec<String> {
    message
        .content
        .iter()
        .map(|item| match &item.block {
            ContentBlock::Text { text } => format!("t:{text}"),
            ContentBlock::Thinking { thinking, .. } => format!("th:{thinking}"),
            ContentBlock::ToolUse { id, .. } => format!("use:{id}"),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let body = match content.as_slice() {
                    [ToolResultContent::Text { text }] => text.clone(),
                    other => format!("{other:?}"),
                };
                format!(
                    "res:{tool_use_id}:{body}{}",
                    if *is_error { ":err" } else { "" }
                )
            }
            other => format!("{other:?}"),
        })
        .collect()
}

fn roles(api: &[ApiMessage]) -> Vec<ApiRole> {
    api.iter().map(|message| message.role).collect()
}

/// 回复里文字出现在 tool_use 之后：请求里 tool_use 移到末尾，文字块各自分开、顺序不变。以前实时路径保持 API 的
/// 块顺序（冷读取却是文字在前）。
#[test]
fn tool_uses_move_behind_the_text_that_follows_them() {
    let messages = [
        human("u1", "go"),
        assistant(
            "a1",
            vec![text("before"), call("t1"), text("after"), call("t2")],
        ),
        result("r1", "a1", &["t1", "t2"]),
    ];
    let api = normalize(&messages, &OPTIONS);
    assert_eq!(api[1].role, ApiRole::Assistant);
    assert_eq!(shape(&api[1]), ["t:before", "t:after", "use:t1", "use:t2"]);
}

/// tool_use 夹在两个 thinking 块之间时整条不动。
#[test]
fn a_tool_use_between_two_thinking_blocks_stays_in_place() {
    let messages = [
        human("u1", "go"),
        assistant(
            "a1",
            vec![thinking("one"), call("t1"), thinking("two"), text("after")],
        ),
        result("r1", "a1", &["t1"]),
    ];
    let api = normalize(&messages, &OPTIONS);
    assert_eq!(shape(&api[1]), ["th:one", "use:t1", "th:two", "t:after"]);
}

/// 缺结果的调用补一条错误结果，排在已有结果之前。以前请求里留着不配对的 tool_use。
#[test]
fn a_missing_result_is_filled_in_first() {
    let messages = [
        human("u1", "go"),
        assistant("a1", vec![call("t1"), call("t2")]),
        result("r2", "a1", &["t2"]),
    ];
    let api = normalize(&messages, &OPTIONS);
    assert_eq!(
        roles(&api),
        [ApiRole::User, ApiRole::Assistant, ApiRole::User]
    );
    assert_eq!(
        shape(&api[2]),
        [
            "res:t1:[Tool result missing due to internal error]:err",
            "res:t2:out t2"
        ]
    );
}

/// 回复之后没有 user 消息时，缺的结果另起一条 user 消息。
#[test]
fn results_missing_at_the_end_get_their_own_message() {
    let messages = [
        human("u1", "go"),
        assistant("a1", vec![text("x"), call("t1")]),
    ];
    let api = normalize(&messages, &OPTIONS);
    assert_eq!(
        roles(&api),
        [ApiRole::User, ApiRole::Assistant, ApiRole::User]
    );
    assert_eq!(
        shape(&api[2]),
        ["res:t1:[Tool result missing due to internal error]:err"]
    );
}

/// 不属于前一条回复的结果与重复的结果去掉；不跟在回复后面的结果去掉，剩下的文字留着。
#[test]
fn orphaned_and_duplicate_results_are_dropped() {
    let messages = [
        result("r0", "a0", &["t0"]),
        human("u1", "go"),
        assistant("a1", vec![call("t1")]),
        result("r1", "a1", &["t1", "t1", "t9"]),
    ];
    let api = normalize(&messages, &OPTIONS);
    assert_eq!(
        roles(&api),
        [ApiRole::User, ApiRole::Assistant, ApiRole::User]
    );
    assert_eq!(shape(&api[0]), ["t:go"]);
    assert_eq!(shape(&api[2]), ["res:t1:out t1"]);
}

/// 第一条就是只有孤立结果的消息时换成说明文字；同一个 tool_use 在后面再出现时去掉，回复空了放一个说明块。
#[test]
fn a_leading_orphan_and_a_repeated_tool_use_are_replaced() {
    let messages = [
        result("r0", "a0", &["t0"]),
        assistant("a1", vec![call("t1")]),
        result("r1", "a1", &["t1"]),
        assistant("a2", vec![call("t1")]),
    ];
    let api = normalize(&messages, &OPTIONS);
    assert_eq!(
        shape(&api[0]),
        ["t:[Orphaned tool result removed due to conversation resume]"]
    );
    assert_eq!(shape(&api[3]), ["t:[Tool use interrupted]"]);
}
