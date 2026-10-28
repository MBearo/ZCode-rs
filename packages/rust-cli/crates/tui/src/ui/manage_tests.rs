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
fn the_pill_counts_only_the_running_shells() {
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
    // 混合时只数命令（用户决定 2026-10-27）；`running` 仍数全部在跑的条目。
    let mixed = [entry("b1", Shell, Running), entry("a1", Agent, Running)];
    assert_eq!(pill(&mixed, Lang::En).as_deref(), Some("1 shell"));
    assert_eq!(pill(&mixed, Lang::Zh).as_deref(), Some("1 个 shell"));
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

/// 用户决定（tui-display.md §2.3、D25）：子代理不进 pill——子代理列表已经逐行列出。全是子代理时没有 pill（缺省机器
/// 在远端的子代理也一样，原来写 `N agents`）；混合时只数在跑的命令（原来写 `N background tasks`）。
#[test]
fn there_is_no_pill_when_only_subagents_run() {
    use BackgroundKind::{Agent, Shell};
    use BackgroundStatus::{Completed, Running};
    let local = [entry("a1", Agent, Running), entry("a2", Agent, Running)];
    let remote = BackgroundEntry {
        machine: Some("dev-a".into()),
        ..entry("a3", Agent, Running)
    };
    for lang in [Lang::En, Lang::Zh] {
        assert_eq!(pill(&local, lang), None);
        assert_eq!(pill(std::slice::from_ref(&remote), lang), None);
        assert_eq!(pill(&[remote.clone(), local[0].clone()], lang), None);
    }
    // 一个子代理加一个已结束的命令：只数在跑的，仍然全是子代理。
    assert_eq!(
        pill(
            &[entry("b1", Shell, Completed), entry("a1", Agent, Running)],
            Lang::En
        ),
        None
    );
    // 混合（含远端的子代理）：只数命令。
    let mixed = [
        entry("b1", Shell, Running),
        remote,
        entry("a1", Agent, Running),
        entry("b2", Shell, Running),
    ];
    assert_eq!(pill(&mixed, Lang::En).as_deref(), Some("2 shells"));
    assert_eq!(pill(&mixed, Lang::Zh).as_deref(), Some("2 个 shell"));
    // 一个在跑的命令、一个已结束的子代理：全是命令。
    assert_eq!(
        pill(
            &[entry("b1", Shell, Running), entry("a1", Agent, Completed)],
            Lang::Zh
        )
        .as_deref(),
        Some("1 个 shell")
    );
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
