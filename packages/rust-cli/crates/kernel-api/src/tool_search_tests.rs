use serde_json::{Value, json};

use super::*;
use crate::ids::{MessageId, ToolUseId};
use crate::mcp::McpToolInfo;
use crate::message::{UserMessage, UserOrigin};

struct Flags {
    name: String,
    defer: bool,
    /// MCP 工具的来源（`always_load` 在里面）；内置工具为 None。
    mcp: Option<McpToolInfo>,
    web_search: bool,
    enabled: bool,
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
    fn is_enabled(&self) -> bool {
        self.enabled
    }
    fn needs_native_web_search(&self) -> bool {
        self.web_search
    }
    fn mcp_info(&self) -> Option<&McpToolInfo> {
        self.mcp.as_ref()
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
        mcp: mcp.then(|| McpToolInfo {
            server: "s".to_owned(),
            tool: "t".to_owned(),
            always_load: always,
        }),
        web_search: false,
        enabled: true,
    }
}

fn names(tools: &[Arc<dyn Tool>]) -> Vec<&str> {
    tools.iter().map(|tool| tool.name()).collect()
}

/// 模型这一步看得到的工具（工作线 F131：主循环、`system/init` 与 `/context` 共用）：不启用的不在；能力答不出或支持时
/// 留 WebSearch，答“不支持”时去掉；有 ToolSearch 时去掉 WaitForMcpServers。
#[test]
fn visible_tools_follow_the_step_rules() {
    let all = || -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(tool("Read", false, false, false)),
            Arc::new(Flags {
                web_search: true,
                ..tool("WebSearch", true, false, false)
            }),
            Arc::new(Flags {
                enabled: false,
                ..tool("TodoWrite", false, false, false)
            }),
            Arc::new(tool(WAIT_FOR_MCP_SERVERS, false, false, false)),
        ]
    };
    assert_eq!(
        names(&visible_tools(all(), None)),
        ["Read", "WebSearch", WAIT_FOR_MCP_SERVERS]
    );
    let without = ModelCapabilities {
        supports_native_web_search: false,
        ..ModelCapabilities::default()
    };
    assert_eq!(
        names(&visible_tools(all(), Some(&without))),
        ["Read", WAIT_FOR_MCP_SERVERS]
    );
    let mut with_search = all();
    with_search.push(Arc::new(tool(TOOL_SEARCH, false, false, false)));
    assert_eq!(
        names(&visible_tools(with_search, None)),
        ["Read", "WebSearch", TOOL_SEARCH]
    );
}

/// ToolSearch 要有延迟的工具才用得上；工具顺序是内置在前、MCP 在后，同组按名字。
#[test]
fn search_applies_and_the_model_order() {
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(tool("mcp__s__b", false, true, true)),
        Arc::new(tool("write", false, false, false)),
        Arc::new(tool(TOOL_SEARCH, false, false, false)),
        Arc::new(tool("Read", false, false, false)),
        Arc::new(tool("mcp__s__a", false, true, false)),
    ];
    assert!(search_applies(&tools));
    tools.sort_by(|left, right| model_order(left.as_ref(), right.as_ref()));
    assert_eq!(
        names(&tools),
        ["Read", TOOL_SEARCH, "write", "mcp__s__a", "mcp__s__b"]
    );
    // 唯一的 MCP 工具是 alwaysLoad：没有延迟的工具，ToolSearch 用不上。
    tools.retain(|tool| tool.name() != "mcp__s__a");
    assert!(!search_applies(&tools));
}

#[test]
fn deferral_follows_always_load_mcp_and_should_defer() {
    assert!(is_deferred(&tool("WebFetch", true, false, false)));
    assert!(!is_deferred(&tool("Read", false, false, false)));
    // MCP 工具一律延迟，除非 alwaysLoad。
    assert!(is_deferred(&tool("mcp__s__t", false, true, false)));
    assert!(!is_deferred(&tool("mcp__s__t", false, true, true)));
    // ToolSearch 自己从不延迟。
    assert!(!is_deferred(&tool(
        crate::tool_names::TOOL_SEARCH,
        true,
        false,
        false
    )));
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

fn boundary(carried: &[&str]) -> Message {
    use crate::message::{CompactBoundary, CompactTrigger, SystemMessage};
    Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new("b1"),
        trigger: CompactTrigger::Auto,
        pre_tokens: 0,
        summary: MessageId::new("s1"),
        preserved: None,
        discovered_tools: carried.iter().map(|name| (*name).to_owned()).collect(),
    }))
}

#[test]
fn loaded_names_include_the_tools_a_boundary_carries() {
    let history = vec![
        boundary(&["mcp__fixture__lookup", "WebFetch"]),
        Message::User(UserMessage {
            id: MessageId::new("m1"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("t1"),
                content: vec![ToolResultContent::ToolReference {
                    tool_name: "NotebookEdit".to_owned(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("a1"),
            },
        }),
    ];
    let names: Vec<String> = loaded(&history).into_iter().collect();
    assert_eq!(names, ["NotebookEdit", "WebFetch", "mcp__fixture__lookup"]);
    // 只看引用的旧函数不变。
    let names: Vec<String> = referenced(&history).into_iter().collect();
    assert_eq!(names, ["NotebookEdit"]);
}

#[test]
fn the_carried_tools_are_optional_in_the_boundary_json() {
    let Message::System(crate::message::SystemMessage::CompactBoundary(empty)) = boundary(&[])
    else {
        unreachable!()
    };
    let json = serde_json::to_value(&empty).unwrap();
    assert!(json.get("discovered_tools").is_none());
    let read: crate::message::CompactBoundary = serde_json::from_value(json).unwrap();
    assert!(read.discovered_tools.is_empty());
    let Message::System(crate::message::SystemMessage::CompactBoundary(carried)) =
        boundary(&["WebFetch"])
    else {
        unreachable!()
    };
    let json = serde_json::to_value(&carried).unwrap();
    assert_eq!(json["discovered_tools"], json!(["WebFetch"]));
}

/// 搜索回退（F132 的 T2）：有启用的 Bash 时不补；没有时按 `wanted` 的顺序补回 Glob / Grep，跳过已在的、别的名字与
/// 取不回的。
#[test]
fn search_falls_back_only_without_an_enabled_bash() {
    let arc = |tool: Flags| Arc::new(tool) as Arc<dyn Tool>;
    let withheld = |name: &str| {
        SEARCH_TOOLS
            .contains(&name)
            .then(|| arc(tool(name, false, false, false)))
    };
    let bash = arc(tool("Bash", false, false, false));
    let disabled = arc(Flags {
        enabled: false,
        ..tool("Bash", false, false, false)
    });
    assert!(search_covered([&bash]));
    assert!(!search_covered([&disabled]));
    assert!(search_fallback(&[bash], ["Grep", "Glob"], withheld).is_empty());
    let read = arc(tool("Read", false, false, false));
    let grep = arc(tool("Grep", false, false, false));
    assert_eq!(
        names(&search_fallback(
            std::slice::from_ref(&read),
            ["Grep", "Read", "Glob", "Grep"],
            withheld
        )),
        ["Grep", "Glob"]
    );
    assert_eq!(
        names(&search_fallback(&[disabled, grep], SEARCH_TOOLS, withheld)),
        ["Glob"]
    );
    assert!(search_fallback(&[read], ["Glob"], |_| None).is_empty());
}
