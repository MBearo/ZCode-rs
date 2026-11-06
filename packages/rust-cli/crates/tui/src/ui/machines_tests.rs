//! 远端执行 F98（docs/specs/rust-cli/remote-exec.md §25.4）：`/machines` 的立即重试与连接的分步、询问框的远端目录与 MCP
//! 的机器、错误的说明。界面状态在 `App` 里，这里不起事件循环（端到端的在 suite 的 `machines_more.rs`）。
use std::time::Instant;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::{InteractionId, SessionId, ToolUseId};
use rust_cli_kernel_api::orchestration::{DetachedAsk, DetachedEvent, InteractionRequest};
use rust_cli_kernel_api::permission::PermissionRule;
use serde_json::json;

use super::*;
use crate::machines::{ConnectStep, ExecutorInfo};
use crate::ui::i18n::Lang;
use crate::ui::test_support::key;
use rust_cli_tui_kit::render::StyledLine;

fn app(lang: Lang) -> App {
    let mut app = crate::ui::test_support::app_in(lang);
    app.machines_enabled = true;
    app
}

fn machine(name: &str, status: MachineStatus) -> MachineRow {
    MachineRow {
        name: name.into(),
        ssh: name.into(),
        working_directory: None,
        status,
        latency_ms: None,
        executor: None,
    }
}

fn waiting() -> MachineStatus {
    MachineStatus::Reconnecting {
        attempt: 6,
        retry_at_ms: Some(16_000),
        reason: "cannot reach the executor: link io error: executor exited (exit status: 255): ssh: connect to host 127.0.0.1 port 51110: Connection refused".into(),
    }
}

fn plain(lines: &[StyledLine]) -> Vec<String> {
    lines
        .iter()
        .map(|line| crate::ui::lines::plain(line).trim_end().to_owned())
        .collect()
}

/// §25.4.1：`r` 与 `/machines connect` 对重连中的机器是立即重试；提示行在等重连时多 `r to retry now`。旧代码没有 `r`，
/// connect 回“已经连着”。
#[test]
fn a_machine_waiting_to_reconnect_can_be_retried_now() {
    let mut app = app(Lang::En);
    app.machines = vec![
        machine("dev-a", waiting()),
        machine("gpu-1", MachineStatus::Ready),
    ];
    assert_eq!(
        app.machines_command("connect dev-a"),
        Some(Action::Machines(MachineRequest::Retry("dev-a".into())))
    );
    assert_eq!(
        app.machines_command("connect gpu-1"),
        Some(Action::Machines(MachineRequest::Connect("gpu-1".into())))
    );
    assert_eq!(
        app.machines_command(""),
        Some(Action::Machines(MachineRequest::Hosts))
    );
    // 槽照 Puffin `machines_slot`：空格做随状态的操作，重连中的是立即重试（tui-kit.md §4.2「`/machines`」）。
    let hint: String = app.machines_slot().iter().map(|span| &*span.text).collect();
    assert!(hint.contains("space retry now"), "{hint}");
    assert_eq!(
        app.machines_key(key(KeyCode::Char(' '))),
        Some(Action::Machines(MachineRequest::Retry("dev-a".into())))
    );
    // 旧界面的 `r` 留着；就绪的机器上不做事，空格是断开。
    assert_eq!(
        app.machines_key(key(KeyCode::Char('r'))),
        Some(Action::Machines(MachineRequest::Retry("dev-a".into())))
    );
    app.machines_key(key(KeyCode::Down));
    assert_eq!(app.machines_key(key(KeyCode::Char('r'))), None);
    let hint: String = app.machines_slot().iter().map(|span| &*span.text).collect();
    assert!(hint.contains("space disconnect"), "{hint}");
}

/// ⏎ 打开设置里的那一页（Puffin `machines_key`）：机器是它的页，`+ Add a machine` 是新机器的页；↑↓ 循环。以前 ⏎ 随状态
/// 连或断，没有 `+ Add a machine`。
#[test]
fn enter_opens_the_machine_in_settings() {
    let mut app = app(Lang::En);
    app.machines = vec![machine("dev-a", MachineStatus::Ready)];
    app.machines_command("");
    assert_eq!(
        app.machine_entries(),
        [
            super::super::machines_list::Entry::Machine("dev-a".into()),
            super::super::machines_list::Entry::Add
        ]
    );
    let lines = plain(&app.machines_lines(100, 0));
    assert!(
        lines.iter().any(|l| l.contains("+ Add a machine")),
        "{lines:#?}"
    );
    // ↑ 从第一项绕到最后一项。
    app.machines_key(key(KeyCode::Up));
    let hint: String = app.machines_slot().iter().map(|span| &*span.text).collect();
    assert!(hint.starts_with("⏎ add"), "{hint}");
    app.machines_key(key(KeyCode::Enter));
    // 同 Puffin：设置盖在上面，列表留在下面（关上设置后回到列表）。
    assert!(app.settings.is_some());
    assert!(matches!(app.overlay, Some(Overlay::Machines(_))));
}

/// §25.4.2：第一次连接时状态行与列表分出上传与握手。旧代码只写 `connecting`。
#[test]
fn the_first_connection_shows_its_steps() {
    // 坞行（tui-kit.md §4.0 第 6 条，Puffin `machines_row`）：这一步、上传的大小、连了多久；以前状态栏下面一行从 `● local`
    // 起列出每台。
    for (lang, upload, handshake, checking) in [
        (
            Lang::En,
            "○ dev-a  uploading the executor (89 MB) · 0s",
            "○ dev-a  handshaking · 0s",
            "○ dev-a  checking · 0s",
        ),
        (
            Lang::Zh,
            "○ dev-a  上传执行器（89 MB） · 0s",
            "○ dev-a  握手 · 0s",
            "○ dev-a  检查中 · 0s",
        ),
    ] {
        let mut app = app(lang);
        let status = |app: &App| {
            let row = app.machines_row(0).expect("a machine is changing");
            crate::ui::lines::plain(&rust_cli_tui_kit::app::layout::Layout::lay(&row, 100)[0])
                .trim()
                .to_owned()
        };
        app.machines = vec![machine(
            "dev-a",
            MachineStatus::Connecting(ConnectStep::Uploading { bytes: 93_323_264 }),
        )];
        assert_eq!(status(&app), upload);
        app.machines[0].status = MachineStatus::Connecting(ConnectStep::Handshaking);
        assert_eq!(status(&app), handshake);
        app.machines[0].status = MachineStatus::Connecting(ConnectStep::Checking);
        assert_eq!(status(&app), checking);
        // 就绪与没连的不写：都不在变时没有这一行。
        app.machines[0].status = MachineStatus::Ready;
        assert!(app.machines_row(0).is_none());
    }
}

/// §25.4.4（Docker 试用 L7）：列表里重连中的机器的最近一次错误去掉内部的错误链（字照 Puffin，同设置的 Machines 段）。
#[test]
fn the_last_error_is_said_plainly() {
    let mut app = app(Lang::En);
    app.machines = vec![machine("dev-a", waiting())];
    app.machines_command("");
    let lines = plain(&app.machines_lines(100, 0));
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start().starts_with("Last error: ")
                && !line.contains("link io error")),
        "{lines:#?}"
    );
}

fn ask(tool: &str, input: serde_json::Value, machine: Option<&str>) -> DetachedEvent {
    DetachedEvent::Asked {
        ask: Box::new(DetachedAsk {
            interaction: InteractionId::new("i1"),
            session: SessionId::new("sess_old"),
            title: String::new(),
            request: InteractionRequest::Permission {
                tool_use: ToolUseId::new("tu1"),
                tool: tool.into(),
                input,
                message: "needs approval".into(),
                // 两条建议：没有可编辑的种子，这一行列出命令与目录（shell-permissions.md §4.10）。
                suggestions: vec![
                    PermissionRule {
                        tool: "Bash".into(),
                        content: Some("mkdir:*".into()),
                    },
                    PermissionRule {
                        tool: "Bash".into(),
                        content: Some("awk:*".into()),
                    },
                ],
                rule_seed: None,
                asked_by: None,
                machine: rust_cli_kernel_api::ids::MachineName::new(machine.unwrap_or_default()),
            },
        }),
    }
}

/// 打开询问：在回答的请求的工作目录与绑定的机器（“不再询问”的规则按它写；板上的字照 Puffin，不写目录与机器）。
fn asked(app: &mut App) -> (String, Option<String>) {
    app.sync_interaction(Instant::now());
    let ui = app.interaction.as_ref().unwrap();
    (
        ui.workspace.clone(),
        ui.bound_machine.remote().map(str::to_owned),
    )
}

/// §25.4.3（Docker 试用 L4）：远端 Bash 的“不再询问”写那台机器的工作目录（清单的；没写时握手报告的 home）。旧代码写本机
/// 的工作区。
#[test]
fn a_remote_bash_ask_names_the_remote_directory() {
    let mut app = app(Lang::En);
    let mut dev = machine("dev-a", MachineStatus::Ready);
    dev.working_directory = Some("/srv/proj".into());
    let mut gpu = machine("gpu-1", MachineStatus::Ready);
    gpu.executor = Some(ExecutorInfo {
        os: "linux".into(),
        arch: "aarch64".into(),
        version: "0.1.61".into(),
        refreshes_env: true,
        home: Some("/home/dev".into()),
    });
    app.machines = vec![dev, gpu];
    app.detached_changed(ask(
        "Bash",
        json!({"command": "mkdir -p out && awk 1 a > out/b"}),
        Some("dev-a"),
    ));
    assert_eq!(asked(&mut app).0, "/srv/proj");
}

/// §25.4.3（Docker 试用 L5）：MCP 工具的询问照服务器的绑定找机器。
#[test]
fn an_mcp_ask_names_the_machine_of_its_server() {
    let mut app = app(Lang::En);
    app.mcp_machines = vec![("devtools".into(), "dev-a".into())];
    app.detached_changed(ask("mcp__devtools__where", json!({}), None));
    assert_eq!(asked(&mut app).1.as_deref(), Some("dev-a"));
}
