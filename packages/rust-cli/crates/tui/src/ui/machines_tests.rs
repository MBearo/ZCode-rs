//! 远端执行 F98（docs/specs/rust-cli/remote-exec.md §25.4）：`/machines` 的立即重试与连接的分步、询问框的远端目录与 MCP
//! 的机器、错误的说明。界面状态在 `App` 里，这里不起事件循环（端到端的在 suite 的 `machines_more.rs`）。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::ids::{InteractionId, SessionId, ToolUseId};
use rust_cli_kernel_api::orchestration::{DetachedAsk, DetachedEvent, InteractionRequest};
use rust_cli_kernel_api::permission::{PermissionMode, PermissionRule};
use rust_cli_kernel_api::testing::selection;
use serde_json::json;

use super::*;
use crate::machines::{ConnectStep, ExecutorInfo};
use crate::ui::i18n::{EN, Lang};
use crate::ui::interaction_view::View;
use crate::ui::palette::{ColorLevel, Palette};
use crate::view::Transcript;

fn app(lang: Lang) -> App {
    let mut app = App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        lang,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn plain(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| crate::ui::text::plain(line).trim_end().to_owned())
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
    app.machines_command("");
    let lines = plain(&app.machines_lines(100, 30, 0));
    let hint = lines.last().unwrap();
    assert!(hint.contains("r to retry now"), "{hint}");
    assert_eq!(
        app.machines_key(key(KeyCode::Char('r'))),
        Some(Action::Machines(MachineRequest::Retry("dev-a".into())))
    );
    // 就绪的机器上 `r` 不做事，提示行也不写。
    app.machines_key(key(KeyCode::Down));
    assert_eq!(app.machines_key(key(KeyCode::Char('r'))), None);
    let lines = plain(&app.machines_lines(100, 30, 0));
    assert!(!lines.last().unwrap().contains("retry"), "{lines:?}");
}

/// §25.4.2：第一次连接时状态行与列表分出上传与握手。旧代码只写 `connecting`。
#[test]
fn the_first_connection_shows_its_steps() {
    for (lang, upload, handshake) in [
        (
            Lang::En,
            "○ dev-a connecting · uploading the executor (89 MB)",
            "○ dev-a connecting · handshaking",
        ),
        (
            Lang::Zh,
            "○ dev-a 连接中 · 上传执行器（89 MB）",
            "○ dev-a 连接中 · 握手",
        ),
    ] {
        let mut app = app(lang);
        let status = |app: &App| {
            crate::ui::text::plain(&app.machine_status_line(100, 0).unwrap())
                .trim()
                .to_owned()
        };
        app.machines = vec![machine(
            "dev-a",
            MachineStatus::Connecting(ConnectStep::Uploading { bytes: 93_323_264 }),
        )];
        assert_eq!(status(&app), format!("● local  {upload}"));
        app.machines[0].status = MachineStatus::Connecting(ConnectStep::Handshaking);
        assert_eq!(status(&app), format!("● local  {handshake}"));
        app.machines[0].status = MachineStatus::Connecting(ConnectStep::Checking);
        assert!(
            status(&app).ends_with("○ dev-a connecting")
                || status(&app).ends_with("○ dev-a 连接中")
        );
    }
}

/// §25.4.4（Docker 试用 L7）：列表里重连中的机器的最近一次错误去掉内部的错误链，中文写人话。
#[test]
fn the_last_error_is_said_plainly() {
    let mut app = app(Lang::Zh);
    app.machines = vec![machine("dev-a", waiting())];
    app.machines_command("");
    let lines = plain(&app.machines_lines(100, 30, 0));
    assert!(
        lines
            .iter()
            .any(|line| line
                == "    最近一次的错误：ssh 连不上 127.0.0.1:51110（Connection refused）"),
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
                asked_by: None,
                machine: machine.map(str::to_owned),
            },
        }),
    }
}

fn ask_lines(app: &mut App) -> Vec<String> {
    app.sync_interaction(Instant::now());
    let (_, request) = app.pending().unwrap();
    let request = request.clone();
    let view = View {
        palette: &app.palette,
        strings: &EN,
        lang: app.lang,
        width: 120,
        height: 40,
        now_ms: 0,
    };
    plain(&app.interaction.as_ref().unwrap().lines(&request, &view).0)
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
    let lines = ask_lines(&mut app);
    assert!(
        lines.iter().any(|line| line.contains("in /srv/proj")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("/Users/dev")),
        "{lines:#?}"
    );
}

/// §25.4.3（Docker 试用 L5）：MCP 工具的询问框照服务器的绑定写机器。旧代码只写“使用工具”。
#[test]
fn an_mcp_ask_names_the_machine_of_its_server() {
    for (lang, expected) in [
        (Lang::En, "Tool use · runs on dev-a"),
        (Lang::Zh, "使用工具 · 在 dev-a 上执行"),
    ] {
        let mut app = app(lang);
        app.mcp_machines = vec![("devtools".into(), "dev-a".into())];
        app.detached_changed(ask("mcp__devtools__where", json!({}), None));
        let lines = ask_lines(&mut app);
        assert!(lines[1].trim_start().starts_with(expected), "{lines:#?}");
    }
}
