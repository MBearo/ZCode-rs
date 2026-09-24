//! Busy-input ledger updates and guided prompts (Node `steering.ts`: queue
//! edits, removals, delivery fallbacks and the guide drain). Only a session
//! stored in the Node database records them.
use super::{Notice, Op, Prompt, Turn};
use crate::session::Session;
use serde_json::{Value, json};

/// A background task's result delivered to the model (Node task notification).
pub struct Notification<'a> {
    /// The ledger row (`runtime_command_<uuid>`), the notice message and part.
    pub ids: (String, String, String),
    pub text: &'a str,
    pub task: Option<&'a str>,
    pub origin: Option<Value>,
    /// The run's turn uuid when the notification opens a turn.
    pub turn: Option<&'a str>,
    pub tools: &'a [String],
}

impl Session {
    /// Node `updateSessionInputs`: `[{id, text?, queuePosition?, delivery?, intent?}]`.
    pub fn node_update_inputs(&mut self, now: u64, updates: Vec<Value>) {
        if self.node.created && !updates.is_empty() {
            self.node.push(now, Op::UpdateInputs(updates));
        }
    }

    /// Node `settleSessionInput` of an admitted input (a removed queue item is
    /// `cancelled/user_removed`).
    pub fn node_settle_input(&mut self, now: u64, id: &str, status: &str, reason: &str) {
        if self.node.created {
            let (id, status, reason) = (id.into(), status.into(), Some(reason.into()));
            self.node.push(now, Op::SettleInput { id, status, reason });
        }
    }

    /// Node conversation rewind before an edited or retried input; the rerun
    /// opens a new turn.
    pub fn node_rewind(&mut self, now: u64, target: &str, anchor: &str) {
        if self.node.created {
            let (target, anchor) = (target.into(), anchor.into());
            self.node.push(now, Op::Rewind { target, anchor });
            self.node.turn = None;
            self.node.latest = None;
        }
    }

    /// Node guide drain: the guided input's user message joins the running
    /// turn, promoted from its ledger row.
    pub fn node_guided_prompt(&mut self, now: u64, p: Prompt) {
        let Some(runtime) = self.node.turn.as_ref().map(|t| t.runtime.clone()) else {
            return;
        };
        let message = p.message.clone();
        self.push_prompt(now, p, &runtime);
        if let Some(turn) = self.node.turn.as_mut() {
            turn.messages.push(message);
        }
    }

    /// Node task notification: the ledger row (`backgroundNotification`), the
    /// `background_task` notice (`task_notification` presentation) and the row
    /// promoted to it (`enqueueBackgroundTaskNotification`,
    /// `persistBackgroundTaskNotificationBatch`).
    pub fn node_task_notification(&mut self, now: u64, n: Notification) {
        if !self.node.created {
            return;
        }
        let (ledger, message, part) = n.ids;
        let mut payload = json!({"text": n.text});
        if let Some(task) = n.task {
            payload["taskId"] = task.into();
        }
        if let Some(origin) = &n.origin {
            payload["originMeta"] = origin.clone();
        }
        let row = json!({"id": ledger, "sessionID": self.id, "kind": "backgroundNotification",
            "delivery": "queue", "payload": payload});
        self.node.push(now, Op::SaveInput(row));
        if let Some(turn) = n.turn {
            self.node.turn = Some(Turn {
                runtime: crate::node_ids::turn_id(turn),
                messages: vec![message.clone()],
                user: message.clone(),
                ..Turn::default()
            });
        }
        let presentation = if n.turn.is_some() {
            "task_notification"
        } else {
            "task_notification_steer"
        };
        let mut metadata = json!({"inputPresentation": presentation});
        if let Some(origin) = n.origin {
            metadata["originMeta"] = origin;
        }
        metadata["visibility"] = "model-only".into();
        let notice = Notice {
            message: message.clone(),
            part,
            source: "background_task",
            text: n.text,
            metadata: Some(metadata),
            tools: n.tools,
        };
        self.node_notice(now, notice);
        self.node.push(
            now,
            Op::MarkInputPromoted {
                id: ledger,
                message,
            },
        );
    }

    /// Node coordinator message to a running child (`steerTurn` with
    /// `coordinator_steer`): the guide ledger row, promoted to the user message.
    pub fn node_coordinator_steer(
        &mut self,
        now: u64,
        (ledger, message, part): (String, String, String),
        text: &str,
        command: &str,
        tools: &[String],
    ) {
        if !self.node.created || self.node.turn.is_none() {
            return;
        }
        let row = json!({"id": ledger, "sessionID": self.id, "kind": "sendText",
            "delivery": "guide", "payload": {"text": text}});
        self.node.push(now, Op::SaveInput(row));
        let metadata =
            json!({"turnSteerDelivery": "guide", "inputPresentation": "coordinator_steer"});
        self.node_guided_prompt(
            now,
            Prompt {
                message,
                part,
                turn: "",
                text,
                command: Some(command),
                queue_id: Some(&ledger),
                metadata: Some(metadata),
                tools,
                files: vec![],
            },
        );
    }
}
