//! `/goal` 与 `/fork` 跨端口类型的序列化（docs/specs/rust-cli/goal.md §9、session-lifecycle.md §7）。
use serde_json::json;

use super::*;
use crate::env::{EnvPath, WorkspaceRef};
use crate::env_rewind::{RestoreAction, RestoredFile};
use crate::hooks::{HookContext, HookEvent, HookEventName, StopCondition, StopVerdict};
use crate::ids::{ArtifactId, EnvironmentId, MessageId, SessionId, ToolUseId};
use crate::message::{ContentBlock, Message, UserMessage, UserOrigin};
use crate::model::ModelSelection;
use crate::permission::PermissionMode;
use crate::session_api::{Command, ForkFiles};
use crate::store::{ArtifactPurpose, ArtifactRef, ForkReceipt, SessionFork, WorkspaceCheckpoint};

fn round_trip<T>(value: &T) -> serde_json::Value
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let text = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(text.clone()).unwrap();
    assert_eq!(&back, value);
    text
}

#[test]
fn goal_status_uses_the_fixed_keys() {
    let set = GoalStatus {
        met: false,
        sentinel: true,
        failed: false,
        condition: "tests pass".to_owned(),
        reason: None,
        iterations: None,
        duration_ms: None,
        tokens: None,
    };
    assert_eq!(
        round_trip(&set),
        json!({"met": false, "sentinel": true, "condition": "tests pass"})
    );
    let met = GoalStatus {
        met: true,
        sentinel: false,
        failed: false,
        condition: "tests pass".to_owned(),
        reason: Some("all green".to_owned()),
        iterations: Some(2),
        duration_ms: Some(1500),
        tokens: Some(40),
    };
    assert_eq!(
        round_trip(&met),
        json!({"met": true, "condition": "tests pass", "reason": "all green",
            "iterations": 2, "durationMs": 1500, "tokens": 40})
    );
    let view = GoalView {
        active: Some(ActiveGoal {
            condition: "x".to_owned(),
            iterations: 1,
            set_at_ms: 10,
            tokens: 3,
            last_reason: Some("not yet".to_owned()),
        }),
        achieved: Some(met),
    };
    round_trip(&view);
}

#[test]
fn stop_conditions_and_verdicts_round_trip() {
    let event = HookEvent {
        name: HookEventName::Stop,
        session: SessionId::new("sess_1"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/project"),
        },
        payload: json!({"stop_hook_active": false}),
        context: HookContext {
            mode: Some(PermissionMode::Default),
            ..HookContext::default()
        },
    };
    round_trip(&StopCondition {
        event,
        condition: "done".to_owned(),
        transcript: Vec::new(),
    });
    let feedback = Message::User(UserMessage {
        id: MessageId::new("msg_2"),
        content: vec![ContentBlock::Text {
            text: "Stop hook feedback:\n[done]: no".to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: "stop_hook_feedback".to_owned(),
        },
    });
    for verdict in [
        StopVerdict::Met { reason: None },
        StopVerdict::NotMet {
            reason: Some("no".to_owned()),
            feedback,
        },
        StopVerdict::Impossible {
            reason: Some("never".to_owned()),
        },
        StopVerdict::Failed {
            message: "JSON validation failed".to_owned(),
        },
        StopVerdict::TimedOut,
        StopVerdict::Cancelled,
    ] {
        round_trip(&verdict);
    }
}

#[test]
fn forks_round_trip() {
    let command = Command::ForkCheckpoint {
        checkpoint: None,
        files: ForkFiles::Restore,
    };
    assert_eq!(
        round_trip(&command),
        json!({"type": "fork_checkpoint", "files": "restore"})
    );
    let checkpoint = WorkspaceCheckpoint {
        id: "checkpoint_1".to_owned(),
        prompt: MessageId::new("msg_1"),
        assistant: MessageId::new("msg_2"),
        tool_use: ToolUseId::new("toolu_1"),
        snapshot: ArtifactRef {
            session: SessionId::new("sess_1"),
            id: ArtifactId::new("tool-result-1"),
            purpose: ArtifactPurpose::Checkpoint,
            media_type: "application/json".to_owned(),
            size: 10,
        },
    };
    round_trip(&SessionFork {
        parent: SessionId::new("sess_1"),
        child: SessionId::new("sess_2"),
        created_at_ms: 5,
        through: MessageId::new("msg_1"),
        checkpoint: Some(checkpoint),
        restored: vec![RestoredFile {
            path: EnvPath::new("/Users/dev/project/a.txt"),
            action: RestoreAction::Restore,
        }],
        selection: ModelSelection {
            provider: "anthropic".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            reasoning: None,
            connection: None,
        },
        mode: PermissionMode::Default,
        plan: true,
    });
    round_trip(&ForkReceipt { copied: 3 });
}
