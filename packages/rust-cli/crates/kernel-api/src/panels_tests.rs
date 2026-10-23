//! 面板查询 DTO 的序列化往返（spec §6 规则 8）。
use super::*;
use crate::ids::SessionId;

#[test]
fn subagent_entries_round_trip() {
    let entry = SubagentEntry {
        agent: AgentId::new("agent_1"),
        child: SessionId::new("sess_subagent_agent_1"),
        agent_type: "Explore".to_owned(),
        description: "Find callers".to_owned(),
        status: SubagentStatus::Running,
        background: true,
        waiting: false,
        elapsed_ms: 1_500,
        ended_ago_ms: None,
        version: 3,
        owner: None,
        activity: None,
        parked: false,
    };
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["status"], "running");
    assert!(json.get("ended_ago_ms").is_none());
    assert!(json.get("activity").is_none());
    assert!(json.get("parked").is_none());
    assert_eq!(
        serde_json::from_value::<SubagentEntry>(json).unwrap(),
        entry
    );
    // 修复线 F30：停放的条目写出 `parked`；旧的快照没有它，读成 false（下面的 activity 一项同）。
    let parked = SubagentEntry {
        status: SubagentStatus::Completed,
        parked: true,
        ..entry
    };
    let json = serde_json::to_value(&parked).unwrap();
    assert_eq!(json["parked"], true);
    assert_eq!(
        serde_json::from_value::<SubagentEntry>(json).unwrap(),
        parked
    );
}

#[test]
fn subagent_activities_round_trip() {
    for activity in [
        SubagentActivity::Thinking,
        SubagentActivity::Tool {
            name: "Bash".to_owned(),
        },
        SubagentActivity::Retrying {
            attempt: 2,
            max_retries: 10,
            retry_in_ms: 1_000,
            error: "server error 500: boom".to_owned(),
        },
        SubagentActivity::Stalled {
            retry_in_ms: 280_000,
        },
    ] {
        let json = serde_json::to_value(&activity).unwrap();
        assert_eq!(
            serde_json::from_value::<SubagentActivity>(json).unwrap(),
            activity
        );
    }
    // 旧的快照没有 activity：读成 None。
    let json = serde_json::json!({
        "agent": "agent_1", "child": "sess_1", "agent_type": "Explore", "description": "d",
        "status": "running", "background": false, "elapsed_ms": 1, "version": 0
    });
    let old = serde_json::from_value::<SubagentEntry>(json).unwrap();
    assert_eq!(old.activity, None);
    assert!(!old.parked);
}

/// 修复线 F34：子代理条目与后台任务的同一份快照；两个字段缺省时读成空。
#[test]
fn tasks_snapshots_round_trip() {
    let snapshot = TasksSnapshot {
        subagents: vec![SubagentEntry {
            agent: AgentId::new("agent_1"),
            child: SessionId::new("sess_subagent_agent_1"),
            agent_type: "general-purpose".to_owned(),
            description: "Alpha task".to_owned(),
            status: SubagentStatus::Running,
            background: true,
            waiting: false,
            elapsed_ms: 10,
            ended_ago_ms: None,
            version: 0,
            owner: None,
            activity: None,
            parked: false,
        }],
        background: vec![crate::manage::BackgroundEntry {
            id: "agent_1".to_owned(),
            kind: crate::manage::BackgroundKind::Agent,
            description: "Alpha task".to_owned(),
            status: BackgroundStatus::Running,
            output: None,
        }],
    };
    let json = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(json["background"][0]["kind"], "agent");
    assert_eq!(
        serde_json::from_value::<TasksSnapshot>(json).unwrap(),
        snapshot
    );
    assert_eq!(
        serde_json::from_value::<TasksSnapshot>(serde_json::json!({})).unwrap(),
        TasksSnapshot::default()
    );
}

#[test]
fn workspace_sessions_and_shell_details_round_trip() {
    let session = WorkspaceSession {
        summary: SessionSummary {
            id: SessionId::new("sess_1"),
            title: "Fix the build".to_owned(),
            updated_at_ms: 10,
        },
        activity: Some(SessionActivity::NeedsInput),
        last_reply: Some("done".to_owned()),
    };
    let json = serde_json::to_value(&session).unwrap();
    assert_eq!(json["activity"], "needs_input");
    assert_eq!(
        serde_json::from_value::<WorkspaceSession>(json).unwrap(),
        session
    );

    let detail = ShellDetail {
        id: "b1".to_owned(),
        command: "npm test".to_owned(),
        status: BackgroundStatus::Running,
        elapsed_ms: Some(65_000),
        output: ShellOutput::Tail {
            lines: vec!["ok".to_owned()],
            total: 1,
        },
    };
    let json = serde_json::to_value(&detail).unwrap();
    assert_eq!(json["output"]["kind"], "tail");
    assert_eq!(serde_json::from_value::<ShellDetail>(json).unwrap(), detail);
    let large = serde_json::to_value(ShellOutput::TooLarge { bytes: 9 }).unwrap();
    assert_eq!(large["kind"], "too_large");
}
