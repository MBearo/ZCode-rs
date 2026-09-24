//! The session goal as legacy `session/goal` and the legacy snapshot show it
//! (Node `formatGoalSummary` / `formatGoalChanged`, `zcodeSessionGoalSchema`).
use crate::goal::Goal;
use serde_json::{Value, json};

/// Node `PLAN_MODE_GOAL_CONTINUATION_SKIPPED_MESSAGE`, appended after a blank line.
pub const PLAN_NOTE: &str = "Plan mode 下已记录 goal，但不会自动继续。";

/// Node target status: `active | paused | budget_limited | complete`.
pub fn status(goal: &Goal) -> &'static str {
    match goal.status.as_str() {
        "verified" => "complete",
        "paused" if goal.exhausted() => "budget_limited",
        "paused" => "paused",
        _ => "active",
    }
}

/// `formatGoalChanged(title, target)`.
pub fn changed(title: &str, goal: &Goal) -> String {
    let budget = goal
        .token_budget
        .map_or_else(|| "none".to_owned(), |b| b.to_string());
    format!(
        "{title}\nObjective: {}\nUsage: {} tokens / {budget}\nTime: {} seconds",
        goal.objective,
        goal.tokens_used,
        goal.time_used_ms / 1000
    )
}

/// `formatGoalSummary(target)`.
pub fn summary(goal: Option<&Goal>) -> String {
    match goal {
        Some(goal) => changed(&format!("Goal {}", status(goal)), goal),
        None => "No goal is set. Use /goal <objective> to set one.".into(),
    }
}

/// Strict `zcodeSessionGoalSchema`; `created` stands in for goals persisted
/// without timestamps.
pub fn target(goal: &Goal, session_id: &str, created: u64) -> Value {
    let created_at = if goal.created_at == 0 {
        created
    } else {
        goal.created_at
    };
    json!({"sessionId": session_id, "targetId": goal.target_id, "objective": goal.objective,
        "summaryTitle": null, "status": status(goal),
        // schema 要求正整数或 null：0 预算按未设置处理。
        "tokenBudget": goal.token_budget.filter(|b| *b > 0),
        "tokensUsed": goal.tokens_used, "timeUsedSeconds": goal.time_used_ms / 1000,
        "activeRunStartedAtMs": goal.active_run_started_at_ms,
        "activeRunLastSeenAtMs": goal.last_seen,
        "createdAt": created_at, "updatedAt": goal.updated_at.max(created_at)})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_goals_like_node() {
        let mut goal = Goal::new("t".into(), "ship it".into(), 5);
        assert_eq!(
            changed("Goal active", &goal),
            "Goal active\nObjective: ship it\nUsage: 0 tokens / none\nTime: 0 seconds"
        );
        goal.token_budget = Some(10);
        goal.tokens_used = 12;
        goal.time_used_ms = 61_999;
        goal.status = "paused".into();
        assert_eq!(
            summary(Some(&goal)),
            "Goal budget_limited\nObjective: ship it\nUsage: 12 tokens / 10\nTime: 61 seconds"
        );
        goal.status = "verified".into();
        assert_eq!(status(&goal), "complete");
        goal.status = "notSatisfied".into();
        assert_eq!(status(&goal), "active");
        assert_eq!(
            summary(None),
            "No goal is set. Use /goal <objective> to set one."
        );
        goal.created_at = 0;
        goal.token_budget = Some(0);
        let target = target(&goal, "s", 3);
        assert_eq!(target["createdAt"], 3);
        assert_eq!(target["tokenBudget"], Value::Null);
        assert_eq!(target["summaryTitle"], Value::Null);
    }
}
