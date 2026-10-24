//! 管理命令的文字（tui-display.md §2.3、§4.4、§4.9）。
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::manage::{BackgroundEntry, BackgroundKind, McpServerInfo, McpServerState};

use super::*;

fn entry(id: &str, kind: BackgroundKind, status: BackgroundStatus) -> BackgroundEntry {
    BackgroundEntry {
        id: id.into(),
        kind,
        description: id.into(),
        status,
        output: None,
        machine: None,
    }
}

#[test]
fn the_pill_counts_running_tasks_by_kind() {
    use BackgroundKind::{Agent, Shell};
    use BackgroundStatus::{Completed, Running};
    assert_eq!(pill(&[], Lang::En), None);
    assert_eq!(pill(&[entry("b1", Shell, Completed)], Lang::En), None);
    assert_eq!(
        pill(&[entry("b1", Shell, Running)], Lang::En).as_deref(),
        Some("1 shell")
    );
    let shells = [entry("b1", Shell, Running), entry("b2", Shell, Running)];
    assert_eq!(pill(&shells, Lang::En).as_deref(), Some("2 shells"));
    let agents = [entry("a1", Agent, Running)];
    assert_eq!(pill(&agents, Lang::En).as_deref(), Some("1 local agent"));
    let mixed = [entry("b1", Shell, Running), entry("a1", Agent, Running)];
    assert_eq!(
        pill(&mixed, Lang::En).as_deref(),
        Some("2 background tasks")
    );
    assert_eq!(pill(&mixed, Lang::Zh).as_deref(), Some("2 个后台任务"));
    assert_eq!(running(&mixed), 2);
}

#[test]
fn the_status_line_sums_up_mcp_servers() {
    let server = |name: &str, state| McpServerInfo {
        name: name.into(),
        state,
        reason: None,
        tools: 0,
    };
    assert_eq!(mcp_summary(&[], Lang::En), "none");
    let servers = [
        server("a", McpServerState::Connected),
        server("b", McpServerState::Failed),
        server("c", McpServerState::Connected),
    ];
    assert_eq!(mcp_summary(&servers, Lang::En), "2 connected, 1 failed");
    assert_eq!(
        effort_description("medium"),
        Some("Balanced approach with standard implementation and testing")
    );
    assert_eq!(effort_description("turbo"), None);
}
