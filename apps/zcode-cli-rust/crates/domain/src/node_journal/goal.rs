//! Goal journal hooks (spec rust-m11-node-storage §5.4): the `session_target`
//! row kept equal to the goal, the `/goal` control-only prompt, the
//! continuation turn's notice, completion verification records and goal
//! state notices (Node `session-facade.ts`, `target.ts`,
//! `target-completion-verification.ts`, `goal-state-reminder.ts`).
use super::timeline::timeline_records;
use super::{Notice, Op, Prompt, Turn};
use crate::goal::Verdict;
use crate::session::Session;
use serde_json::{Map, json};

/// Node `session-facade.ts` goal state change texts.
pub fn state_text(change: &str) -> &'static str {
    match change {
        "paused" => {
            "The active session goal is paused. Do not continue pursuing it unless the user resumes or replaces the goal."
        }
        "resumed" => "The session goal is active again and will be pursued.",
        _ => {
            "The session goal has been cleared. Do not continue pursuing any previous goal unless the user sets a new goal."
        }
    }
}

impl Session {
    /// The `session_target` row equals the goal: written when it changed,
    /// deleted when the goal was cleared.
    pub fn node_sync_goal(&mut self, now: u64) {
        if !self.node.created {
            return;
        }
        let row = self.goal.as_ref().map(|g| g.node_target(&self.id));
        if row != self.node.target {
            self.node.push(now, Op::Target(row.clone()));
            self.node.target = row;
        }
    }

    /// Node `recordExternalUserPrompt`: the `/goal` user message of a
    /// control-only turn (no `anchor.turnId`, `executionKind: controlOnly`).
    pub fn node_control_prompt(&mut self, now: u64, mut p: Prompt) {
        let mut metadata = p.metadata.take().unwrap_or_else(|| json!({}));
        metadata["executionKind"] = "controlOnly".into();
        p.metadata = Some(metadata);
        self.push_prompt(now, p, "");
    }

    /// Node continuation turn: the turn opens with the `goal-continuation`
    /// notice (model-only) after the pending model change.
    pub fn node_goal_continuation(&mut self, now: u64, turn: &str, n: Notice) {
        if !self.node.created {
            return;
        }
        self.node.turn = Some(Turn {
            runtime: crate::node_ids::turn_id(turn),
            user: n.message.clone(),
            ..Turn::default()
        });
        self.node_flush_model_change(now);
        self.node_notice(now, n);
    }

    /// Node `goal_state_change` notice (`paused`, `resumed` or `cleared`).
    pub fn node_goal_state(
        &mut self,
        now: u64,
        (message, part): (String, String),
        change: &str,
        tools: &[String],
    ) {
        self.node_notice(
            now,
            Notice {
                message,
                part,
                source: "goal_state_change",
                text: state_text(change),
                metadata: None,
                tools,
            },
        );
    }

    /// Node `persistDurableSessionEvent(TargetCompletionVerification)`: the
    /// lifecycle entry and the verification timeline message and part.
    pub fn node_goal_verification(
        &mut self,
        now: u64,
        (event, trace): (String, String),
        verdict: Option<&Verdict>,
    ) {
        if !self.node.created {
            return;
        }
        let Some(goal) = self.goal.as_ref() else {
            return;
        };
        let Some(verifying) = goal.verifying.clone() else {
            return;
        };
        let status = verdict.map_or("started", |v| v.status);
        let mut payload = Map::new();
        if let Some(anchor) = &verifying.anchor {
            payload.insert("anchorAssistantMessageId".into(), anchor.clone().into());
        }
        if let Some(turn) = &verifying.turn {
            payload.insert("anchorTurnId".into(), turn.clone().into());
        }
        payload.insert("goalIteration".into(), goal.iteration.into());
        payload.insert("status".into(), status.into());
        payload.insert("targetId".into(), goal.target_id.clone().into());
        if let Some(verdict) = verdict {
            payload.insert("verification".into(), verdict.verification());
        }
        payload.insert("verificationId".into(), verifying.id.clone().into());
        let data = json!({"eventId": event, "payload": payload, "sequenceNumber": self.revision,
            "traceId": trace});
        let entry = json!({"id": event, "sessionID": self.id, "type": "target_completion_verification",
            "time": {"created": now, "updated": now}, "data": data});
        let key = format!("{}_{}", goal.target_id, goal.iteration);
        let mut draft = json!({"timelineType": "goal_verification", "display": "separator",
            "status": status});
        if let Some(anchor) = &verifying.anchor {
            draft["anchorMessageId"] = anchor.clone().into();
        }
        if let Some(turn) = &verifying.turn {
            draft["anchorTurnId"] = turn.clone().into();
        }
        draft["targetId"] = goal.target_id.clone().into();
        draft["verificationId"] = verifying.id.clone().into();
        draft["goalIteration"] = goal.iteration.into();
        if let Some(verdict) = verdict {
            draft["verification"] = verdict.verification();
        }
        let ended = verdict.is_some().then_some(now);
        draft["time"] = match ended {
            Some(end) => json!({"start": verifying.started, "end": end}),
            None => json!({"start": verifying.started}),
        };
        let parent = verifying
            .anchor
            .clone()
            .or_else(|| self.node.latest.clone())
            .unwrap_or_default();
        let host = self.node_host();
        let (info, part) = timeline_records(
            &host,
            (
                &format!("msg_goal_verify_{key}"),
                &format!("part_goal_verify_{key}_timeline"),
            ),
            &parent,
            (verifying.started, ended),
            draft,
        );
        self.node.push(now, Op::Entry(entry));
        self.node.push(now, Op::Message(info));
        self.node.push(now, Op::Part(part));
    }
}
