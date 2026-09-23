use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::OnceLock;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    pub target_id: String,
    pub objective: String,
    pub status: String,
    pub iteration: u64,
    pub verifications: Vec<Value>,
    pub iterations: Vec<Value>,
    pub tokens_used: u64,
    pub token_budget: Option<u64>,
    pub time_used_ms: u64,
    pub active_run_started_at_ms: Option<u64>,
    pub last_seen: Option<u64>,
}
#[derive(Clone)]
pub struct Verdict {
    pub outcome: &'static str,
    pub reason: String,
    pub next_action: Option<String>,
}
impl Verdict {
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            outcome: "failed",
            reason: reason.into(),
            next_action: None,
        }
    }
    pub fn parse(text: &str) -> Self {
        let parsed = serde_json::from_str::<Value>(text)
            .ok()
            .or_else(|| serde_json::from_str(text.get(text.find('{')?..=text.rfind('}')?)?).ok());
        let Some(object) = parsed.filter(Value::is_object) else {
            return Self::failed("The completion verifier did not return valid JSON.");
        };
        let Some(passed) = object["passed"].as_bool() else {
            return Self::failed("The completion verifier did not return a boolean verdict.");
        };
        let reason = object["reason"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("The completion verifier could not confirm every goal requirement.");
        Self {
            outcome: if passed { "pass" } else { "notSatisfied" },
            reason: reason.chars().take(8000).collect(),
            next_action: object["nextAction"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(8000).collect()),
        }
    }
}
impl Goal {
    pub fn new(target_id: String, objective: String, now: u64) -> Self {
        Self {
            target_id,
            objective,
            status: "active".into(),
            iteration: 0,
            verifications: vec![],
            iterations: vec![],
            tokens_used: 0,
            token_budget: None,
            time_used_ms: 0,
            active_run_started_at_ms: Some(now),
            last_seen: Some(now),
        }
    }
    pub fn active(&self) -> bool {
        matches!(
            self.status.as_str(),
            "active" | "verifying" | "notSatisfied"
        )
    }
    pub fn start(&mut self, now: u64) {
        self.status = "active".into();
        self.active_run_started_at_ms = Some(now);
        self.last_seen = Some(now);
    }
    pub fn account(&mut self, usage: &Value, now: u64) {
        self.tokens_used = self
            .tokens_used
            .saturating_add(usage["prompt_tokens"].as_u64().unwrap_or(0))
            .saturating_add(usage["completion_tokens"].as_u64().unwrap_or(0));
        if let Some(last) = self.last_seen {
            self.time_used_ms = self.time_used_ms.saturating_add(now.saturating_sub(last));
        }
        if self.last_seen.is_some() {
            self.last_seen = Some(now);
        }
    }
    pub fn settle(&mut self, now: u64) {
        self.account(&Value::Null, now);
        self.last_seen = None;
        self.active_run_started_at_ms = None;
    }
    pub fn pause(&mut self, now: u64) {
        self.settle(now);
        if self.active() {
            self.status = "paused".into();
        }
    }
    pub fn exhausted(&self) -> bool {
        self.token_budget
            .is_some_and(|budget| self.tokens_used >= budget)
    }
    pub fn projection(&self) -> Value {
        json!({"targetId":self.target_id,"objective":self.objective,"summaryTitle":null,
            "status":self.status,"iteration":self.iteration,"verifications":self.verifications,"iterations":self.iterations,
            "timeUsedSeconds":self.time_used_ms / 1000,"activeRunStartedAtMs":self.active_run_started_at_ms})
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
        let mut prompt = templates[kind]
            .as_str()
            .unwrap()
            .replace(
                "Tokens used: 0",
                &format!("Tokens used: {}", self.tokens_used),
            )
            .replace("Token budget: none", &format!("Token budget: {budget}"))
            .replace(
                "Tokens remaining: unbounded",
                &format!("Tokens remaining: {remaining}"),
            )
            .replace(
                "Time used: 0",
                &format!("Time used: {}", self.time_used_ms / 1000),
            )
            .replace(
                "Time spent pursuing goal: 0",
                &format!("Time spent pursuing goal: {}", self.time_used_ms / 1000),
            )
            .replace("{objective}", &escape(&self.objective));
        if let Some(verdict) = verdict {
            prompt = format!(
                "Completion verifier result:\nReason: {}\nNext action: {}\n\n{prompt}",
                escape(&verdict.reason),
                escape(verdict.next_action.as_deref().unwrap_or(""))
            );
        }
        prompt
    }
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
