//! The completion verifier's result (Node `parseGoalCompletionVerificationText`,
//! `failOpenGoalCompletionVerification`, `failedGoalCompletionVerification`
//! and the `TargetCompletionVerification` lifecycle status).
use serde_json::Value;

/// Node `GOAL_COMPLETION_VERIFICATION_FALLBACK_REASON`.
const FALLBACK_REASON: &str =
    "The completion verifier could not confirm that every goal requirement is complete.";

#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    /// The lifecycle status: `completed`, `failed_closed` or `cancelled`.
    pub status: &'static str,
    pub passed: bool,
    pub reason: String,
    pub next_action: Option<String>,
}

fn trimmed(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Node `extractFencedJsonCandidate`.
fn fenced(text: &str) -> Option<String> {
    let text = text.trim();
    let rest = text.strip_prefix("```")?.strip_suffix("```")?;
    let (head, body) = rest.split_once('\n')?;
    let head = head.trim_end_matches('\r').trim_matches([' ', '\t']);
    if !head.is_empty() && !head.eq_ignore_ascii_case("json") {
        return None;
    }
    let body = body.strip_suffix('\n').unwrap_or(body);
    let body = body.strip_suffix('\r').unwrap_or(body);
    Some(body.trim().to_owned())
}

/// Node `parseJsonObjectCandidate`: the outermost braces as a JSON object.
fn object(text: &str) -> Option<serde_json::Map<String, Value>> {
    let text = text.trim();
    let (start, end) = (text.find('{')?, text.rfind('}')?);
    if end < start {
        return None;
    }
    match serde_json::from_str(&text[start..=end]) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// Node `collectJsonObjectCandidates` + `parseJsonObject`.
fn parse_object(text: &str) -> Option<serde_json::Map<String, Value>> {
    let text = text.trim();
    let mut candidates = vec![text.to_owned()];
    let string = match serde_json::from_str::<Value>(text) {
        Ok(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => None,
    };
    if let Some(string) = &string {
        candidates.push(string.clone());
    }
    candidates.extend(fenced(text));
    if let Some(string) = &string {
        candidates.extend(fenced(string));
    }
    candidates.iter().find_map(|c| object(c))
}

impl Verdict {
    fn completed(passed: bool, reason: String, next_action: Option<String>) -> Self {
        Self {
            status: "completed",
            passed,
            reason,
            next_action,
        }
    }

    /// Node: the verifier's text; unparsable output fails open (passes).
    pub fn parse(text: &str) -> Self {
        let Some(object) = parse_object(text) else {
            return Self::completed(
                true,
                "The completion verifier did not return valid JSON.".into(),
                None,
            );
        };
        let field = |key: &str| object.get(key).and_then(trimmed);
        let reason = field("reason").unwrap_or_else(|| FALLBACK_REASON.into());
        let passed = object.get("passed") == Some(&Value::Bool(true));
        Self::completed(passed, reason, field("nextAction"))
    }

    /// Node: a verifier that calls tools fails open.
    pub fn tool_calls() -> Self {
        Self::completed(
            true,
            "The completion verifier attempted to call tools instead of returning a verification result.".into(),
            None,
        )
    }

    /// Node `failed_closed`: the request failed; the goal still passes.
    pub fn request_failed(message: &str) -> Self {
        Self {
            status: "failed_closed",
            passed: true,
            reason: format!("Completion verifier request failed: {message}"),
            next_action: None,
        }
    }

    pub fn cancelled() -> Self {
        Self {
            status: "cancelled",
            passed: false,
            reason: "Completion verifier request was cancelled.".into(),
            next_action: None,
        }
    }

    /// Node `GoalCompletionVerificationOutput` (`{nextAction?, passed, reason}`).
    pub fn verification(&self) -> Value {
        let mut out = serde_json::Map::new();
        if let Some(next) = &self.next_action {
            out.insert("nextAction".into(), next.clone().into());
        }
        out.insert("passed".into(), self.passed.into());
        out.insert("reason".into(), self.reason.clone().into());
        Value::Object(out)
    }

    /// The V4 marker outcome (Node `onTargetVerification`).
    pub fn outcome(&self) -> &'static str {
        match (self.status, self.passed) {
            ("completed", true) => "pass",
            ("completed", false) => "notSatisfied",
            _ => "failed",
        }
    }

    /// The V4 goal status the verification leaves.
    pub fn goal_status(&self) -> &'static str {
        match (self.status, self.outcome()) {
            ("cancelled", _) => "paused",
            ("failed_closed", _) => "failed",
            (_, "pass") => "verified",
            _ => "notSatisfied",
        }
    }

    /// Another continuation turn follows (Node: not passed with a next action).
    pub fn continues(&self) -> bool {
        self.status == "completed" && !self.passed && self.next_action.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn verdicts_follow_nodes_fail_open_rules() {
        let pass = Verdict::parse(r#"{"passed": true, "reason": " done ", "nextAction": ""}"#);
        assert_eq!(
            (pass.passed, pass.reason.as_str(), pass.next_action),
            (true, "done", None)
        );
        let fail = Verdict::parse("```json\n{\"passed\": false, \"nextAction\": \"fix\"}\n```");
        assert_eq!(
            (fail.passed, fail.reason.as_str()),
            (false, FALLBACK_REASON)
        );
        assert!(fail.continues());
        assert_eq!(fail.outcome(), "notSatisfied");
        let quoted = Verdict::parse(r#""{\"passed\": \"true\"}""#);
        assert!(!quoted.passed, "only the boolean true passes");
        let garbage = Verdict::parse("no json");
        assert!(garbage.passed && garbage.status == "completed");
        let failed = Verdict::request_failed("boom");
        assert_eq!(
            (failed.outcome(), failed.goal_status()),
            ("failed", "failed")
        );
        assert_eq!(
            failed.verification(),
            json!({"passed": true, "reason": "Completion verifier request failed: boom"})
        );
        assert_eq!(Verdict::cancelled().goal_status(), "paused");
        assert_eq!(
            Verdict::parse(r#"{"passed":false,"reason":"r","nextAction":"n"}"#).verification(),
            json!({"nextAction": "n", "passed": false, "reason": "r"})
        );
    }
}
