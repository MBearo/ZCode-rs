use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, SystemMessage, ToolResultContent,
    UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::ApiRole;
use serde_json::Value;

use super::*;

const ON: NormalizeOptions = NormalizeOptions {
    system_turns: true,
    turn_effort: None,
    skip_cache_write: false,
};
const OFF: NormalizeOptions = NormalizeOptions {
    system_turns: false,
    turn_effort: None,
    skip_cache_write: false,
};

fn text(value: &str) -> ContentBlock {
    ContentBlock::Text {
        text: value.to_owned(),
    }
}

fn human(id: &str, value: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![text(value)],
        origin: UserOrigin::Human,
    })
}

fn attachment(id: &str, source: &str, body: &str) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new(id),
        source: source.to_owned(),
        text: reminder::wrap(body),
        data: Value::Null,
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

fn texts(message: &ApiMessage) -> Vec<String> {
    message
        .content
        .iter()
        .map(|item| match &item.block {
            ContentBlock::Text { text } => text.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

/// 第一个回合的形状：署名（user 角色）排在人类输入前，补一个换行；system 角色的
/// attachment 去包装、空行连接，成为紧跟其后的 system 消息，并带最后的缓存断点。
#[test]
fn a_first_turn_folds_the_attachments_after_the_input() {
    let messages = [
        human("u1", "Say hello."),
        attachment("a1", "environment", "# Environment"),
        attachment("a2", "model", "You are powered by the model m."),
        attachment("a3", "date", "Today's date is 2026-10-17."),
        attachment("a4", "remote_session_change", "Attribution"),
    ];
    let api = normalize(&messages, &ON);
    assert_eq!(api.len(), 2);
    assert_eq!(api[0].role, ApiRole::User);
    assert_eq!(
        texts(&api[0]),
        [
            "<system-reminder>\nAttribution\n</system-reminder>\n",
            "Say hello."
        ]
    );
    assert_eq!(api[1].role, ApiRole::System);
    assert_eq!(
        texts(&api[1]),
        ["# Environment\n\nYou are powered by the model m.\n\nToday's date is 2026-10-17."]
    );
    assert!(
        api[0]
            .content
            .iter()
            .all(|item| item.cache_control.is_none())
    );
    assert_eq!(
        api[1].content[0]
            .cache_control
            .as_ref()
            .map(|cache| cache.kind.as_str()),
        Some("ephemeral")
    );
}

#[test]
fn without_system_turns_reminders_stay_in_the_user_message() {
    let messages = [
        human("u1", "hi"),
        attachment("a1", "date", "Today's date is 2026-10-17."),
    ];
    let api = normalize(&messages, &OFF);
    assert_eq!(api.len(), 1);
    assert_eq!(
        texts(&api[0]),
        [
            "<system-reminder>\nToday's date is 2026-10-17.\n</system-reminder>\n",
            "hi"
        ]
    );
}

/// attachment 不越过工具结果：工具循环里的提醒留在结果之后，折叠成结果后面的 system 消息。
#[test]
fn attachments_stop_at_tool_results_and_assistants() {
    let tool_use = ToolUseId::new("toolu_1");
    let messages = [
        human("u1", "go"),
        attachment("a1", "date", "d1"),
        assistant(
            "m1",
            vec![ContentBlock::ToolUse {
                id: tool_use.clone(),
                name: "Read".to_owned(),
                input: Value::Null,
            }],
        ),
        Message::User(UserMessage {
            id: MessageId::new("u2"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use,
                content: vec![ToolResultContent::Text {
                    text: "ok".to_owned(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("m1"),
            },
        }),
        attachment("a2", "total_tokens_reminder", "t2"),
        Message::System(SystemMessage::Interrupted {
            id: MessageId::new("s1"),
        }),
    ];
    let api = normalize(&messages, &ON);
    let roles: Vec<ApiRole> = api.iter().map(|message| message.role).collect();
    assert_eq!(
        roles,
        [
            ApiRole::User,
            ApiRole::System,
            ApiRole::Assistant,
            ApiRole::User,
            ApiRole::System
        ]
    );
    assert_eq!(texts(&api[1]), ["d1"]);
    assert_eq!(texts(&api[4]), ["t2"]);
}

#[test]
fn folded_text_without_a_user_message_falls_back_to_a_wrapped_reminder() {
    let messages = [
        assistant("m1", vec![text("hello")]),
        attachment("a1", "date", "d1"),
    ];
    let api = normalize(&messages, &ON);
    assert_eq!(api.len(), 2);
    assert_eq!(api[1].role, ApiRole::User);
    assert_eq!(
        texts(&api[1]),
        ["<system-reminder>\nd1\n</system-reminder>"]
    );
}

#[test]
fn adjacent_user_messages_join_with_a_newline_and_keep_tool_results_first() {
    let messages = [human("u1", "a"), human("u2", "b")];
    let api = normalize(&messages, &ON);
    assert_eq!(texts(&api[0]), ["a\n", "b"]);
}

#[test]
fn the_turn_effort_rides_on_the_system_turn_after_the_first_prompt() {
    let options = NormalizeOptions {
        system_turns: true,
        turn_effort: Some("medium".to_owned()),
        skip_cache_write: false,
    };
    let messages = [
        human("u1", "a"),
        attachment("a1", "date", "d1"),
        assistant("m1", vec![text("b")]),
        human("u2", "c"),
    ];
    let api = normalize(&messages, &options);
    let configs: Vec<Option<&str>> = api
        .iter()
        .map(|message| {
            message
                .output_config
                .as_ref()
                .and_then(|config| config.effort.as_deref())
        })
        .collect();
    assert_eq!(configs, [None, Some("medium"), None, None]);

    // 后面没有 system 消息时，插一条只带 output_config 的 system 消息。
    let api = normalize(&[human("u1", "a")], &options);
    assert_eq!(api.len(), 2);
    assert_eq!(api[1].role, ApiRole::System);
    assert!(api[1].content.is_empty());
    assert!(api[0].content[0].cache_control.is_some());
}

/// 只供显示的 attachment（文本为空，如 `max_turns_reached`）不发给模型。
#[test]
fn display_only_attachments_are_left_out() {
    let mut display = attachment("a1", "max_turns_reached", "");
    if let Message::Attachment(attachment) = &mut display {
        attachment.text.clear();
    }
    let api = normalize(&[human("u1", "hi"), display], &ON);
    assert_eq!(api.len(), 1);
    assert_eq!(texts(&api[0]), ["hi"]);
}

fn marked(api: &[ApiMessage]) -> Vec<(usize, usize)> {
    api.iter()
        .enumerate()
        .flat_map(|(index, message)| {
            message
                .content
                .iter()
                .enumerate()
                .filter(|(_, item)| item.cache_control.is_some())
                .map(move |(block, _)| (index, block))
        })
        .collect()
}

/// 摘要请求（不写缓存）：断点跳过末尾的 system 消息与最后一条 user，落在 Read 调用上。
#[test]
fn a_summary_request_marks_the_message_before_the_last() {
    let options = NormalizeOptions {
        skip_cache_write: true,
        ..ON
    };
    let call = ContentBlock::ToolUse {
        id: ToolUseId::new("t1"),
        name: "Read".to_owned(),
        input: serde_json::json!({"file_path": "/a"}),
    };
    let result = Message::User(UserMessage {
        id: MessageId::new("r1"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("t1"),
            content: vec![ToolResultContent::Text {
                text: "1\talpha".to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("m1"),
        },
    });
    let messages = [
        human(
            "u0",
            "[earlier conversation truncated for compaction retry]",
        ),
        assistant("m1", vec![call]),
        result,
        attachment(
            "a1",
            "total_tokens_reminder",
            "<total_tokens>1</total_tokens>",
        ),
        human("u1", "summarize"),
    ];
    let api = normalize(&messages, &options);
    let roles: Vec<ApiRole> = api.iter().map(|message| message.role).collect();
    assert_eq!(
        roles,
        [
            ApiRole::User,
            ApiRole::Assistant,
            ApiRole::User,
            ApiRole::System
        ]
    );
    assert_eq!(marked(&api), [(1, 0)]);
    // 主循环照旧：最后一条有内容的消息。
    assert_eq!(marked(&normalize(&messages, &ON)), [(3, 0)]);
    // 以 thinking 结尾的 assistant 不能盖章，再往前找；找不到就不加。
    let thinking = ContentBlock::Thinking {
        thinking: "t".to_owned(),
        signature: None,
    };
    let messages = [
        human("u0", "a"),
        assistant("m1", vec![text("b"), thinking]),
        human("u1", "summarize"),
    ];
    assert_eq!(marked(&normalize(&messages, &options)), [(0, 0)]);
    assert!(marked(&normalize(&[human("u1", "summarize")], &options)).is_empty());
}

fn image() -> ContentBlock {
    ContentBlock::Image {
        source: rust_cli_kernel_api::message::ImageSource::Base64 {
            media_type: "image/png".to_owned(),
            data: "iVBORw0KGgo=".to_owned(),
        },
    }
}

/// 带内容块的 attachment（`@` 提及的图片、带图片输出的 notebook，tui-input.md §1.3）。
fn media(id: &str, call: &str, blocks: Vec<ContentBlock>) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new(id),
        source: "file".to_owned(),
        text: reminder::wrap(call),
        data: serde_json::json!({
            "filename": "/w/x",
            rust_cli_kernel_api::message::ATTACHMENT_BLOCKS: blocks,
        }),
    })
}

/// 会话开头的图片不折进 system，调用说明与 image 块各成一条 user 消息；之后带图片的 notebook、
/// 署名与人类输入接在第二条后面；只有文字的 PDF 照旧折进 system。
#[test]
fn attachments_with_blocks_stay_in_user_messages() {
    let cell = reminder::wrap("<cell id=\"p\">show()</cell id=\"p\">");
    let messages = [
        human("u1", "Check @pic.png @doc.pdf @plot.ipynb"),
        media("a1", "call pic", vec![image()]),
        attachment(
            "a2",
            "file",
            "call doc\nResult of calling the Read tool:\nPDF file read",
        ),
        media("a3", "call plot", vec![text(&cell), image()]),
        attachment("a4", "remote_session_change", "Attribution"),
    ];
    let api = normalize(&messages, &ON);
    assert_eq!(api.len(), 3);
    assert_eq!(api[0].role, ApiRole::User);
    assert_eq!(texts(&api[0]), [reminder::wrap("call pic")]);
    assert_eq!(api[1].role, ApiRole::User);
    let second: Vec<&ContentBlock> = api[1].content.iter().map(|item| &item.block).collect();
    let attribution = text(&format!("{}\n", reminder::wrap("Attribution")));
    let prompt = text("Check @pic.png @doc.pdf @plot.ipynb");
    assert_eq!(
        second,
        [
            &image(),
            &text(&reminder::wrap("call plot")),
            &text(&cell),
            &image(),
            &attribution,
            &prompt
        ]
    );
    assert_eq!(api[2].role, ApiRole::System);
    assert!(texts(&api[2])[0].starts_with("call doc"));
    // 不支持对话中间 system 消息：只有文字的 attachment 留在 user 消息里，带内容块的位置不变。
    let api = normalize(&messages, &OFF);
    assert_eq!(api.len(), 2);
    assert_eq!(texts(&api[0]), [reminder::wrap("call pic")]);
    let second: Vec<&ContentBlock> = api[1].content.iter().map(|item| &item.block).collect();
    let doc = text(&reminder::wrap(
        "call doc\nResult of calling the Read tool:\nPDF file read",
    ));
    assert_eq!(
        second,
        [
            &image(),
            &doc,
            &text(&reminder::wrap("call plot")),
            &text(&cell),
            &image(),
            &attribution,
            &prompt
        ]
    );
}

/// 上一条已是 user（例如 user 角色的提醒）：调用说明与内容块直接接在后面，不补换行；assistant 之后重新分成两条。
#[test]
fn attachments_with_blocks_join_a_preceding_user_message() {
    let messages = [
        human("u1", "first"),
        attachment("a1", "remote_session_change", "Attribution"),
        media("a2", "call pic", vec![image()]),
    ];
    let api = normalize(&messages, &ON);
    assert_eq!(api.len(), 1);
    let blocks: Vec<&ContentBlock> = api[0].content.iter().map(|item| &item.block).collect();
    assert_eq!(
        blocks,
        [
            &text(&reminder::wrap("Attribution")),
            &text(&reminder::wrap("call pic")),
            &image(),
            &text("first"),
        ]
    );
    let messages = [
        human("u1", "first"),
        assistant("m1", vec![text("ok")]),
        human("u2", "second @pic.png"),
        media("a1", "call pic", vec![image()]),
    ];
    let api = normalize(&messages, &ON);
    let roles: Vec<ApiRole> = api.iter().map(|message| message.role).collect();
    assert_eq!(
        roles,
        [
            ApiRole::User,
            ApiRole::Assistant,
            ApiRole::User,
            ApiRole::User
        ]
    );
    assert_eq!(texts(&api[2]), [reminder::wrap("call pic")]);
    assert_eq!(api[3].content[0].block, image());
    assert_eq!(api[3].content[1].block, text("second @pic.png"));
}

fn read_then(result: &str, tool: &str, after: Vec<Message>) -> Vec<Message> {
    let tool_use = ToolUseId::new("toolu_1");
    let mut messages = vec![
        human("u1", "go"),
        assistant(
            "m1",
            vec![ContentBlock::ToolUse {
                id: tool_use.clone(),
                name: tool.to_owned(),
                input: Value::Null,
            }],
        ),
        Message::User(UserMessage {
            id: MessageId::new("u2"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use,
                content: vec![ToolResultContent::Text {
                    text: result.to_owned(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("m1"),
            },
        }),
    ];
    messages.extend(after);
    messages
}

fn result_texts(message: &ApiMessage) -> Vec<String> {
    match &message.content[0].block {
        ContentBlock::ToolResult { content, .. } => content
            .iter()
            .map(|item| match item {
                ToolResultContent::Text { text } => text.clone(),
                other => format!("{other:?}"),
            })
            .collect(),
        other => panic!("not a tool result: {other:?}"),
    }
}

/// 没有 system turn 的模型（claude-sonnet-4-6）：工具轮的提醒并进工具结果的字符串，
/// 缓存断点落在工具结果上。缺陷修复前提醒是结果后面的另一个文本块（R4 F7）。
#[test]
fn without_system_turns_the_reminder_joins_the_tool_result() {
    let messages = read_then(
        "1\talpha\n2\tbeta\n3\t",
        "Read",
        vec![attachment(
            "a1",
            "total_tokens_reminder",
            "<total_tokens>9 tokens left</total_tokens>",
        )],
    );
    let api = normalize(&messages, &OFF);
    assert_eq!(api.len(), 3);
    assert_eq!(api[2].content.len(), 1);
    assert_eq!(
        result_texts(&api[2]),
        [
            "1\talpha\n2\tbeta\n3\n\n<system-reminder>\n<total_tokens>9 tokens left</total_tokens>\n</system-reminder>"
        ]
    );
    assert_eq!(marked(&api), [(2, 0)]);
}

/// Agent / MCP 的结果是块数组：最后一个文本块补换行，提醒成为结果里的下一个块。
#[test]
fn an_agent_result_takes_the_reminder_as_another_block() {
    let messages = read_then("done", "Agent", vec![attachment("a1", "date", "d1")]);
    let api = normalize(&messages, &OFF);
    assert_eq!(
        result_texts(&api[2]),
        ["done\n", "<system-reminder>\nd1\n</system-reminder>"]
    );
}

/// 有 system turn 时 system 角色的提醒照常折成 system 消息；user 角色的提醒同样并进结果（并入
/// 不看 system turn）。
#[test]
fn with_system_turns_only_user_role_reminders_join_the_tool_result() {
    let messages = read_then(
        "ok",
        "Read",
        vec![
            attachment("a1", "date", "d1"),
            attachment("a2", "session_context", "ctx"),
        ],
    );
    let api = normalize(&messages, &ON);
    assert_eq!(api.len(), 4);
    assert_eq!(
        result_texts(&api[2]),
        ["ok\n\n<system-reminder>\nctx\n</system-reminder>"]
    );
    assert_eq!(texts(&api[3]), ["d1"]);
}

/// 工具边界并入的人类插话（`queued_command`、`commandMode: prompt`，query.md §4）。
fn steered(id: &str, prompt: &str) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new(id),
        source: "queued_command".to_owned(),
        text: reminder::wrap(&format!(
            "The user sent a new message while you were working:\n{prompt}"
        )),
        data: serde_json::json!({"prompt": prompt, "commandMode": "prompt"}),
    })
}

/// 修复线 F13（context.md §3 第 10 条）：有 system turn 时人类插话按人类输入
/// ——去掉包装、作为文本块接在工具结果所在的 user 消息后面，排在 system 文字之前；两条插话
/// 之间补换行。修前它被折进末尾的 system 消息，与剩余 token 连在一起。
#[test]
fn with_system_turns_a_steered_prompt_follows_the_tool_result_as_text() {
    let messages = read_then(
        "ok",
        "Read",
        vec![
            steered("q1", "first"),
            steered("q2", "second"),
            attachment(
                "a1",
                "total_tokens_reminder",
                "<total_tokens>9</total_tokens>",
            ),
        ],
    );
    let api = normalize(&messages, &ON);
    assert_eq!(api.len(), 4);
    assert_eq!(api[2].role, ApiRole::User);
    assert!(matches!(
        api[2].content[0].block,
        ContentBlock::ToolResult { .. }
    ));
    assert_eq!(
        texts(&api[2])[1..],
        [
            "The user sent a new message while you were working:\nfirst\n",
            "The user sent a new message while you were working:\nsecond"
        ]
    );
    assert_eq!(api[3].role, ApiRole::System);
    assert_eq!(texts(&api[3]), ["<total_tokens>9</total_tokens>"]);
}

/// 没有 system turn 的模型照旧（带包装、并进工具结果）；后台通知（`commandMode: task-notification`）仍是 system
/// 角色。
#[test]
fn notices_and_models_without_system_turns_keep_the_old_place_for_queued_commands() {
    let api = normalize(&read_then("ok", "Read", vec![steered("q1", "first")]), &OFF);
    assert_eq!(
        result_texts(&api[2]),
        [
            "ok\n\n<system-reminder>\nThe user sent a new message while you were working:\nfirst\n</system-reminder>"
        ]
    );
    let notice = Message::Attachment(AttachmentMessage {
        id: MessageId::new("n1"),
        source: "queued_command".to_owned(),
        text: reminder::wrap("<task-notification>done</task-notification>"),
        data: serde_json::json!({"prompt": "done", "commandMode": "task-notification"}),
    });
    let api = normalize(&read_then("ok", "Read", vec![notice]), &ON);
    assert_eq!(api.len(), 4);
    assert_eq!(api[2].content.len(), 1);
    assert_eq!(
        texts(&api[3]),
        ["<task-notification>done</task-notification>"]
    );
}
