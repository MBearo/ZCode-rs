//! shell DTO 的序列化往返（docs/specs/rust-cli/README.md §6 规则 8）。

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::*;
use crate::env::{CommandEvent, StartCommand};
use crate::permission::{PermissionRule, PermissionRules, RuleBehavior, RuleSource, SourcedRule};

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value, "{text}");
}

fn path(value: &str) -> EnvPath {
    EnvPath::new(value)
}

#[test]
fn start_command_round_trips_with_every_option() {
    round_trip(&StartCommand {
        command: "npm test".to_owned(),
        cwd: path("/w"),
        timeout_ms: Some(120_000),
        task: ShellTaskId::new("b0123abcd"),
        shell: ShellKind::Bash,
        cwd_scope: CwdScope::Session {
            roots: vec![path("/w"), path("/extra")],
        },
        background: Some(BackgroundCommand {
            description: "Run tests".to_owned(),
            tool_use: Some(ToolUseId::new("toolu_1")),
            kind: Default::default(),
        }),
        sandbox: Some(SandboxPolicy {
            filesystem: FilesystemPolicy {
                allow_write: vec![path("/w")],
                deny_write: vec![path("/w/.git/hooks")],
                deny_read: vec![path("/home/dev/.ssh")],
            },
            network: NetworkPolicy::Domains {
                allowed: vec!["example.com".to_owned()],
                denied: Vec::new(),
            },
            required: true,
        }),
        hook: Some(HookIo {
            stdin: "{\"hook_event_name\":\"PreToolUse\"}\n".to_owned(),
            env: [("ZCODE_PROJECT_DIR".to_owned(), "/w".to_owned())].into(),
        }),
        on_timeout: Some(TimeoutBackground {
            command: BackgroundCommand {
                description: "Run tests".to_owned(),
                tool_use: Some(ToolUseId::new("toolu_1")),
                kind: Default::default(),
            },
            deadline_ms: Some(600_000),
        }),
        socket: None,
        env: Default::default(),
    });
}

/// 工作线 F69：没有 `on_timeout` 的请求序列化后没有这个键（执行环境按请求的 JSON 认重放，旧请求的指纹不变）；
/// 转后台的事件能往返。
#[test]
fn timeout_backgrounding_is_optional_on_the_wire() {
    let start: StartCommand = serde_json::from_value(json!({
        "command": "ls", "cwd": "/w", "timeout_ms": 5, "task": "b1"
    }))
    .unwrap();
    assert!(start.on_timeout.is_none());
    let text = serde_json::to_string(&start).unwrap();
    assert!(!text.contains("on_timeout"), "{text}");
    round_trip(&CommandEvent::Backgrounded);
    assert_eq!(
        serde_json::to_value(CommandEvent::Backgrounded).unwrap(),
        json!({"type": "backgrounded"})
    );
}

#[test]
fn older_start_command_payloads_get_defaults() {
    let start: StartCommand = serde_json::from_value(json!({
        "command": "ls", "cwd": "/w", "timeout_ms": null, "task": "b1"
    }))
    .unwrap();
    assert_eq!(start.shell, ShellKind::Bash);
    assert_eq!(start.cwd_scope, CwdScope::Pinned);
    assert!(start.background.is_none() && start.sandbox.is_none() && start.hook.is_none());
}

#[test]
fn command_events_and_notices_round_trip() {
    round_trip(&CommandEvent::Started {
        output: path("/tmp/b1.output"),
    });
    round_trip(&CommandEvent::Finished {
        outcome: Outcome::Failed {
            code: Some("timeout".to_owned()),
            message: "Command timed out after 1s".to_owned(),
        },
        exit_code: Some(143),
        timed_out: true,
        cwd_reset: Some(path("/w")),
        output_bytes: 10,
        output_limit: None,
    });
    round_trip(&CommandEvent::Finished {
        outcome: Outcome::Cancelled,
        exit_code: Some(137),
        timed_out: false,
        cwd_reset: None,
        output_bytes: 6_000_000_000,
        output_limit: Some("Command killed: output file exceeded 5GB".to_owned()),
    });
    round_trip(&ShellNotice {
        seq: 3,
        operation: OperationId::new("op"),
        task: ShellTaskId::new("b1"),
        tool_use: None,
        description: "echo".to_owned(),
        output: path("/tmp/b1.output"),
        outcome: Outcome::Completed,
        exit_code: Some(0),
        kind: Default::default(),
        output_bytes: 0,
    });
    round_trip(&ShellState {
        cwd: path("/w/sub"),
        outputs: Some(path("/tmp/zcode-shell/sessions/s1/tasks")),
        background: vec![ShellTaskId::new("b1")],
    });
    round_trip(&NetworkPolicy::LocalOnly {
        allow_local_binding: true,
    });
}

#[test]
fn permission_rules_round_trip() {
    round_trip(&PermissionRules {
        version: 2,
        rules: vec![SourcedRule {
            source: RuleSource::LocalSettings,
            behavior: RuleBehavior::Allow,
            rule: PermissionRule {
                tool: "Bash".to_owned(),
                content: Some("git status:*".to_owned()),
            },
        }],
        additional_directories: vec![path("/extra")],
        avoid_prompts: true,
    });
}

#[test]
fn background_task_dtos_round_trip() {
    round_trip(&BackgroundTask {
        session: SessionId::new("s1"),
        task: ShellTaskId::new("b0123abcd"),
    });
    round_trip(&Some(BackgroundInfo {
        command: "sleep 30".to_owned(),
        status: BackgroundStatus::Running,
        kind: Default::default(),
        description: String::new(),
    }));
    round_trip(&BackgroundStop::Stopped {
        command: "sleep 30".to_owned(),
    });
    round_trip(&BackgroundStop::NotRunning {
        status: BackgroundStatus::Failed,
    });
    round_trip(&BackgroundStop::NotFound);
    assert_eq!(
        serde_json::to_value(BackgroundStop::NotFound).unwrap(),
        json!({"kind": "not_found"})
    );
}

/// 修复线 F117：`BackgroundInfo` 的种类与说明。没有这两项的 JSON（旧执行器、F117 之前写的）解成 Bash 与空；Bash 与空
/// 不写出；monitor 往返不变。
#[test]
fn background_info_carries_the_kind_and_the_description() {
    let old: BackgroundInfo =
        serde_json::from_value(json!({"command": "sleep 30", "status": "running"})).unwrap();
    assert_eq!(
        (old.kind, old.description.as_str()),
        (BackgroundKind::Bash, "")
    );
    assert_eq!(
        serde_json::to_value(&old).unwrap(),
        json!({"command": "sleep 30", "status": "running"})
    );
    let monitor = BackgroundInfo {
        command: "tail -f build.log".to_owned(),
        status: BackgroundStatus::Running,
        kind: BackgroundKind::Monitor,
        description: "build log".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(&monitor).unwrap(),
        json!({
            "command": "tail -f build.log", "status": "running", "kind": "monitor",
            "description": "build log"
        })
    );
    round_trip(&monitor);
}

#[test]
fn ended_status_follows_the_notification_rule() {
    let failed = Outcome::Failed {
        code: Some("timeout".to_owned()),
        message: "timed out".to_owned(),
    };
    assert_eq!(
        BackgroundStatus::ended(&Outcome::Completed, Some(0)),
        BackgroundStatus::Completed
    );
    assert_eq!(
        BackgroundStatus::ended(&Outcome::Completed, Some(2)),
        BackgroundStatus::Failed
    );
    assert_eq!(
        BackgroundStatus::ended(&failed, Some(143)),
        BackgroundStatus::Failed
    );
    assert_eq!(
        BackgroundStatus::ended(&Outcome::Cancelled, Some(137)).as_str(),
        "killed"
    );
}

#[test]
fn older_finished_payloads_have_no_output_limit() {
    // F11：`output_limit` 是后加的字段，没有它的旧载荷照常解析，也不写出这个键。
    let finished: CommandEvent = serde_json::from_value(json!({
        "type": "finished", "outcome": {"status": "completed"}, "exit_code": 0
    }))
    .unwrap();
    let CommandEvent::Finished { output_limit, .. } = &finished else {
        panic!("not finished");
    };
    assert_eq!(output_limit, &None);
    assert!(
        !serde_json::to_string(&finished)
            .unwrap()
            .contains("output_limit")
    );
}

/// 工作线 F83：Monitor 的种类、WebSocket 来源与生命周期能序列化往返；缺省的 Bash 种类与 0 字节不写出，旧载荷照读。
#[test]
fn monitor_payloads_round_trip_and_older_ones_still_parse() {
    let background = BackgroundCommand {
        description: "errors in deploy.log".to_owned(),
        tool_use: Some(ToolUseId::new("toolu_1")),
        kind: BackgroundKind::Monitor,
    };
    round_trip(&background);
    assert_eq!(
        serde_json::to_value(&background).unwrap()["kind"],
        json!("monitor")
    );
    let plain: BackgroundCommand =
        serde_json::from_value(json!({"description": "Run tests"})).unwrap();
    assert_eq!(plain.kind, BackgroundKind::Bash);
    assert!(!serde_json::to_string(&plain).unwrap().contains("kind"));
    round_trip(&MonitorSocket {
        url: "wss://events.example.com/stream".to_owned(),
        protocols: vec!["v1".to_owned()],
    });
    for event in [
        SocketEvent::Error {
            message: "Failed to connect".to_owned(),
        },
        SocketEvent::Rejected { status: 403 },
        SocketEvent::Dropped { bytes: 2_000_000 },
        SocketEvent::Closed {
            code: 1000,
            reason: String::new(),
        },
    ] {
        round_trip(&event);
    }
    assert_eq!(
        serde_json::to_value(SocketEvent::Closed {
            code: 1006,
            reason: "Failed to connect".to_owned(),
        })
        .unwrap(),
        json!({"type": "closed", "code": 1006, "reason": "Failed to connect"})
    );
    let notice: ShellNotice = serde_json::from_value(json!({
        "seq": 1, "operation": "op", "task": "b1", "description": "d",
        "output": "/t/b1.output", "outcome": {"status": "completed"}, "exit_code": 0
    }))
    .unwrap();
    assert_eq!(
        (notice.kind, notice.output_bytes),
        (BackgroundKind::Bash, 0)
    );
    let text = serde_json::to_string(&notice).unwrap();
    assert!(
        !text.contains("kind") && !text.contains("output_bytes"),
        "{text}"
    );
    round_trip(&crate::monitor::MonitorArmed {
        machine: None,
        operation: crate::ids::OperationId::new("op_1"),
        task: crate::ids::ShellTaskId::new("b1"),
        tool_use: ToolUseId::new("toolu_1"),
        description: "errors in deploy.log".to_owned(),
        timeout_ms: 300_000,
        source: crate::monitor::MonitorSource::Socket,
        output: crate::env::EnvPath::new("/t/b1.output"),
    });
}

#[test]
fn start_command_overrides_are_not_serialized() {
    // F96（remote-exec.md §23.5）：环境的覆盖只在执行器里由服务端填，不进序列化（也就不进 env-local 的去重指纹）。
    let mut start: StartCommand = serde_json::from_value(json!({
        "command": "true",
        "cwd": "/w",
        "timeout_ms": null,
        "task": "b1",
    }))
    .unwrap();
    assert!(start.env.is_empty());
    let plain = serde_json::to_string(&start).unwrap();
    start
        .env
        .insert("SSH_AUTH_SOCK".to_owned(), Some("/tmp/a".to_owned()));
    start.env.insert("DISPLAY".to_owned(), None);
    assert_eq!(serde_json::to_string(&start).unwrap(), plain);
}
