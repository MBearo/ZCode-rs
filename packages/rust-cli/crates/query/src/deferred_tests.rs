use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{ContentBlock, ToolResultContent, UserMessage, UserOrigin};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::tool::{ToolError, ToolOutput, ToolProgress, ToolUseContext};
use serde_json::Value;
use tokio::sync::mpsc;

use super::*;

/// 只有名字与延迟标记的工具。
struct Named {
    name: &'static str,
    defer: bool,
    mcp: bool,
}

#[async_trait]
impl Tool for Named {
    fn name(&self) -> &str {
        self.name
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &PromptContext) -> String {
        format!("{} tool", self.name)
    }
    fn should_defer(&self) -> bool {
        self.defer
    }
    fn is_mcp(&self) -> bool {
        self.mcp
    }
    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Unsupported {
            what: "test".to_owned(),
        })
    }
}

fn tool(name: &'static str, defer: bool) -> Arc<dyn Tool> {
    Arc::new(Named {
        name,
        defer,
        mcp: name.starts_with("mcp__"),
    })
}

fn pool() -> Vec<Arc<dyn Tool>> {
    vec![
        tool("Write", false),
        tool("Bash", false),
        tool("mcp__srv__ping", false),
        tool("WebFetch", true),
        tool(TOOL_SEARCH, false),
        tool("Read", false),
        tool("NotebookEdit", true),
    ]
}

fn prompt() -> PromptContext {
    PromptContext {
        tool_names: Vec::new(),
        mode: PermissionMode::Default,
    }
}

fn names(wire: &WireTools) -> Vec<&str> {
    wire.definitions
        .iter()
        .map(|tool| tool.name.as_str())
        .collect()
}

fn referencing(name: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("m1"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("t1"),
            content: vec![ToolResultContent::ToolReference {
                tool_name: name.to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("a1"),
        },
    })
}

#[test]
fn deferred_tools_stay_out_until_referenced_and_the_placeholder_is_second_to_last() {
    let wire = wire_tools(&pool(), &[], &prompt(), (true, ToolOrder::ByName));
    // 内置按名字排序、MCP 在后（MCP 工具一律延迟）；占位工具在倒数第二位（在 Write 之前）。
    assert_eq!(
        names(&wire),
        ["Bash", "Read", TOOL_SEARCH, PLACEHOLDER, "Write"]
    );
    assert_eq!(wire.flags.keys().collect::<Vec<_>>(), [PLACEHOLDER]);
    let placeholder = &wire.definitions[3];
    assert_eq!(placeholder.description, PLACEHOLDER_DESCRIPTION);
    assert_eq!(
        placeholder.input_schema,
        json!({"type": "object", "properties": {}})
    );

    let history = [referencing("WebFetch"), referencing("mcp__srv__ping")];
    let wire = wire_tools(&pool(), &history, &prompt(), (true, ToolOrder::ByName));
    assert_eq!(
        names(&wire),
        [
            "Bash",
            "Read",
            TOOL_SEARCH,
            "WebFetch",
            "Write",
            PLACEHOLDER,
            "mcp__srv__ping"
        ]
    );
    assert!(wire.flags["WebFetch"].defer_loading);
    assert!(wire.flags["mcp__srv__ping"].defer_loading);
    assert!(!wire.flags.contains_key("Bash"));
}

#[test]
fn without_support_every_tool_is_inline_and_tool_search_is_dropped() {
    let wire = wire_tools(&pool(), &[], &prompt(), (false, ToolOrder::ByName));
    assert_eq!(
        names(&wire),
        [
            "Bash",
            "NotebookEdit",
            "Read",
            "WebFetch",
            "Write",
            "mcp__srv__ping"
        ]
    );
    assert!(wire.flags.is_empty());
    // 没有延迟工具时也不启用。
    let plain = vec![tool("Read", false), tool(TOOL_SEARCH, false)];
    let wire = wire_tools(&plain, &[], &prompt(), (true, ToolOrder::ByName));
    assert_eq!(names(&wire), ["Read"]);
}

fn content(message: &Message) -> Vec<ContentBlock> {
    match message {
        Message::User(user) => user.content.clone(),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn referenced_results_get_the_tool_loaded_note_and_unavailable_ones_are_dropped() {
    let message = referencing("WebFetch");
    let available = vec!["WebFetch".to_owned()];
    // 启用时：保留引用，末尾加 `Tool loaded.`；已有说明时不再加。
    let sent = tool_references(&message, true, &available);
    let blocks = content(&sent);
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        blocks[1],
        ContentBlock::Text {
            text: "Tool loaded.".to_owned()
        }
    );
    assert_eq!(content(&tool_references(&sent, true, &available)).len(), 2);
    // 工具不在了 / 没有启用：引用去掉，换成占位文本，不加说明。
    let gone = tool_references(&message, true, &[]);
    let removed = |blocks: Vec<ContentBlock>| match &blocks[..] {
        [ContentBlock::ToolResult { content, .. }] => content.clone(),
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(
        removed(content(&gone)),
        [ToolResultContent::Text {
            text: "[Tool references removed - tools no longer available]".to_owned()
        }]
    );
    assert_eq!(
        removed(content(&tool_references(&message, false, &available))),
        [ToolResultContent::Text {
            text: "[Tool references removed - tool search not enabled]".to_owned()
        }]
    );
    // 没有引用的消息原样借用。
    let plain = Message::User(UserMessage {
        id: MessageId::new("m2"),
        content: vec![ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        origin: UserOrigin::Human,
    });
    assert!(matches!(
        tool_references(&plain, true, &available),
        std::borrow::Cow::Borrowed(_)
    ));
}
