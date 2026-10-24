//! Node 写入的会话冷读取：`fixtures/node-cold.json`（复制自 `apps/zcode-cli-rust/crates/state/fixtures`，
//! 由 `scripts/zcode-cli-rust-node-cold-fixtures.mjs` 用 Node 仓储写入并记录 Node
//! `hydrateMessageHistoryFromSession` 的结果）。比较文本、推理、工具调用与工具结果；去掉本 crate
//! 不读的部分：attachment 条目、只有合成 part 的提醒消息、没有任何内容的 assistant、文件 / agent
//! part 的占位文本（docs/specs/rust-cli/node-store.md §5、§7）。Node 给未结束调用补的中断结果，本工程在同一位置补同样的
//! 结果（Node 的文字，§4.6，工作线 F86、F88），直接比。
use rusqlite::Connection;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{
    ContentBlock, ImageSource, Message, ToolResultContent, UserOrigin,
};
use serde_json::{Value, json};

use crate::repo::open;

fn fixture() -> Value {
    serde_json::from_str(include_str!("../fixtures/node-cold.json")).unwrap()
}

/// 迁移过的库，写入夹具里的原始行（`{table: [[列值...]]}`）。
fn database(tables: &Value) -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let (conn, _) =
        open::open(&dir.path().join("db.sqlite"), open::MIGRATION_LOCK_WAIT, 1).unwrap();
    for (table, rows) in tables.as_object().unwrap() {
        for row in rows.as_array().unwrap() {
            let values: Vec<rusqlite::types::Value> = row
                .as_array()
                .unwrap()
                .iter()
                .map(|v| match v {
                    Value::Null => rusqlite::types::Value::Null,
                    Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().unwrap()),
                    Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                    other => panic!("unexpected column {other}"),
                })
                .collect();
            let slots = vec!["?"; values.len()].join(",");
            conn.execute(
                &format!("insert into {table} values ({slots})"),
                rusqlite::params_from_iter(values),
            )
            .unwrap();
        }
    }
    (dir, conn)
}

fn joined(texts: Vec<&str>) -> String {
    texts
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 文件与 agent part 在 Node 历史里的占位文本。
fn placeholder(text: &str) -> bool {
    text.starts_with("[Attached ") || text.starts_with("[Selected agent: ")
}

/// Node 工具结果的内容：字符串是一个文本块；图片取媒体类型与 data URL。
fn node_content(content: &Value) -> Value {
    match content {
        Value::String(text) => json!([{"type": "text", "text": text}]),
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| match b["type"].as_str() {
                Some("image") => json!({"type": "image", "mediaType": b["mediaType"],
                    "dataUrl": b["dataUrl"]}),
                _ => json!({"type": "text", "text": b["text"]}),
            })
            .collect(),
        _ => Value::Null,
    }
}

/// Node 历史条目的比较形状。
fn node_text(entry: &Value) -> Option<Value> {
    let message = entry.get("message")?;
    let role = message["role"].as_str()?;
    if role == "tool" {
        return Some(json!({"role": "tool", "id": message["toolCallId"],
            "error": message["isError"], "content": node_content(&message["content"])}));
    }
    if role == "user" && entry["metadata"]["source"] == "legacy_synthetic" && !summary_like(message)
    {
        return None;
    }
    let (texts, reasoning): (Vec<&str>, Vec<Value>) = match &message["content"] {
        Value::String(text) => (vec![text.as_str()], vec![]),
        Value::Array(blocks) => (
            blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .filter(|t| !placeholder(t))
                .collect(),
            blocks
                .iter()
                .filter(|b| b["type"] == "reasoning")
                .map(|b| json!({"text": b["text"], "options": b["providerOptions"]}))
                .collect(),
        ),
        _ => return None,
    };
    let text = joined(texts);
    let tools: Vec<Value> = message["toolCalls"]
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .map(|c| json!({"id": c["id"], "name": c["name"], "input": c["input"]}))
                .collect()
        })
        .unwrap_or_default();
    match role {
        "user" => Some(json!({"role": "user", "text": text})),
        "assistant" if text.is_empty() && reasoning.is_empty() && tools.is_empty() => None,
        "assistant" => {
            Some(json!({"role": "assistant", "text": text, "reasoning": reasoning, "tools": tools}))
        }
        _ => None,
    }
}

/// 压缩摘要在 Node 历史里也标为 legacy_synthetic，但它属于文本部分。
fn summary_like(message: &Value) -> bool {
    message["content"]
        .as_str()
        .is_some_and(|text| text.starts_with("This session is being continued"))
}

fn kernel_content(content: &[ToolResultContent]) -> Value {
    content
        .iter()
        .map(|block| match block {
            ToolResultContent::Text { text } => json!({"type": "text", "text": text}),
            ToolResultContent::Image {
                source: ImageSource::Base64 { media_type, data },
            } => json!({"type": "image", "mediaType": media_type,
                "dataUrl": format!("data:{media_type};base64,{data}")}),
            ToolResultContent::Image { source } | ToolResultContent::Document { source } => {
                panic!("unexpected media {source:?}")
            }
            ToolResultContent::ToolReference { tool_name } => panic!("unexpected {tool_name}"),
        })
        .collect()
}

fn kernel_text(message: &Message) -> Vec<Value> {
    let texts = |content: &[ContentBlock]| {
        joined(
            content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect(),
        )
    };
    match message {
        Message::User(user) if matches!(user.origin, UserOrigin::ToolResult { .. }) => user
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => Some(json!({"role": "tool", "id": tool_use_id, "error": is_error,
                    "content": kernel_content(content)})),
                _ => None,
            })
            .collect(),
        Message::User(user) => vec![json!({"role": "user", "text": texts(&user.content)})],
        Message::Assistant(assistant) => {
            let reasoning: Vec<Value> = assistant
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Thinking {
                        thinking,
                        signature,
                    } => Some(json!({"text": thinking,
                        "options": signature.as_ref().map(|s| json!({"anthropic": {"signature": s}}))})),
                    ContentBlock::RedactedThinking { data } => Some(json!({"text": "",
                        "options": {"anthropic": {"redactedData": data}}})),
                    _ => None,
                })
                .collect();
            let tools: Vec<Value> = assistant
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, name, input } => {
                        Some(json!({"id": id, "name": name, "input": input}))
                    }
                    _ => None,
                })
                .collect();
            vec![
                json!({"role": "assistant", "text": texts(&assistant.content),
                "reasoning": reasoning, "tools": tools}),
            ]
        }
        // 压缩边界不发给模型，Node 的上下文里也没有它。
        Message::System(_) => vec![],
        other => panic!("unexpected message {other:?}"),
    }
}

#[test]
fn node_written_sessions_read_back_their_text() {
    let fixture = fixture();
    for case in ["basic", "compacted", "rewound", "rich", "failures"] {
        let case_fixture = &fixture[case];
        let (_dir, conn) = database(&case_fixture["tables"]);
        let session = SessionId::new(case_fixture["sessionID"].as_str().unwrap());
        let (loaded, _) = crate::load::load(&conn, &session).unwrap();
        let expected: Vec<Value> = case_fixture["history"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(node_text)
            .collect();
        assert!(expected.len() >= 4, "{case}: {expected:?}");
        let actual: Vec<Value> = loaded.messages.iter().flat_map(kernel_text).collect();
        assert_eq!(Value::from(actual), Value::from(expected), "{case}");
    }
}

/// 回退切点与压缩边界：活动分支只剩保留前缀与切点之后的消息；压缩后从边界开始，接着是
/// `CompactSummary` 摘要，保留段原样接在摘要后面。
#[test]
fn the_active_branch_applies_the_revert_cursor_and_the_compaction_boundary() {
    let fixture = fixture();
    let ids = |case: &str| {
        let (_dir, conn) = database(&fixture[case]["tables"]);
        let session = SessionId::new(fixture[case]["sessionID"].as_str().unwrap());
        let (loaded, _) = crate::load::load(&conn, &session).unwrap();
        loaded
            .messages
            .iter()
            .map(|m| m.id().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids("rewound"), ["u1", "a1", "u3", "a3"]);
    // Node 写的压缩边界（`b1`）还原成内核边界，排在摘要之前（compact.md §6）。
    assert_eq!(ids("compacted"), ["b1", "s1", "u2", "a2", "u3", "a3"]);
    let (_dir, conn) = database(&fixture["compacted"]["tables"]);
    let session = SessionId::new(fixture["compacted"]["sessionID"].as_str().unwrap());
    let (loaded, _) = crate::load::load(&conn, &session).unwrap();
    assert!(matches!(
        &loaded.messages[1],
        Message::User(user) if user.origin == UserOrigin::CompactSummary
    ));
    assert_eq!(
        loaded.mode,
        Some(rust_cli_kernel_api::permission::PermissionMode::Default)
    );
}
