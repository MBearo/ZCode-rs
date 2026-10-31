use rust_cli_base::models::{DEEPSEEK_V4_PRO, GLM_5_3, HAIKU_3_5_DATED};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::listing::ListingEntry;
use rust_cli_kernel_api::message::{AttachmentMessage, Message};
use serde_json::Value;

use super::*;

fn entry(name: &str) -> ListingEntry {
    ListingEntry {
        name: name.to_owned(),
        line: format!("- {name}: does {name}"),
    }
}

fn recorded(source: &str, (_, data): (String, Value)) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("a"),
        source: source.to_owned(),
        text: String::new(),
        data,
    })
}

#[test]
fn deferred_tools_announce_additions_then_removals() {
    let tools = [ListingEntry {
        name: "WebFetch".to_owned(),
        line: "WebFetch".to_owned(),
    }];
    let first = deferred_tools(&[], &tools, None).unwrap();
    assert!(
        first
            .0
            .ends_with("to load tool schemas before calling them:\nWebFetch")
    );
    let history = [recorded(DEFERRED_TOOLS, first)];
    assert!(deferred_tools(&history, &tools, None).is_none());
    let (text, data) = deferred_tools(&history, &[], None).unwrap();
    assert!(text.starts_with("The following deferred tools are no longer available"));
    assert!(text.ends_with("directly relevant to their request."));
    assert_eq!(data["removedNames"][0], "WebFetch");
}

#[test]
fn agents_are_sorted_and_only_the_first_listing_has_the_concurrency_note() {
    let (text, data) = agents(&[], &[entry("Plan"), entry("helper"), entry("Explore")]).unwrap();
    assert_eq!(
        text,
        "Available agent types for the Agent tool:\n- Explore: does Explore\n- helper: does helper\n- Plan: does Plan\n\nWhen you launch multiple agents for independent work, send them in a single message with multiple tool uses so they run concurrently."
    );
    assert_eq!(data["isInitial"], true);
    let history = [recorded(AGENTS, (text, data))];
    let (text, _) = agents(
        &history,
        &[entry("Plan"), entry("helper"), entry("Explore"), entry("x")],
    )
    .unwrap();
    assert_eq!(
        text,
        "New agent types are now available for the Agent tool:\n- x: does x"
    );
    let (text, _) = agents(&history, &[entry("helper")]).unwrap();
    assert!(
        text.starts_with("The following agent types are no longer available:\n- Explore\n- Plan")
    );
}

#[test]
fn skills_announce_only_new_names() {
    let first = skills(&[], &[entry("a")]).unwrap();
    assert_eq!(
        first.0,
        "The following skills are available for use with the Skill tool:\n\n- a: does a"
    );
    let history = [recorded(SKILLS, first)];
    assert!(skills(&history, &[entry("a")]).is_none());
    let (text, data) = skills(&history, &[entry("a"), entry("b")]).unwrap();
    assert!(text.ends_with("\n\n- b: does b"));
    assert_eq!(data["isInitial"], false);
}

/// 连接中的 MCP 服务器（context.md §4.2，修复线 F36）。
#[test]
fn deferred_tools_announce_connecting_mcp_servers_and_their_change() {
    let tools = [ListingEntry {
        name: "WebFetch".to_owned(),
        line: "WebFetch".to_owned(),
    }];
    let slow = ["slow".to_owned()];
    let first = deferred_tools(&[], &tools, Some(&slow)).unwrap();
    assert_eq!(
        first.0,
        "The following deferred tools are now available via ToolSearch. Their schemas are NOT loaded \u{2014} calling them directly will fail with InputValidationError. Use ToolSearch with query \"select:<name>[,<name>...]\" to load tool schemas before calling them:\nWebFetch\n\nThe following MCP servers are still connecting \u{2014} their tools (typically named mcp__<server>__*) are not yet available but will appear shortly:\nslow\n\nIf the user's request might be served by one of these servers (even if they didn't name it explicitly), call ToolSearch with a relevant keyword \u{2014} ToolSearch will wait for connecting servers and search their tools once available. Do not report a capability as unavailable without first searching."
    );
    assert_eq!(first.1["pendingMcpServers"], serde_json::json!(["slow"]));
    // 没有变化：不再宣布。
    let history = vec![recorded(DEFERRED_TOOLS, first)];
    assert!(deferred_tools(&history, &tools, Some(&slow)).is_none());
    // 连上了（Pending 变空）：没有工具增减也发一条，文字为空，`pendingMcpServers` 是空数组。
    let (text, data) = deferred_tools(&history, &tools, Some(&[])).unwrap();
    assert_eq!(text, "");
    assert_eq!(data["pendingMcpServers"], serde_json::json!([]));
    assert_eq!(data["addedNames"], serde_json::json!([]));
    let mut later = history;
    later.push(recorded(DEFERRED_TOOLS, (text, data)));
    assert!(deferred_tools(&later, &tools, Some(&[])).is_none());
    // 新的服务器开始连（重连中的也算 Pending）：再宣布。
    let (text, _) = deferred_tools(&later, &tools, Some(&slow)).unwrap();
    assert!(text.starts_with("The following MCP servers are still connecting"));
    // 没有 MCP 状态（压缩附件等）：不带这一项。
    let (_, data) = deferred_tools(&[], &tools, None).unwrap();
    assert!(data.get("pendingMcpServers").is_none());
}

#[test]
fn more_than_thirty_connecting_servers_are_summarized() {
    let pending: Vec<String> = (0..32).map(|index| format!("s{index:02}")).collect();
    let (text, _) = deferred_tools(&[], &[], Some(&pending)).unwrap();
    assert!(text.contains("will appear shortly:\ns00, s01, "));
    assert!(text.contains("s28, s29, \u{2026}and 2 more\n\nIf the user's request"));
}

fn request(
    model: &str,
    names: &[&str],
    mcp: Option<rust_cli_kernel_api::mcp_state::McpView>,
) -> ContextRequest {
    use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
    use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
    ContextRequest {
        default_machine: None,
        capabilities: None,
        session: SessionId::new("sess_1"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        agent: None,
        source: QuerySource::MainThread,
        mode: rust_cli_kernel_api::permission::PermissionMode::Default,
        tool_names: names.iter().map(|name| (*name).to_owned()).collect(),
        selection: ModelSelection {
            provider: "anthropic".to_owned(),
            model: model.to_owned(),
            reasoning: None,
            connection: None,
        },
        plan_flags: Default::default(),
        mcp,
    }
}

#[test]
fn connecting_servers_are_announced_only_with_tool_search_and_a_snapshot() {
    use rust_cli_kernel_api::manage::McpServerState;
    use rust_cli_kernel_api::mcp_state::{McpServerView, McpView};
    let view = McpView {
        servers: vec![
            McpServerView {
                name: "zeta".to_owned(),
                state: McpServerState::Pending,
                reconnecting: true,
            },
            McpServerView {
                name: "alpha".to_owned(),
                state: McpServerState::Pending,
                reconnecting: false,
            },
            McpServerView {
                name: "done".to_owned(),
                state: McpServerState::Connected,
                reconnecting: false,
            },
        ],
        instructions: Vec::new(),
    };
    let on = request(GLM_5_3, &["Read", "ToolSearch"], Some(view.clone()));
    assert_eq!(pending_servers(&on).unwrap(), ["alpha", "zeta"]);
    // 工具里没有 ToolSearch、没有快照：都不带。
    assert!(pending_servers(&request(GLM_5_3, &["Read"], Some(view.clone()))).is_none());
    // 工作线 F89（tools-more.md §3.2.1）：不区分模型，以前禁用表里的模型也带。修改：F84 时不带。
    assert_eq!(
        pending_servers(&request(
            HAIKU_3_5_DATED,
            &["ToolSearch"],
            Some(view.clone())
        ))
        .unwrap(),
        ["alpha", "zeta"]
    );
    assert!(pending_servers(&request(GLM_5_3, &["ToolSearch"], None)).is_none());
    // 工作线 F84（tools-more.md §3.2）：本地 ToolSearch（没有卡片的模型、端点答不出能力）同样带。修改：以前不带。
    assert_eq!(
        pending_servers(&request(DEEPSEEK_V4_PRO, &["ToolSearch"], Some(view))).unwrap(),
        ["alpha", "zeta"]
    );
}
