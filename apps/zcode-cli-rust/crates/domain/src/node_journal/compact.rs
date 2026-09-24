//! Compaction journal hooks at Node's persistence points (`compact-active.ts`):
//! the started timeline, the summary with its completed timeline (committed
//! by the store, which selects the preserved tail in the stored transcript),
//! and the timeline of a compaction that ended without a summary.
use super::Op;
use super::timeline::{Compaction, Host, defaults};
use crate::session::Session;
use serde_json::{Value, json};

/// A compaction's start facts; ids are Node's (`cmp_<uuid>`, message, part).
pub struct CompactStart<'a> {
    pub ids: (String, String, String),
    pub trigger: &'a str,
    pub source_command: Option<&'a str>,
    pub pre_tokens: u64,
    pub custom_instructions: bool,
}

impl Session {
    /// The session facts a timeline host assistant records.
    pub fn node_host(&self) -> Host {
        let (provider, model) = match self.node_selection() {
            Some(_) => (self.provider.clone(), self.model.clone()),
            None => Default::default(),
        };
        Host {
            session: self.id.clone(),
            agent: self.node_agent(),
            provider,
            model,
            mode: self.mode.as_str().into(),
            plan: self.plan_enabled,
            cwd: self.node_directory().into(),
        }
    }

    fn push_records(&mut self, now: u64, records: Vec<Value>) {
        for (index, record) in records.into_iter().enumerate() {
            let op = if index == 0 {
                Op::Message(record)
            } else {
                Op::Part(record)
            };
            self.node.push(now, op);
        }
    }

    /// Node `persistCompactTimelineEvent(CompactStarted)`.
    pub fn node_compact_started(&mut self, now: u64, c: CompactStart) {
        if !self.node.created {
            return;
        }
        let (phase, reason) = defaults(c.trigger);
        let (operation, message, part) = c.ids;
        let parent = self.node.latest.clone().unwrap_or_else(|| message.clone());
        let max_attempts = if c.trigger == "auto" {
            crate::compact::AUTO_ATTEMPTS
        } else {
            1
        };
        let compaction = Compaction {
            operation,
            message,
            part,
            trigger: c.trigger.into(),
            phase: phase.into(),
            reason: reason.into(),
            source_command: c.source_command.map(str::to_owned),
            started: now,
            pre_tokens: Some(c.pre_tokens),
            parent,
            max_attempts,
            custom_instructions: c.custom_instructions,
        };
        let update = if max_attempts > 1 {
            json!({"attempt": 1, "maxAttempts": max_attempts})
        } else {
            json!({})
        };
        let payload = compaction.payload("started", &update);
        let records = compaction.records(&self.node_host(), &payload);
        self.push_records(now, records);
        self.node.compaction = Some(compaction);
    }

    /// A compaction ended without a summary: `skipped` (nothing to compact),
    /// `failed` or `interrupted` (Node `finishCompactTimelineFailure`).
    pub fn node_compact_ended(&mut self, now: u64, status: &str, reason: Option<&str>) {
        let Some(c) = self.node.compaction.take() else {
            return;
        };
        let mut update = json!({"endedAt": now, "replace": true});
        if status == "failed" && c.max_attempts > 1 {
            update["attempt"] = c.max_attempts.into();
            update["maxAttempts"] = c.max_attempts.into();
        }
        if let Some(reason) = reason {
            update["reason"] = reason.into();
        }
        let payload = c.payload(status, &update);
        let records = c.records(&self.node_host(), &payload);
        self.push_records(now, records);
    }

    /// Node compaction success; `done` carries the summary ids, texts, token
    /// counts, preserved group count and post-compact reminders.
    pub fn node_compact_done(&mut self, now: u64, mut done: Value) {
        let Some(c) = self.node.compaction.take() else {
            return;
        };
        done["customInstructions"] = c.custom_instructions.into();
        self.node.latest = done["summaryMessageId"].as_str().map(str::to_owned);
        done["compaction"] = serde_json::to_value(&c).unwrap_or(Value::Null);
        done["host"] = serde_json::to_value(self.node_host()).unwrap_or(Value::Null);
        self.node.push(now, Op::CompactSummary(done));
    }
}
