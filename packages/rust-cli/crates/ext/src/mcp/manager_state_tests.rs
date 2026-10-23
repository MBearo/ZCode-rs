//! 连接状态的视图、快照与等待；`-p` 的启动等待（ext.md §3.3.1、§3.3.2、§3.8，修复线 F36）。
//! 时间是暂停的 tokio 时间，时钟跟着它走：截止时刻可以精确断言。

use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::manage::McpServerState;
use rust_cli_kernel_api::mcp::McpTransportConfig;
use serde_json::json;

use super::*;
use crate::clock_tests::TokioClock;
use crate::mcp::config::{McpServerOptions, McpSettings};
use crate::mcp::startup::McpStartup;
use crate::mcp::{McpScope, McpServer};
use crate::test_support::{FakeMcp, held, server, workspace};

fn config(name: &str, always_load: bool, remote: bool) -> McpServer {
    McpServer {
        name: name.to_owned(),
        scope: McpScope::Project,
        transport: if remote {
            McpTransportConfig::Http {
                url: "https://mcp.example.com/mcp".to_owned(),
                headers: Default::default(),
            }
        } else {
            McpTransportConfig::Stdio {
                command: "node".to_owned(),
                args: Vec::new(),
                env: Default::default(),
                cwd: None,
            }
        },
        options: McpServerOptions {
            timeout_ms: None,
            always_load,
        },
    }
}

fn tools_server() -> Arc<crate::test_support::Handler> {
    server(
        json!({"tools": {}}),
        Some("Fast instructions."),
        |method, _| {
            (method == "tools/list").then(|| {
                Ok(json!({"tools": [{"name": "lookup", "inputSchema": {"type": "object"}}]}))
            })
        },
    )
}

fn slow() -> Arc<crate::test_support::Handler> {
    held(|method, _| {
        (method == "tools/list")
            .then(|| Ok(json!({"tools": [{"name": "lookup", "inputSchema": {"type": "object"}}]})))
    })
}

struct Rig {
    fake: Arc<FakeMcp>,
    manager: McpManager,
    clock: Arc<TokioClock>,
    _scope: TaskScope,
}

fn rig(fake: FakeMcp, startup: McpStartup, servers: Vec<McpServer>) -> Rig {
    let fake = Arc::new(fake);
    let scope = TaskScope::root("test", |_| {});
    let clock = TokioClock::new();
    let settings = McpSettings {
        startup,
        ..McpSettings::default()
    };
    let manager = McpManager::new(fake.clone(), scope.spawner(), settings, clock.clone());
    manager.start(workspace(), servers);
    Rig {
        fake,
        manager,
        clock,
        _scope: scope,
    }
}

/// 过 `after_ms` 毫秒后完成 `name` 的握手。
fn release_after(rig: &Rig, name: &'static str, after_ms: u64) {
    let fake = rig.fake.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(after_ms)).await;
        fake.release(name, json!({"tools": {}}), Some("Slow instructions."));
    });
}

async fn print_wait(rig: &Rig) -> u64 {
    let start = rig.clock.now_ms();
    rig.manager.wait_for_print().await;
    rig.clock.now_ms() - start
}

#[tokio::test(start_paused = true)]
async fn print_waits_two_seconds_then_the_slow_server_joins_in_the_background() {
    let rig = rig(
        FakeMcp::default()
            .with("fast", tools_server())
            .with("slow", slow()),
        McpStartup::default(),
        vec![config("fast", false, false), config("slow", false, false)],
    );
    // 缺陷修复（F36）：原来等全部连上或失败；现在到 prewait 截止（缺省 2000 ms）就开跑。
    assert_eq!(print_wait(&rig).await, 2_000);
    let (tools, view) = rig.manager.snapshot();
    let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
    assert_eq!(names, ["mcp__fast__lookup"]);
    assert_eq!(view.pending(), ["slow"]);
    // 后台继续连：握手完成后工具与 instructions 一起出现。
    rig.fake
        .release("slow", json!({"tools": {}}), Some("Slow instructions."));
    let view = rig
        .manager
        .wait(1_000, &|view: &McpView| view.pending().is_empty())
        .await;
    assert!(view.pending().is_empty());
    let (tools, view) = rig.manager.snapshot();
    let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
    assert_eq!(names, ["mcp__fast__lookup", "mcp__slow__lookup"]);
    let servers: Vec<&str> = view
        .instructions
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(servers, ["fast", "slow"]);
}

#[tokio::test(start_paused = true)]
async fn print_returns_as_soon_as_every_server_settles() {
    let rig = rig(
        FakeMcp::default().with("slow", slow()).refusing("broken"),
        McpStartup::default(),
        vec![config("slow", false, false), config("broken", false, false)],
    );
    release_after(&rig, "slow", 700);
    assert_eq!(print_wait(&rig).await, 700);
}

#[tokio::test(start_paused = true)]
async fn always_load_servers_block_start_up_up_to_the_connect_timeout() {
    // 连上就不再等：① 在 3000 结束，② 没有 Pending 的服务器。
    let rig1 = rig(
        FakeMcp::default().with("slow", slow()),
        McpStartup::default(),
        vec![config("slow", true, false)],
    );
    release_after(&rig1, "slow", 3_000);
    assert_eq!(print_wait(&rig1).await, 3_000);
    // 没连上：① 到 5000（MCP_CONNECT_TIMEOUT_MS），② 从那时起再等 2000（prewait 在启动等待之后开始）。
    let rig2 = rig(
        FakeMcp::default().with("slow", slow()),
        McpStartup::default(),
        vec![config("slow", true, false)],
    );
    assert_eq!(print_wait(&rig2).await, 7_000);
}

#[tokio::test(start_paused = true)]
async fn blocking_start_up_waits_for_every_server_up_to_the_connect_timeout() {
    let startup = McpStartup {
        nonblocking: false,
        connect_wait_ms: 4_000,
        prewait_ms: 0,
        ..McpStartup::default()
    };
    let rig = rig(
        FakeMcp::default().with("slow", slow()),
        startup,
        vec![config("slow", false, false)],
    );
    assert_eq!(print_wait(&rig).await, 4_000);
}

#[tokio::test(start_paused = true)]
async fn servers_named_by_allow_rules_get_at_least_two_seconds() {
    // prewait 截止 500 ms（ZCODE_MCP_STARTUP_WAIT_MS）：点名的服务器再等到 2000。
    let startup = McpStartup {
        prewait_ms: 500,
        required: ["slow".to_owned()].into(),
        ..McpStartup::default()
    };
    let rig1 = rig(
        FakeMcp::default().with("slow", slow()),
        startup.clone(),
        vec![config("slow", false, false)],
    );
    assert_eq!(print_wait(&rig1).await, 2_000);
    // 没有点名的服务器只等到 500。
    let rig2 = rig(
        FakeMcp::default().with("other", slow()),
        startup,
        vec![config("other", false, false)],
    );
    assert_eq!(print_wait(&rig2).await, 500);
    // 截止为 0（host_no_wait）：不等。
    let rig3 = rig(
        FakeMcp::default().with("other", slow()),
        McpStartup {
            prewait_ms: 0,
            ..McpStartup::default()
        },
        vec![config("other", false, false)],
    );
    assert_eq!(print_wait(&rig3).await, 0);
}

/// stream-json 输入的 `lifted`（headless.md §6，修复线 F39）：② 只等 alwaysLoad 与 allow 规则点名的
/// 服务器，其余可延迟的服务器不等。
#[tokio::test(start_paused = true)]
async fn lifted_prewait_waits_only_for_servers_that_are_not_deferrable() {
    let lifted = McpStartup {
        wait_for_deferrable: false,
        ..McpStartup::default()
    };
    // 可延迟的服务器：不等。
    let rig1 = rig(
        FakeMcp::default().with("slow", slow()),
        lifted.clone(),
        vec![config("slow", false, false)],
    );
    assert_eq!(print_wait(&rig1).await, 0);
    // allow 规则点名的服务器：等到截止（2000）。
    let rig2 = rig(
        FakeMcp::default().with("named", slow()),
        McpStartup {
            required: ["named".to_owned()].into(),
            ..lifted.clone()
        },
        vec![config("named", false, false)],
    );
    assert_eq!(print_wait(&rig2).await, 2_000);
    // alwaysLoad 的服务器：① 等到 5000，② 再等到截止。
    let rig3 = rig(
        FakeMcp::default().with("eager", slow()),
        lifted,
        vec![config("eager", true, false)],
    );
    assert_eq!(print_wait(&rig3).await, 7_000);
}

#[tokio::test(start_paused = true)]
async fn wait_stops_at_the_deadline_with_the_last_view() {
    let rig = rig(
        FakeMcp::default().with("slow", slow()),
        McpStartup::default(),
        vec![config("slow", false, false)],
    );
    let start = rig.clock.now_ms();
    let view = rig
        .manager
        .wait(5_000, &|view: &McpView| view.pending().is_empty())
        .await;
    assert_eq!(rig.clock.now_ms() - start, 5_000);
    assert_eq!(view.pending(), ["slow"]);
    assert_eq!(view.servers[0].state, McpServerState::Pending);
    assert!(!view.servers[0].reconnecting);
}

#[tokio::test(start_paused = true)]
async fn a_dropped_remote_server_is_pending_and_reconnecting() {
    let rig = rig(
        FakeMcp::default().with("files", tools_server()),
        McpStartup::default(),
        vec![config("files", false, true)],
    );
    rig.manager.wait_ready().await;
    rig.fake.drop_server("files", false);
    let view = rig
        .manager
        .wait(500, &|view: &McpView| {
            view.servers[0].state == McpServerState::Pending
        })
        .await;
    assert_eq!(view.servers[0].state, McpServerState::Pending);
    assert!(view.servers[0].reconnecting);
    // 退避后重连成功：不再是重连中。
    let view = rig
        .manager
        .wait(5_000, &|view: &McpView| {
            view.servers[0].state == McpServerState::Connected
        })
        .await;
    assert!(!view.servers[0].reconnecting);
}

#[tokio::test(start_paused = true)]
async fn declared_is_per_session_and_cleared_when_the_session_closes() {
    let rig = rig(FakeMcp::default(), McpStartup::default(), Vec::new());
    let first = SessionId::new("sess_1");
    let second = SessionId::new("sess_2");
    rig.manager.declare_wait_tool(&first);
    assert!(rig.manager.declared(&first));
    assert!(!rig.manager.declared(&second));
    rig.manager.release(&first);
    assert!(!rig.manager.declared(&first));
}

#[tokio::test]
async fn a_direct_call_waits_at_most_ten_seconds_or_mcp_timeout() {
    let scope = TaskScope::root("test", |_| {});
    let manager = |connect_timeout_ms| {
        McpManager::new(
            Arc::new(FakeMcp::default()),
            scope.spawner(),
            McpSettings {
                connect_timeout_ms,
                ..McpSettings::default()
            },
            TokioClock::new(),
        )
    };
    assert_eq!(manager(30_000).tool_wait_ms(), 10_000);
    assert_eq!(manager(4_000).tool_wait_ms(), 4_000);
}
