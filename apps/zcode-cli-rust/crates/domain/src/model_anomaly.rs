//! Tool call anomaly reminders of one turn (Node `helpers/model-anomaly.ts`
//! and `turn-tool-warnings.ts`). Spec rust-m7-stream-recovery §5.
use serde_json::Value;

/// Node `ModelAnomalyGuardConfig` with its defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guard {
    /// `toolCallWarningThreshold`; off when absent or 0.
    pub tool_call_threshold: u64,
    pub repeated_threshold: u64,
    pub max_warnings: u64,
}

impl Default for Guard {
    fn default() -> Self {
        Self {
            tool_call_threshold: 0,
            repeated_threshold: 3,
            max_warnings: 3,
        }
    }
}

impl Guard {
    /// The effective `modelAnomalyGuard` config object.
    pub fn from_config(config: &Value) -> Self {
        let default = Self::default();
        let number = |key: &str, fallback: u64| config[key].as_u64().unwrap_or(fallback);
        Self {
            tool_call_threshold: number("toolCallWarningThreshold", default.tool_call_threshold),
            repeated_threshold: number(
                "repeatedToolCallWarningThreshold",
                default.repeated_threshold,
            ),
            max_warnings: number("maxBudgetWarningsPerTurn", default.max_warnings),
        }
    }
}

/// Node `RepeatedToolCallWarningState` plus the turn's tool call count.
#[derive(Clone, Debug, Default)]
pub struct TurnAnomalies {
    signature: Option<String>,
    streak: u64,
    injected: u64,
    tool_calls: u64,
}

/// Node `buildRepeatedToolCallSignature`: the name and the input with sorted keys.
fn signature(call: &Value) -> String {
    let name = call["function"]["name"].as_str().unwrap_or("");
    let arguments = call["function"]["arguments"].as_str().unwrap_or("");
    // serde_json 的对象按键排序，与 Node stableJson 同样与键顺序无关。
    let input = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| arguments.into());
    format!("{}:{input}", Value::from(name))
}

impl TurnAnomalies {
    /// One committed step's calls; returns the reminder bodies to inject, in
    /// Node's order (budget first, then repeated calls).
    pub fn observe(&mut self, calls: &[Value], guard: &Guard) -> Vec<String> {
        let mut reminders = vec![];
        let previous = self.tool_calls;
        self.tool_calls += calls.len() as u64;
        let threshold = guard.tool_call_threshold;
        if threshold > 0
            && previous < threshold
            && self.tool_calls >= threshold
            && self.inject(guard)
        {
            reminders.push(budget_body(self.tool_calls));
        }
        if guard.repeated_threshold == 0 {
            return reminders;
        }
        for call in calls {
            let signature = signature(call);
            if self.signature.as_ref() == Some(&signature) {
                self.streak += 1;
            } else {
                self.signature = Some(signature);
                self.streak = 1;
            }
            if self.streak == guard.repeated_threshold && self.inject(guard) {
                let name = call["function"]["name"].as_str().unwrap_or("");
                reminders.push(repeated_body(name, self.streak));
            }
        }
        reminders
    }

    fn inject(&mut self, guard: &Guard) -> bool {
        let allowed = self.injected < guard.max_warnings;
        if allowed {
            self.injected += 1;
        }
        allowed
    }
}

/// Node `buildRepeatedToolCallReminderBody`.
pub fn repeated_body(tool: &str, count: u64) -> String {
    [
        format!("You have called {tool} with the same input {count} times in a row."),
        "Do not repeat the exact same tool call again unless the user explicitly asked you to retry it unchanged.".into(),
        "Use the existing result to take a different next step, explain the blocker, or ask the user for guidance.".into(),
    ]
    .join("\n")
}

/// Node `buildToolCallBudgetReminderBody`.
pub fn budget_body(count: u64) -> String {
    [
        format!("This turn has already made {count} tool calls."),
        "Do not keep calling tools reflexively. Use the gathered results to choose a different next step, summarize the blocker, or ask the user for guidance if you are stuck.".into(),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, arguments: &str) -> Value {
        json!({"function": {"name": name, "arguments": arguments}})
    }

    #[test]
    fn a_third_identical_call_warns_once_per_streak_within_the_turn_budget() {
        let guard = Guard::default();
        let mut turn = TurnAnomalies::default();
        let read = call("Read", r#"{"file_path":"a","limit":5}"#);
        let same = call("Read", r#"{"limit":5,"file_path":"a"}"#);
        assert!(turn.observe(std::slice::from_ref(&read), &guard).is_empty());
        assert!(turn.observe(std::slice::from_ref(&same), &guard).is_empty());
        assert_eq!(
            turn.observe(std::slice::from_ref(&read), &guard),
            vec![repeated_body("Read", 3)]
        );
        assert!(
            turn.observe(std::slice::from_ref(&read), &guard).is_empty(),
            "only at the threshold"
        );
        for name in ["Grep", "Bash"] {
            let other = call(name, "{}");
            let reminders = turn.observe(&[other.clone(), other.clone(), other], &guard);
            assert_eq!(reminders.len(), 1, "{name}");
        }
        let edit = call("Edit", "{}");
        assert!(
            turn.observe(&[edit.clone(), edit.clone(), edit], &guard)
                .is_empty(),
            "at most maxBudgetWarningsPerTurn reminders per turn"
        );
        let mut quiet = TurnAnomalies::default();
        let off = Guard {
            repeated_threshold: 0,
            ..guard
        };
        assert!(
            quiet
                .observe(&[read.clone(), read.clone(), read], &off)
                .is_empty()
        );
    }

    #[test]
    fn the_budget_warning_fires_when_the_turn_crosses_its_threshold() {
        let guard = Guard::from_config(&json!({"toolCallWarningThreshold": 3}));
        assert_eq!((guard.repeated_threshold, guard.max_warnings), (3, 3));
        let mut turn = TurnAnomalies::default();
        let calls = [call("A", "{}"), call("B", "{}")];
        assert!(turn.observe(&calls, &guard).is_empty());
        assert_eq!(turn.observe(&calls, &guard), vec![budget_body(4)]);
        assert!(turn.observe(&calls, &guard).is_empty());
        assert!(budget_body(4).starts_with("This turn has already made 4 tool calls."));
    }
}
