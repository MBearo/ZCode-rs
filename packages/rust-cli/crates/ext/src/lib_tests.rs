//! 组装：注册表、清单来源、MCP 启动（ext.md §1）。

use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::mcp::McpTransportConfig;
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{ListTools, MemoryEnvironment, SequentialIds};
use serde_json::json;

use super::*;
use crate::test_support::{CWD, EchoBash, FakeMcp, facts, server, workspace};

fn ext(env: MemoryEnvironment, fake: Arc<FakeMcp>, scope: &TaskScope, servers: McpServers) -> Ext {
    let base: Arc<dyn ToolRegistry> = Arc::new(ListTools(vec![Arc::new(EchoBash::default())]));
    let env = Arc::new(env);
    let ports = ExtPorts {
        info: env.clone(),
        files: env,
        ids: Arc::new(SequentialIds::default()),
        gate: Arc::new(PolicyGate::default()),
        mcp: fake,
        machines: None,
        spawner: scope.spawner(),
        clock: crate::clock_tests::TokioClock::new(),
    };
    let config = ExtConfig {
        mcp: McpSettings {
            servers,
            ..McpSettings::default()
        },
        workspace: Some(workspace()),
        ..ExtConfig::default()
    };
    Ext::new(ports, base, config)
}

fn request() -> ContextRequest {
    ContextRequest {
        default_machine: None,
        capabilities: None,
        session: SessionId::new("sess_1"),
        workspace: workspace(),
        agent: None,
        source: QuerySource::MainThread,
        mode: PermissionMode::Default,
        tool_names: vec!["Skill".to_owned()],
        selection: ModelSelection {
            provider: "anthropic".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            reasoning: None,
            connection: None,
        },
        plan_flags: Default::default(),
        mcp: None,
    }
}

#[tokio::test]
async fn the_registry_adds_skill_and_mcp_tools_after_the_base() {
    let fake = Arc::new(FakeMcp::default().with(
        "files",
        server(json!({"tools": {}}), Some("Read files."), |method, _| {
            (method == "tools/list").then(|| {
                Ok(json!({"tools": [{"name": "read", "inputSchema": {"type": "object"}}]}))
            })
        }),
    ));
    let scope = TaskScope::root("test", |_| {});
    let env = MemoryEnvironment::new(facts()).with_file(
        &format!("{CWD}/.zcode/skills/greet/SKILL.md"),
        "---\ndescription: Greet\n---\n",
    );
    let servers = vec![McpServer {
        name: "files".to_owned(),
        scope: McpScope::Project,
        transport: McpTransportConfig::Stdio {
            command: "node".to_owned(),
            args: Vec::new(),
            env: Default::default(),
            cwd: None,
        },
        options: Default::default(),
        machine: None,
    }];
    let ext = ext(env, fake, &scope, servers);
    let registry = ext.registry();
    let names = |registry: &Arc<dyn ToolRegistry>| -> Vec<String> {
        registry
            .tools()
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect()
    };
    assert_eq!(names(&registry), ["Bash", "Skill"]);
    ext.start().await;
    ext.mcp().wait_ready().await;
    assert_eq!(names(&registry), ["Bash", "Skill", "mcp__files__read"]);
    assert!(registry.find("mcp__files__read").is_some());
    let listings = ext.listings();
    let found = listings.listings_for(&request(), &[]).await.unwrap();
    let lines: Vec<&str> = found
        .skills
        .iter()
        .map(|entry| entry.line.as_str())
        .collect();
    // 文件 skill 在前，内置的 `loop` 在后（tools-more.md §17.1），再是内置提示词命令（修复线 L-A）。
    assert_eq!(lines[0], "- greet: Greet");
    assert!(lines[1].starts_with("- loop: "));
    assert!(lines[2].starts_with("- init: "));
    assert!(lines[3].starts_with("- security-review: "));
    assert_eq!(lines.len(), 4);
    let instructions = listings.mcp_instructions(&request()).await.unwrap();
    assert_eq!(instructions[0].line, "## files\nRead files.");
}

/// WaitForMcpServers 在有连接中的服务器时出现；主线程带过它的会话之后一直带着（ext.md §3.8，修复线 F36）。快照里的
/// instructions 与工具同一时点，清单来源按快照给（query.md §1.3）。
#[tokio::test(start_paused = true)]
async fn the_wait_tool_follows_pending_servers_and_the_declared_session() {
    let fake = Arc::new(FakeMcp::default().with("slow", crate::test_support::held(|_, _| None)));
    let scope = TaskScope::root("test", |_| {});
    let servers = vec![McpServer {
        name: "slow".to_owned(),
        scope: McpScope::Project,
        transport: McpTransportConfig::Stdio {
            command: "node".to_owned(),
            args: Vec::new(),
            env: Default::default(),
            cwd: None,
        },
        options: Default::default(),
        machine: None,
    }];
    let ext = ext(
        MemoryEnvironment::new(facts()),
        fake.clone(),
        &scope,
        servers,
    );
    let registry = ext.registry();
    ext.start().await;
    let names = |tools: Vec<Arc<dyn Tool>>| -> Vec<String> {
        tools.iter().map(|tool| tool.name().to_owned()).collect()
    };
    assert_eq!(
        names(registry.tools()),
        ["Bash", "Skill", "WaitForMcpServers"]
    );
    let main = SessionId::new("sess_1");
    let other = SessionId::new("sess_2");
    let snapshot = registry.snapshot(&main);
    assert_eq!(
        names(snapshot.tools),
        ["Bash", "Skill", "WaitForMcpServers"]
    );
    assert_eq!(snapshot.mcp.unwrap().pending(), ["slow"]);
    registry.mcp().unwrap().declare_wait_tool(&main);
    fake.release("slow", json!({}), Some("Slow."));
    ext.mcp().wait_ready().await;
    // 连上之后：现在的工具表没有它；带过它的会话的快照仍有，别的会话没有。
    assert_eq!(names(registry.tools()), ["Bash", "Skill"]);
    let declared = registry.snapshot(&main);
    assert_eq!(
        names(declared.tools),
        ["Bash", "Skill", "WaitForMcpServers"]
    );
    assert_eq!(names(registry.snapshot(&other).tools), ["Bash", "Skill"]);
    assert!(registry.find("WaitForMcpServers").is_some());
    // 清单来源：有快照时用快照里的 instructions，没有时取现在的。
    let listings = ext.listings();
    let mut stale = request();
    stale.mcp = Some(rust_cli_kernel_api::mcp_state::McpView::default());
    assert!(listings.mcp_instructions(&stale).await.unwrap().is_empty());
    let mut fresh = request();
    fresh.mcp = declared.mcp;
    let lines: Vec<String> = listings
        .mcp_instructions(&fresh)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.line)
        .collect();
    assert_eq!(lines, ["## slow\nSlow."]);
    assert_eq!(
        listings.mcp_instructions(&request()).await.unwrap()[0].line,
        "## slow\nSlow."
    );
}
