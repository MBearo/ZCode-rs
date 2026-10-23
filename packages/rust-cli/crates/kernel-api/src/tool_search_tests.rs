use serde_json::{Value, json};

use super::*;
use crate::ids::{MessageId, ToolUseId};
use crate::message::{UserMessage, UserOrigin};
struct Flags {
    name: String,
    defer: bool,
    mcp: bool,
    always: bool,
}

#[async_trait::async_trait]
impl Tool for Flags {
    fn name(&self) -> &str {
        &self.name
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &crate::tool::PromptContext) -> String {
        String::new()
    }
    fn should_defer(&self) -> bool {
        self.defer
    }
    fn is_mcp(&self) -> bool {
        self.mcp
    }
    fn always_load(&self) -> bool {
        self.always
    }
    async fn call(
        &self,
        _input: Value,
        _context: crate::tool::ToolUseContext,
        _progress: tokio::sync::mpsc::Sender<crate::tool::ToolProgress>,
    ) -> Result<crate::tool::ToolOutput, crate::tool::ToolError> {
        Err(crate::tool::ToolError::Unsupported {
            what: "test".to_owned(),
        })
    }
}

fn tool(name: &str, defer: bool, mcp: bool, always: bool) -> Flags {
    Flags {
        name: name.to_owned(),
        defer,
        mcp,
        always,
    }
}

#[test]
fn deferral_follows_always_load_mcp_and_should_defer() {
    assert!(is_deferred(&tool("WebFetch", true, false, false)));
    assert!(!is_deferred(&tool("Read", false, false, false)));
    // MCP 工具一律延迟，除非 alwaysLoad。
    assert!(is_deferred(&tool("mcp__s__t", false, true, false)));
    assert!(!is_deferred(&tool("mcp__s__t", false, true, true)));
    // ToolSearch 自己从不延迟。
    assert!(!is_deferred(&tool(TOOL_SEARCH, true, false, false)));
}

#[test]
fn referenced_names_come_from_tool_results() {
    let reference = |name: &str| ToolResultContent::ToolReference {
        tool_name: name.to_owned(),
    };
    let history = vec![
        Message::User(UserMessage {
            id: MessageId::new("m1"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("t1"),
                content: vec![reference("WebFetch"), reference("NotebookEdit")],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("a1"),
            },
        }),
        Message::User(UserMessage {
            id: MessageId::new("m2"),
            content: vec![ContentBlock::Text {
                text: "tool_reference WebSearch".to_owned(),
            }],
            origin: UserOrigin::Human,
        }),
    ];
    let names: Vec<String> = referenced(&history).into_iter().collect();
    assert_eq!(names, ["NotebookEdit", "WebFetch"]);
}

#[test]
fn tool_reference_round_trips_in_anthropic_shape() {
    let block = ToolResultContent::ToolReference {
        tool_name: "WebFetch".to_owned(),
    };
    let value = serde_json::to_value(&block).unwrap();
    assert_eq!(
        value,
        json!({"type": "tool_reference", "tool_name": "WebFetch"})
    );
    assert_eq!(
        serde_json::from_value::<ToolResultContent>(value).unwrap(),
        block
    );
}

#[test]
fn names_sort_like_locale_compare() {
    let mut names = vec!["Write", "WebFetch", "agent_x", "Bash", "TaskStop", "taskA"];
    names.sort_by(|left, right| compare_names(left, right));
    assert_eq!(
        names,
        ["agent_x", "Bash", "taskA", "TaskStop", "WebFetch", "Write"]
    );
}
