//! 管理查询 DTO 的序列化往返（spec §6 规则 8）。
use super::*;

#[test]
fn entries_round_trip() {
    let entry = BackgroundEntry {
        id: "b1".to_owned(),
        kind: BackgroundKind::Shell,
        description: "sleep 10".to_owned(),
        status: BackgroundStatus::Running,
        output: Some("tick".to_owned()),
        machine: None,
        reconnecting: false,
    };
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["kind"], "shell");
    assert_eq!(json["status"], "running");
    // 本机的条目不写 `machine`（远端执行 R4，remote-exec.md §21.7）。
    assert!(json.get("machine").is_none());
    assert_eq!(
        serde_json::from_value::<BackgroundEntry>(json).unwrap(),
        entry
    );
    let remote = BackgroundEntry {
        machine: Some("dev-a".to_owned()),
        ..entry
    };
    let json = serde_json::to_value(&remote).unwrap();
    assert_eq!(json["machine"], "dev-a");
    // 为假时不写出，R4 写出的条目照常能读；为真时写出（远端执行 F96，remote-exec.md §23.3）。
    assert!(json.get("reconnecting").is_none());
    assert_eq!(
        serde_json::from_value::<BackgroundEntry>(json).unwrap(),
        remote
    );
    let stale = BackgroundEntry {
        reconnecting: true,
        ..remote
    };
    let json = serde_json::to_value(&stale).unwrap();
    assert_eq!(json["reconnecting"], true);
    assert_eq!(
        serde_json::from_value::<BackgroundEntry>(json).unwrap(),
        stale
    );

    let server = McpServerInfo {
        name: "docs".to_owned(),
        state: McpServerState::Failed,
        reason: Some("spawn failed".to_owned()),
        tools: 0,
        machine: None,
    };
    let json = serde_json::to_value(&server).unwrap();
    assert_eq!(json["state"], "failed");
    assert!(json.get("machine").is_none());
    assert_eq!(
        serde_json::from_value::<McpServerInfo>(json).unwrap(),
        server
    );
    // 绑定到远端机器的服务器（R5，remote-exec.md §22.2）。
    let remote = McpServerInfo {
        machine: Some("dev-a".to_owned()),
        ..server
    };
    let json = serde_json::to_value(&remote).unwrap();
    assert_eq!(json["machine"], "dev-a");
    assert_eq!(
        serde_json::from_value::<McpServerInfo>(json).unwrap(),
        remote
    );
}

#[test]
fn hook_entries_round_trip() {
    let entry = HookEntry {
        event: "PreToolUse".to_owned(),
        matcher: Some("Bash".to_owned()),
        kind: "command".to_owned(),
        display: "echo hi".to_owned(),
        source: "user".to_owned(),
    };
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(serde_json::from_value::<HookEntry>(json).unwrap(), entry);
}

#[test]
fn export_files_round_trip() {
    let file = ExportFile {
        workspace: crate::env::WorkspaceRef {
            environment: crate::ids::EnvironmentId::new("local"),
            root: crate::env::EnvPath::new("/Users/dev/project"),
        },
        path: "notes/chat".to_owned(),
        text: "❯ hi\n".to_owned(),
    };
    let json = serde_json::to_value(&file).unwrap();
    assert_eq!(json["path"], "notes/chat");
    assert_eq!(serde_json::from_value::<ExportFile>(json).unwrap(), file);
}

#[test]
fn mcp_controls_round_trip() {
    let control = McpControl {
        action: McpAction::Disable,
        servers: vec!["docs".to_owned()],
    };
    let json = serde_json::to_value(&control).unwrap();
    assert_eq!(json["action"], "disable");
    assert_eq!(serde_json::from_value::<McpControl>(json).unwrap(), control);
    assert_eq!(
        serde_json::to_value(McpServerState::Disabled).unwrap(),
        "disabled"
    );
}

#[test]
fn directory_changes_round_trip() {
    let request = ChangeDirectory {
        workspace: crate::env::WorkspaceRef {
            environment: crate::ids::EnvironmentId::new("local"),
            root: crate::env::EnvPath::new("/Users/dev/project"),
        },
        session: None,
        path: "../other".to_owned(),
    };
    let json = serde_json::to_value(&request).unwrap();
    assert!(json.get("session").is_none());
    assert_eq!(
        serde_json::from_value::<ChangeDirectory>(json).unwrap(),
        request
    );
    let command = crate::session_api::Command::SetWorkspace {
        root: crate::env::EnvPath::new("/Users/dev/other"),
        note: Some("moved".to_owned()),
    };
    let json = serde_json::to_value(&command).unwrap();
    assert_eq!(json["type"], "set_workspace");
    assert_eq!(
        serde_json::from_value::<crate::session_api::Command>(json).unwrap(),
        command
    );
    let command = crate::session_api::Command::ChangeDirectory {
        root: crate::env::EnvPath::new("/Users/dev/other"),
    };
    let json = serde_json::to_value(&command).unwrap();
    assert_eq!(json["type"], "change_directory");
    assert_eq!(
        serde_json::from_value::<crate::session_api::Command>(json).unwrap(),
        command
    );
    for outcome in [
        DirectoryOutcome::Moved,
        DirectoryOutcome::Unchanged,
        DirectoryOutcome::Failed {
            current: crate::env::EnvPath::new("/Users/dev/project"),
        },
    ] {
        let json = serde_json::to_value(&outcome).unwrap();
        assert!(json["outcome"].is_string());
        assert_eq!(
            serde_json::from_value::<DirectoryOutcome>(json).unwrap(),
            outcome
        );
    }
    let event = crate::session_api::KernelEvent::WorkspaceChanged {
        workspace: crate::env::WorkspaceRef {
            environment: crate::ids::EnvironmentId::new("local"),
            root: crate::env::EnvPath::new("/Users/dev/other"),
        },
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "workspace_changed");
    assert_eq!(
        serde_json::from_value::<crate::session_api::KernelEvent>(json).unwrap(),
        event
    );
}

#[test]
fn plan_files_round_trip() {
    let file = PlanFile {
        path: "/Users/dev/project/.zcode/plans/plan-ses_1.md".to_owned(),
        exists: true,
    };
    let json = serde_json::to_value(&file).unwrap();
    assert_eq!(serde_json::from_value::<PlanFile>(json).unwrap(), file);
    let query = PlanFileQuery {
        workspace: crate::env::WorkspaceRef {
            environment: crate::ids::EnvironmentId::new("local"),
            root: crate::env::EnvPath::new("/Users/dev/project"),
        },
        session: crate::ids::SessionId::new("ses_1"),
    };
    let json = serde_json::to_value(&query).unwrap();
    assert_eq!(
        serde_json::from_value::<PlanFileQuery>(json).unwrap(),
        query
    );
}
