//! The session goal: Node's durable `session_target` facts and the V4
//! `GoalState` projection over them (spec rust-m11-node-storage §5.4).
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::OnceLock;

pub use super::goal_verdict::Verdict;

fn active() -> String {
    "active".into()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    /// Node `target_<ms base36>_<uuid>`.
    pub target_id: String,
    pub objective: String,
    #[serde(default)]
    pub summary_title: Option<String>,
    /// V4 `GoalState.status`: active, verifying, notSatisfied, verified,
    /// paused or failed.
    pub status: String,
    /// Node `session_target.status`: active, paused, budget_limited or complete.
    #[serde(default = "active")]
    pub target_status: String,
    pub iteration: u64,
    pub verifications: Vec<Value>,
    pub iterations: Vec<Value>,
    pub tokens_used: u64,
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub time_used_seconds: u64,
    /// The command whose continuation loop owns the open run.
    #[serde(default)]
    pub active_input_id: Option<String>,
    pub active_run_started_at_ms: Option<u64>,
    /// Node `active_run_last_seen_at` (the run's heartbeat).
    pub last_seen: Option<u64>,
    /// Tokens of the open run, added to `tokens_used` when it finishes.
    #[serde(default)]
    pub run_tokens: u64,
    /// The running completion verification.
    #[serde(default)]
    pub verifying: Option<Verifying>,
    /// The command whose continuation loop runs the goal (Node `inputId`).
    #[serde(default)]
    pub loop_input: Option<String>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

/// A running completion verification (Node `verificationId`, its start and
/// the assistant message and turn it is anchored to).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verifying {
    pub id: String,
    pub started: u64,
    pub anchor: Option<String>,
    pub turn: Option<String>,
}

/// Node `mapGoalStatus`.
pub fn map_status(target_status: &str) -> &'static str {
    match target_status {
        "active" => "active",
        "complete" => "verified",
        _ => "paused",
    }
}

impl Goal {
    /// Node `setSessionTarget`: a new active goal without a run.
    pub fn new(target_id: String, objective: String, now: u64) -> Self {
        Self {
            target_id,
            objective,
            summary_title: None,
            status: "active".into(),
            target_status: "active".into(),
            iteration: 0,
            verifications: vec![],
            iterations: vec![],
            tokens_used: 0,
            token_budget: None,
            time_used_seconds: 0,
            active_input_id: None,
            active_run_started_at_ms: None,
            last_seen: None,
            run_tokens: 0,
            verifying: None,
            loop_input: None,
            created_at: now,
            updated_at: now,
        }
    }
    /// The goal is pursued (V4 statuses of an unfinished goal).
    pub fn active(&self) -> bool {
        matches!(
            self.status.as_str(),
            "active" | "verifying" | "notSatisfied"
        )
    }
    pub fn running(&self) -> bool {
        self.active_run_started_at_ms.is_some()
    }
    fn touch(&mut self, at: u64) {
        self.updated_at = self.updated_at.max(at);
    }
    /// Node `updateSessionTargetStatus`; the V4 status follows
    /// (`TargetChanged` → `mapGoalStatus`).
    pub fn set_status(&mut self, status: &str, now: u64) {
        self.target_status = status.into();
        self.status = map_status(status).into();
        self.updated_at = now;
    }
    /// Node `startSessionTargetRun` (only an active goal runs).
    pub fn start_run(&mut self, input: &str, now: u64) {
        if self.target_status != "active" {
            return;
        }
        self.active_input_id = Some(input.into());
        self.active_run_started_at_ms = Some(now);
        self.last_seen = Some(now);
        self.run_tokens = 0;
        self.status = map_status(&self.target_status).into();
        self.touch(now);
    }
    /// Model usage of the open run (Node `turnUsage.totalTokens`) and its heartbeat.
    pub fn account(&mut self, usage: &Value, now: u64) {
        if !self.running() {
            return;
        }
        let total = usage["total_tokens"].as_u64().unwrap_or_else(|| {
            usage["prompt_tokens"].as_u64().unwrap_or(0)
                + usage["completion_tokens"].as_u64().unwrap_or(0)
        });
        self.account_tokens(total, now);
    }
    /// Tokens of the open run outside its steps (Node's turn usage also sums
    /// in-turn compaction and tools' internal requests).
    pub fn account_tokens(&mut self, total: u64, now: u64) {
        if !self.running() {
            return;
        }
        self.run_tokens = self.run_tokens.saturating_add(total);
        self.heartbeat(now);
    }
    /// Node `heartbeatSessionTargetRun`.
    pub fn heartbeat(&mut self, now: u64) {
        if self.running() {
            self.last_seen = Some(self.last_seen.unwrap_or(0).max(now));
            self.touch(now);
        }
    }
    /// Node `finishSessionTargetRun`: the run's time (whole seconds, rounded
    /// up) and, when `tokens`, its tokens; `status` replaces the status.
    pub fn finish_run(&mut self, now: u64, tokens: bool, status: Option<&str>) {
        let Some(started) = self.active_run_started_at_ms.take() else {
            return;
        };
        if tokens {
            self.tokens_used = self.tokens_used.saturating_add(self.run_tokens);
        }
        self.time_used_seconds += now.saturating_sub(started).div_ceil(1000);
        let status = status
            .map(str::to_owned)
            .or_else(|| self.exhausted().then(|| "budget_limited".to_owned()));
        if let Some(status) = status {
            self.target_status = status;
        }
        self.status = map_status(&self.target_status).into();
        self.active_input_id = None;
        self.last_seen = None;
        self.run_tokens = 0;
        self.touch(now);
    }
    /// Node `recoverInterruptedSessionTargetRun`: an open run of an ended
    /// process counts up to its last heartbeat; an active goal pauses.
    pub fn recover(&mut self, now: u64) {
        let Some(started) = self.active_run_started_at_ms.take() else {
            return;
        };
        let seen = self.last_seen.unwrap_or(started);
        self.time_used_seconds += seen.saturating_sub(started).div_ceil(1000);
        if self.target_status == "active" {
            self.target_status = "paused".into();
        }
        self.status = map_status(&self.target_status).into();
        self.active_input_id = None;
        self.last_seen = None;
        self.run_tokens = 0;
        self.touch(now);
    }
    pub fn exhausted(&self) -> bool {
        self.token_budget
            .is_some_and(|budget| self.tokens_used >= budget)
    }
    /// Node `SessionGoal` (`decodeTargetRow`) of session `session`.
    pub fn node_target(&self, session: &str) -> Value {
        json!({"sessionID": session, "targetID": self.target_id, "objective": self.objective,
            "summaryTitle": self.summary_title, "status": self.target_status,
            "tokenBudget": self.token_budget, "tokensUsed": self.tokens_used,
            "timeUsedSeconds": self.time_used_seconds, "activeInputId": self.active_input_id,
            "activeRunStartedAtMs": self.active_run_started_at_ms,
            "activeRunLastSeenAtMs": self.last_seen,
            "time": {"created": self.created_at, "updated": self.updated_at}})
    }
    pub fn projection(&self) -> Value {
        json!({"targetId":self.target_id,"objective":self.objective,"summaryTitle":self.summary_title,
            "status":self.status,"iteration":self.iteration,"verifications":self.verifications,"iterations":self.iterations,
            "timeUsedSeconds":self.time_used_seconds,"activeRunStartedAtMs":self.active_run_started_at_ms})
    }
    pub fn prompt(&self, kind: &str, verdict: Option<&Verdict>) -> String {
        static TEMPLATES: OnceLock<Value> = OnceLock::new();
        let templates = TEMPLATES.get_or_init(|| {
            serde_json::from_str(include_str!("prompt_templates.json")).expect("goal templates")
        });
        let budget = self
            .token_budget
            .map(|n| n.to_string())
            .unwrap_or_else(|| "none".into());
        let remaining = self
            .token_budget
            .map(|n| n.saturating_sub(self.tokens_used).to_string())
            .unwrap_or_else(|| "unbounded".into());
        let seconds = self.time_used_seconds;
        let mut prompt = templates[kind]
            .as_str()
            .unwrap()
            .replace("Status: active", &format!("Status: {}", self.target_status))
            .replace(
                "Tokens used: 0",
                &format!("Tokens used: {}", self.tokens_used),
            )
            .replace("Token budget: none", &format!("Token budget: {budget}"))
            .replace(
                "Tokens remaining: unbounded",
                &format!("Tokens remaining: {remaining}"),
            )
            .replace("Time used: 0", &format!("Time used: {seconds}"))
            .replace(
                "Time spent pursuing goal: 0",
                &format!("Time spent pursuing goal: {seconds}"),
            )
            .replace(
                "Status before verification: active",
                &format!("Status before verification: {}", self.target_status),
            )
            .replace("{objective}", &escape(&self.objective));
        // Node formatGoalContinuationPrompt：带 nextAction 时首行写入下一步，并附验证结论。
        if let Some(next) = verdict.and_then(|v| v.next_action.as_deref()) {
            let first = "Continue working toward the active session goal.";
            let head = format!(
                "{first} {}\n\nCompletion verifier result:\nReason: {}\nNext action: {}",
                escape(next),
                escape(&verdict.unwrap().reason),
                escape(next)
            );
            prompt = prompt.replacen(first, &head, 1);
        }
        prompt
    }
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
