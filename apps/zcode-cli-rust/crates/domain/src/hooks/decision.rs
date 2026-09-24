//! What merged hook results decide (Node `hook-flow.ts` and
//! `runtime/methods/hooks.ts`).
use super::output::{RunResult, text};
use crate::permission::Behavior;
use serde_json::Value;

/// Node `runPermissionRequestHooks`: the hooks' answer to a permission prompt,
/// or `None` when they gave none (the user still decides).
#[derive(Clone, Debug, PartialEq)]
pub enum RequestAnswer {
    Deny(String),
    /// `permissionUpdates ?? updatedPermissions`.
    Allow(Option<Value>),
    Modify {
        input: Value,
        updates: Option<Value>,
    },
}

pub const ALLOWED_BY_HOOK: &str = "Allowed by PermissionRequest hook";
pub const MODIFIED_BY_HOOK: &str = "Allowed with modified input by PermissionRequest hook";
const DENIED_BY_HOOK: &str = "Denied by PermissionRequest hook";

pub fn request_answer(r: &RunResult) -> Option<RequestAnswer> {
    let denied = || {
        RequestAnswer::Deny(
            r.stop_reason
                .clone()
                .unwrap_or_else(|| DENIED_BY_HOOK.into()),
        )
    };
    if r.prevent_continuation {
        return Some(denied());
    }
    let Some(decision) = &r.request_decision else {
        return match r.behavior {
            Some(Behavior::Deny) => Some(denied()),
            Some(Behavior::Allow) => Some(RequestAnswer::Allow(None)),
            _ => None,
        };
    };
    if decision["behavior"] == "deny" {
        let message = text(decision, "message").unwrap_or_else(|| DENIED_BY_HOOK.into());
        return Some(RequestAnswer::Deny(message));
    }
    let updates = decision
        .get("permissionUpdates")
        .or_else(|| decision.get("updatedPermissions"))
        .cloned();
    Some(match decision.get("updatedInput") {
        Some(input) => RequestAnswer::Modify {
            input: input.clone(),
            updates,
        },
        None => RequestAnswer::Allow(updates),
    })
}

/// Node `applyPreToolPermissionDecision`: a hook `allow` answers an `ask`
/// (never an `alwaysAsk`), a hook `ask` escalates an `allow`; `deny` stays.
pub fn apply_pre_tool(decision: &mut crate::permission::Decision, r: &RunResult) {
    let reason = r.decision_reason.clone();
    match (decision.behavior, r.behavior) {
        (Behavior::Ask, Some(Behavior::Allow)) if !decision.always_ask => {
            decision.behavior = Behavior::Allow;
            decision.reason =
                reason.unwrap_or_else(|| "Tool was allowed by PreToolUse hook".into());
            decision.rule_id = "hook.PreToolUse.allow";
        }
        (Behavior::Allow, Some(Behavior::Ask)) => {
            decision.behavior = Behavior::Ask;
            decision.reason =
                reason.unwrap_or_else(|| "Tool requires approval by PreToolUse hook".into());
            decision.rule_id = "hook.PreToolUse.ask";
        }
        _ => {}
    }
}

/// Node `shouldContinueAfterStopHooks`: at most three continuations a turn.
pub fn should_continue(r: &RunResult, continuations: usize) -> bool {
    r.stop_should_continue && !r.additional_contexts.is_empty() && continuations < 3
}
