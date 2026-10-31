//! 不带参数的 `/rename` 的文本处理（titles.md §8.3–§8.5）：对话文本、JSON 解析、
//! 名字的清理与请求的形状。
use rust_cli_base::models::{GLM_5_3, OPUS_5_5};
use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::{ModelSelection, OutputFormat, QuerySource};

use super::*;

fn text(value: &str) -> ContentBlock {
    ContentBlock::Text {
        text: value.to_owned(),
    }
}

fn user(id: &str, origin: UserOrigin, blocks: Vec<ContentBlock>) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: blocks,
        origin,
    })
}

fn assistant(id: &str, blocks: Vec<ContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: blocks,
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    })
}

#[test]
fn the_conversation_keeps_user_and_assistant_text_only() {
    let history = vec![
        user(
            "u1",
            UserOrigin::Human,
            vec![text("Fix the flaky login test")],
        ),
        Message::Attachment(AttachmentMessage {
            id: MessageId::new("a1"),
            source: "date".to_owned(),
            text: "<system-reminder>date</system-reminder>".to_owned(),
            data: serde_json::Value::Null,
        }),
        assistant(
            "m1",
            vec![
                ContentBlock::Thinking {
                    thinking: "hidden".to_owned(),
                    signature: None,
                },
                text("I will look."),
                ContentBlock::ToolUse {
                    id: ToolUseId::new("toolu_1"),
                    name: "Read".to_owned(),
                    input: serde_json::json!({}),
                },
            ],
        ),
        user(
            "r1",
            UserOrigin::ToolResult {
                assistant: MessageId::new("m1"),
            },
            vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("toolu_1"),
                content: Vec::new(),
                is_error: false,
            }],
        ),
        user(
            "meta1",
            UserOrigin::Meta {
                source: "continue".to_owned(),
            },
            vec![text("Continue from where you left off.")],
        ),
        user(
            "u2",
            UserOrigin::Human,
            vec![text("Then <notes id=\"1\">rerun</notes> it")],
        ),
    ];
    // text 块原样取（titles.md D13）。
    assert_eq!(
        conversation(&history),
        "Fix the flaky login test\nI will look.\nThen <notes id=\"1\">rerun</notes> it"
    );
    assert_eq!(conversation(&[]), "");
}

#[test]
fn the_conversation_keeps_the_last_thousand_units() {
    let long = "a".repeat(990) + "\u{1F600}" + &"b".repeat(20);
    let history = vec![user("u1", UserOrigin::Human, vec![text(&long)])];
    let tail = conversation(&history);
    // 末尾 1000 个 UTF-16 码元：截开的代理对整字丢掉（titles.md D10）。
    assert_eq!(tail.encode_utf16().count(), 1000);
    assert!(tail.ends_with(&"b".repeat(20)));
    let cut = "\u{1F600}".to_owned() + &"c".repeat(999);
    let history = vec![user("u1", UserOrigin::Human, vec![text(&cut)])];
    assert_eq!(conversation(&history), "c".repeat(999));
}

#[test]
fn the_reply_is_parsed_as_json_with_a_name() {
    assert_eq!(
        parse_name("{\"name\":\"flaky-login-test\"}"),
        Some("flaky-login-test".to_owned())
    );
    assert_eq!(
        parse_name("  ```json\n{\"name\": \"fenced\"}\n```  "),
        Some("fenced".to_owned())
    );
    assert_eq!(parse_name("{\"name\": 3}"), None);
    assert_eq!(parse_name("{\"title\": \"x\"}"), None);
    assert_eq!(parse_name("plain words"), None);
    assert_eq!(parse_name(""), None);
}

#[test]
fn names_are_cleaned() {
    assert_eq!(clean_name("  fix-login  "), Some("fix-login".to_owned()));
    // 连续的控制与格式字符换成一个空格，再去掉剩下的 C0 / C1 控制字符。
    assert_eq!(
        clean_name("a\u{200B}\u{200D}b\tc\u{0085}d"),
        Some("a b c d".to_owned())
    );
    assert_eq!(clean_name("\u{2028}\u{FEFF} "), None);
    let long = "x".repeat(250);
    assert_eq!(clean_name(&long).map(|n| n.chars().count()), Some(200));
}

#[test]
fn the_rename_request_has_the_expected_shape() {
    // Anthropic 的有卡片的模型：输出上限与 effort 按卡片（卡片的上限只给有卡片的模型）。
    let carded = ModelSelection {
        provider: "anthropic".to_owned(),
        model: OPUS_5_5.to_owned(),
        reasoning: None,
        connection: None,
    };
    let request = request(
        &carded,
        &SessionId::new("sess_rename"),
        ("Fix it\nOK", None),
    );
    assert_eq!(request.selection, carded);
    // 工作线 F52：带上会话（`metadata.user_id` 的 `session_id`）。
    assert_eq!(
        request.options.session_id,
        Some(SessionId::new("sess_rename"))
    );
    let system: Vec<&str> = request.system.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(
        system,
        [
            rust_cli_prompts::texts::identity(),
            rust_cli_prompts::title::rename_system()
        ]
    );
    assert_eq!(request.messages.len(), 1);
    assert_eq!(
        request.messages[0].content[0].block,
        text("<conversation>\nFix it\nOK\n</conversation>")
    );
    assert!(request.tools.is_empty());
    assert_eq!(request.max_tokens, 128_000);
    assert_eq!(request.thinking, None);
    assert_eq!(request.options.effort.as_deref(), Some("medium"));
    assert!(matches!(
        &request.options.output_format,
        Some(OutputFormat::JsonSchema { schema }) if schema["required"] == serde_json::json!(["name"])
    ));
    assert_eq!(
        request.source,
        QuerySource::Side("rename_generate_name".to_owned())
    );
    // 有卡片形状的模型（GLM 按 opus）上会话选了推理档位时用它；GLM 的输出上限按目录（工作线 F121），目录没给时同没有
    // 卡片的模型。
    let chosen = ModelSelection {
        provider: "p".to_owned(),
        model: GLM_5_3.to_owned(),
        reasoning: Some("high".to_owned()),
        connection: None,
    };
    let glm = super::request(
        &chosen,
        &SessionId::new("sess_rename"),
        ("x", Some(128_000)),
    );
    assert_eq!(glm.options.effort.as_deref(), Some("high"));
    assert_eq!(glm.max_tokens, 128_000);
    let unlisted = super::request(&chosen, &SessionId::new("sess_rename"), ("x", None));
    assert_eq!(unlisted.max_tokens, 32_000);
    // 没有模型卡片时输出上限是 32000（与主循环相同），也不写 effort——推理字段只由配置的 option map 写（修复线
    // F23 修的 bug：以前把选择的档位写进 `output_config.effort`）。
    let other = ModelSelection {
        provider: "p".to_owned(),
        model: "unknown".to_owned(),
        reasoning: Some("high".to_owned()),
        connection: None,
    };
    let request = super::request(&other, &SessionId::new("sess_rename"), ("x", None));
    assert_eq!(request.max_tokens, 32_000);
    assert_eq!(request.options.effort, None);
}

/// 格式字符（Cf）的判断：双向与零宽部分改用 base 的表之后，与原来的整张表逐个码点相同（起名的清理不变）。
#[test]
fn format_characters_are_the_same_set_as_before() {
    fn old_is_format(code: u32) -> bool {
        matches!(
            code,
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
    for code in 0..=0x10FFFF_u32 {
        if let Some(c) = char::from_u32(code) {
            assert_eq!(super::is_format(c), old_is_format(code), "U+{code:04X}");
        }
    }
}
