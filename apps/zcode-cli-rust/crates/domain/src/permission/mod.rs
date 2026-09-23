//! Tool permission policy (Node `core/src/permission/service.ts`). Pure: the
//! engine owns the inputs (mode, rules, config) and hands the run task an
//! immutable snapshot; `check` never touches IO.
mod capability;
mod options;
mod rules;
#[cfg(test)]
mod tests;

pub use capability::{PermissionSpec, Resolved, ToolCapability, resolve};
pub use options::{
    default_updates, denied_by_user, denied_content, protocol_options, tool_capability, v4_options,
};
pub use rules::{Behavior, Rule, Ruleset, Update, apply_updates, matches_content};

use crate::execution::Mode;
use serde_json::Value;
use std::collections::BTreeSet;

/// Node `PermissionConfig` (config file `permission` section).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    pub allowed_tools: BTreeSet<String>,
    pub disallowed_tools: BTreeSet<String>,
    pub auto_approve_high_risk: bool,
}

impl Config {
    pub fn from_config(permission: &Value) -> Self {
        let names = |key: &str| {
            permission[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        };
        Self {
            allowed_tools: names("allowedTools"),
            disallowed_tools: names("disallowedTools"),
            auto_approve_high_risk: permission["autoApproveHighRisk"] == true,
        }
    }
}

/// One tool call as the policy sees it.
pub struct Context<'a> {
    pub tool: &'a str,
    pub input: &'a Value,
    pub mode: Mode,
    pub plan_enabled: bool,
    pub working_directory: Option<&'a str>,
}

/// Tool-specific rule evaluation (Node `ToolPermissionRulePolicy`, Bash only).
pub trait RulePolicy {
    fn evaluate(&self, behavior: Behavior, rules: &[&Rule]) -> bool;
}

/// Node `PermissionDecisionResult` (fields used downstream).
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub behavior: Behavior,
    pub rule_id: &'static str,
    pub reason: String,
    pub risk_level: String,
    pub side_effect_scope: String,
    pub always_ask: bool,
}

/// Inputs owned by the engine: session rules and config are per session tree,
/// project rules per workspace.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub config: Config,
    pub session_rules: Ruleset,
}

impl Policy {
    /// Node `PermissionService.checkPermission`, step for step.
    pub fn check(
        &self,
        ctx: &Context<'_>,
        cap: &Resolved,
        project: Option<&Ruleset>,
        rule_policy: Option<&dyn RulePolicy>,
    ) -> Decision {
        let tool = ctx.tool;
        let result = |behavior, rule_id, reason: String| Decision {
            behavior,
            rule_id,
            reason,
            risk_level: cap.risk_level.clone(),
            side_effect_scope: cap.side_effect_scope.clone(),
            always_ask: cap.always_ask,
        };
        use Behavior::{Allow, Ask, Deny};
        if tool == "EnterPlanMode" {
            let reason = "EnterPlanMode switches to plan mode without a permission prompt";
            return result(Allow, "tool.plan.enter", reason.into());
        }
        if tool == "ExitPlanMode" && !ctx.plan_enabled {
            let reason = "ExitPlanMode can only be used while plan mode is active";
            return result(Deny, "mode.plan.exitOnly", reason.into());
        }
        let disallowed = || {
            result(
                Deny,
                "rule.disallowedTools",
                format!("Tool {tool} is explicitly disallowed"),
            )
        };
        if cap.requires_user_interaction {
            if self.config.disallowed_tools.contains(tool) {
                return disallowed();
            }
            return result(
                Ask,
                "tool.userInteraction",
                format!("Tool {tool} requires user interaction"),
            );
        }
        let auto = || {
            result(
                Deny,
                "mode.auto.unimplemented",
                "Auto mode is reserved but not implemented yet".into(),
            )
        };
        let matches = |ruleset: Option<&Ruleset>, behavior| {
            rules::matches(ruleset, behavior, ctx, cap, rule_policy)
        };
        let project_deny = || {
            result(
                Deny,
                "rule.project.deny",
                format!("Tool {tool} is denied by project permission rules"),
            )
        };
        if cap.always_ask {
            // 与 Node checkAlwaysAsk 一致：只有硬阻断先于 ask，放行分支（yolo/plan）不能绕过。
            if ctx.mode == Mode::Auto {
                return auto();
            }
            if self.config.disallowed_tools.contains(tool) {
                return disallowed();
            }
            if matches(project, Deny) {
                return project_deny();
            }
            if matches(Some(&self.session_rules), Allow) {
                return result(
                    Allow,
                    "rule.session.allow",
                    format!("Tool {tool} was allowed for this session"),
                );
            }
            if owned_workflow_amend(ctx) {
                return result(
                    Allow,
                    "rule.session.workflowOwner",
                    format!("Tool {tool} amends a run this session started"),
                );
            }
            return result(
                Ask,
                "tool.alwaysAsk",
                format!("Tool {tool} always requires explicit approval"),
            );
        }
        if ctx.mode == Mode::Yolo && !ctx.plan_enabled {
            return result(
                Allow,
                "mode.yolo",
                "Yolo mode bypasses permission prompts".into(),
            );
        }
        if ctx.mode == Mode::Auto {
            return auto();
        }
        if self.config.disallowed_tools.contains(tool) {
            return disallowed();
        }
        if matches(project, Deny) {
            return project_deny();
        }
        if matches(project, Ask) {
            return result(
                Ask,
                "rule.project.ask",
                format!("Tool {tool} requires approval by project permission rules"),
            );
        }
        if ctx.plan_enabled {
            return plan_ladder(cap, result);
        }
        if matches(project, Allow) {
            return result(
                Allow,
                "rule.project.allow",
                format!("Tool {tool} is allowed by project permission rules"),
            );
        }
        if tool == "WebFetch"
            && ctx.input["url"]
                .as_str()
                .is_some_and(options::webfetch_preapproved)
        {
            return result(
                Allow,
                "tool.webfetch.preapproved",
                "WebFetch URL is preapproved".into(),
            );
        }
        if rules::workflow_draft_write(ctx) {
            return result(
                Allow,
                "tool.workflowDraft.preapproved",
                "Workflow draft file is preapproved".into(),
            );
        }
        if self.config.allowed_tools.contains(tool) {
            return result(
                Allow,
                "rule.allowedTools",
                format!("Tool {tool} is explicitly allowed"),
            );
        }
        if ctx.mode == Mode::Edit
            && cap.permission_name.as_deref() == Some("edit")
            && cap.side_effect_scope == "workspace"
        {
            return result(
                Allow,
                "mode.edit.fileEdit",
                "Edit mode allows file edit tools".into(),
            );
        }
        build_ladder(cap, self.config.auto_approve_high_risk, result)
    }
}

fn owned_workflow_amend(ctx: &Context<'_>) -> bool {
    let predecessor = &ctx.input["predecessor"];
    ctx.tool == "AmendWorkflow"
        && predecessor["owned_by_this_session"] == true
        && predecessor["stop_reason"] != "user"
}

fn plan_ladder(
    cap: &Resolved,
    result: impl Fn(Behavior, &'static str, String) -> Decision,
) -> Decision {
    use Behavior::{Allow, Deny};
    if cap.read_only && !cap.destructive {
        return result(
            Allow,
            "mode.plan.readOnly",
            "Plan mode allows read-only tool execution".into(),
        );
    }
    if cap.permission_name.as_deref() == Some("mcp") && !cap.destructive {
        return result(
            Allow,
            "mode.plan.mcp",
            "Plan mode allows non-destructive MCP tool execution".into(),
        );
    }
    if cap.allowed_in_plan_mode
        && cap.side_effect_scope == "session"
        && !cap.destructive
        && !cap.needs_approval
    {
        return result(
            Allow,
            "mode.plan.explicitSessionCapability",
            "Plan mode allows this explicit non-destructive session control action".into(),
        );
    }
    result(
        Deny,
        "mode.plan.nonReadOnly",
        "Plan mode only allows read-only, non-destructive tools".into(),
    )
}

fn build_ladder(
    cap: &Resolved,
    auto_approve_high_risk: bool,
    result: impl Fn(Behavior, &'static str, String) -> Decision,
) -> Decision {
    use Behavior::{Allow, Ask};
    if cap.read_only && !cap.destructive && !cap.needs_approval {
        return result(
            Allow,
            "mode.build.readOnly",
            "Build mode allows read-only tools".into(),
        );
    }
    if cap.risk_level == "critical" {
        return result(
            Ask,
            "mode.build.criticalRisk",
            "Critical risk tools require explicit approval".into(),
        );
    }
    if cap.risk_level == "high" && !auto_approve_high_risk {
        return result(
            Ask,
            "mode.build.highRisk",
            "High risk tools require explicit approval".into(),
        );
    }
    if cap.side_effect_scope == "session"
        && cap.risk_level == "low"
        && !cap.destructive
        && !cap.needs_approval
    {
        return result(
            Allow,
            "mode.build.sessionState",
            "Build mode allows low-risk session-local state updates".into(),
        );
    }
    if cap.needs_approval || cap.destructive || cap.side_effect_scope != "none" {
        return result(
            Ask,
            "mode.build.sideEffect",
            "Tool has side effects and requires approval".into(),
        );
    }
    result(
        Allow,
        "mode.build.lowRisk",
        "Build mode allows low-risk tool execution".into(),
    )
}
