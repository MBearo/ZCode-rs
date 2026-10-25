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
        reconnecting: false,
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
        machine: None,
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

/// 远端 MCP（R5，remote-exec.md §22.8）：`/mcp` 列表里绑定到远端机器的服务器名后面接 ` @机器`。
#[test]
fn the_mcp_list_names_the_machine_of_a_remote_server() {
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::testing::selection;

    use crate::ui::app::{App, Overlay};
    use crate::ui::palette::{ColorLevel, Palette};
    use crate::view::Transcript;

    let mut app = App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        None,
        "0.1.0".into(),
    );
    let server = |name: &str, machine: Option<&str>| McpServerInfo {
        name: name.into(),
        state: McpServerState::Connected,
        reason: None,
        tools: 1,
        machine: machine.map(str::to_owned),
    };
    app.open_mcp(&[server("browser-a", Some("dev-a")), server("docs", None)]);
    let Some(Overlay::Page {
        local: Local::Table { rows, .. },
        ..
    }) = &app.overlay
    else {
        panic!("no /mcp page");
    };
    let names: Vec<&str> = rows.iter().map(|(name, _, _)| name.as_str()).collect();
    assert_eq!(names, ["browser-a @dev-a", "docs"]);
}

/// F98（remote-exec.md §25.4.5，Docker 试用 L9）：在跑的子代理里有缺省机器在远端的，pill 不写“本地”；都在本机时照旧。
#[test]
fn the_pill_drops_local_for_agents_on_a_remote_machine() {
    use BackgroundKind::Agent;
    use BackgroundStatus::Running;
    let remote = BackgroundEntry {
        machine: Some("dev-a".into()),
        ..entry("a1", Agent, Running)
    };
    assert_eq!(
        pill(std::slice::from_ref(&remote), Lang::En).as_deref(),
        Some("1 agent")
    );
    assert_eq!(
        pill(std::slice::from_ref(&remote), Lang::Zh).as_deref(),
        Some("1 个代理")
    );
    let both = [remote, entry("a2", Agent, Running)];
    assert_eq!(pill(&both, Lang::En).as_deref(), Some("2 agents"));
    let local = [entry("a2", Agent, Running)];
    assert_eq!(pill(&local, Lang::En).as_deref(), Some("1 local agent"));
    assert_eq!(pill(&local, Lang::Zh).as_deref(), Some("1 个本地代理"));
}

/// F98（remote-exec.md §25.4.4，Docker 试用 L7）：`/mcp` 列表在中文界面用中文的状态词，机器没连上的原因写中文；英文不变。
#[test]
fn the_mcp_list_speaks_the_interface_language() {
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::testing::selection;

    use crate::ui::app::{App, Overlay};
    use crate::ui::palette::{ColorLevel, Palette};
    use crate::view::Transcript;

    let servers = [
        McpServerInfo {
            name: "devtools".into(),
            state: McpServerState::Failed,
            reason: Some("Machine \"dev-a\" is not connected.".into()),
            tools: 0,
            machine: Some("dev-a".into()),
        },
        McpServerInfo {
            name: "docs".into(),
            state: McpServerState::Connected,
            reason: None,
            tools: 2,
            machine: None,
        },
    ];
    let rows = |lang| {
        let mut app = App::new(
            Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
            lang,
            Transcript::new(
                selection(),
                PermissionMode::Default,
                crate::view::test_workspace(),
            ),
            None,
            "0.1.0".into(),
        );
        app.open_mcp(&servers);
        let Some(Overlay::Page {
            local: Local::Table { rows, .. },
            ..
        }) = app.overlay
        else {
            panic!("no /mcp page");
        };
        rows
    };
    assert_eq!(
        rows(Lang::Zh),
        [
            (
                "devtools @dev-a".to_owned(),
                "未连接".to_owned(),
                "机器 dev-a 没有连上。".to_owned()
            ),
            (
                "docs".to_owned(),
                "已连接".to_owned(),
                "2 个工具".to_owned()
            ),
        ]
    );
    assert_eq!(
        rows(Lang::En)[0],
        (
            "devtools @dev-a".to_owned(),
            "not connected".to_owned(),
            "Machine \"dev-a\" is not connected.".to_owned()
        )
    );
}
