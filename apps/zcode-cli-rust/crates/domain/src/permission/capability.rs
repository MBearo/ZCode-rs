//! Tool capability declarations and their Node `resolveCapability` fallbacks.
use serde::Deserialize;
use serde_json::Value;

/// Node `PermissionToolSpec` subset used by the policy.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSpec {
    pub permission: Option<String>,
    pub risk_level: Option<String>,
    pub side_effect_scope: Option<String>,
    pub needs_approval: Option<bool>,
    pub always_ask: Option<bool>,
    pub ask_options: Option<Value>,
}

/// Node `PermissionToolCapability`: what a tool declares, before defaults.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCapability {
    pub allowed_in_plan_mode: Option<bool>,
    pub always_ask: Option<bool>,
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub requires_user_interaction: Option<bool>,
    pub side_effect_scope: Option<String>,
    pub risk_level: Option<String>,
    pub needs_approval: Option<bool>,
    pub permission_capability_group: Option<String>,
    pub permission: Option<PermissionSpec>,
}

impl ToolCapability {
    /// Node MCP tool metadata (`core/src/mcp/index.ts`): annotations decide
    /// read-only and destructive; every MCP call needs approval.
    pub fn mcp(read_only: bool, destructive: bool) -> Self {
        let risk = if destructive {
            "high"
        } else if read_only {
            "low"
        } else {
            "medium"
        };
        Self {
            read_only: Some(read_only),
            destructive: Some(destructive),
            side_effect_scope: Some("network".into()),
            risk_level: Some(risk.into()),
            needs_approval: Some(true),
            permission: Some(PermissionSpec {
                permission: Some("mcp".into()),
                risk_level: Some(risk.into()),
                side_effect_scope: Some("network".into()),
                needs_approval: Some(true),
                ..PermissionSpec::default()
            }),
            ..Self::default()
        }
    }

    /// Bash commands classified read-only drop their approval requirement.
    pub fn read_only_command(mut self) -> Self {
        let override_ = |spec: &mut PermissionSpec| {
            spec.needs_approval = Some(false);
            spec.risk_level = Some("low".into());
            spec.side_effect_scope = Some("none".into());
        };
        self.read_only = Some(true);
        self.destructive = Some(false);
        self.needs_approval = Some(false);
        self.risk_level = Some("low".into());
        self.side_effect_scope = Some("none".into());
        override_(self.permission.get_or_insert_with(PermissionSpec::default));
        self
    }
}

const FALLBACK_READ_ONLY: [&str; 11] = [
    "Read",
    "Glob",
    "Grep",
    "WebSearch",
    "WebFetch",
    "TodoRead",
    "TodoWrite",
    "AskUserQuestion",
    "Agent",
    "Task",
    "Skill",
];

/// Node `ResolvedPermissionCapability`.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub allowed_in_plan_mode: bool,
    pub always_ask: bool,
    pub read_only: bool,
    pub destructive: bool,
    pub requires_user_interaction: bool,
    pub side_effect_scope: String,
    pub risk_level: String,
    pub needs_approval: bool,
    pub capability_group: Option<String>,
    pub permission_name: Option<String>,
}

/// Node `resolveCapability`, including the tool-name fallbacks.
pub fn resolve(tool: &str, cap: &ToolCapability) -> Resolved {
    let fallback_read_only = FALLBACK_READ_ONLY.contains(&tool);
    let spec = cap.permission.as_ref();
    let scope = spec
        .and_then(|s| s.side_effect_scope.clone())
        .or_else(|| cap.side_effect_scope.clone());
    let risk = spec
        .and_then(|s| s.risk_level.clone())
        .or_else(|| cap.risk_level.clone())
        // Node 对写工具与其余工具的兜底风险都是 medium。
        .unwrap_or_else(|| if fallback_read_only { "low" } else { "medium" }.into());
    Resolved {
        allowed_in_plan_mode: cap.allowed_in_plan_mode.unwrap_or(false),
        always_ask: spec
            .and_then(|s| s.always_ask)
            .or(cap.always_ask)
            .unwrap_or(false),
        read_only: cap.read_only.unwrap_or(fallback_read_only),
        destructive: cap.destructive.unwrap_or(tool == "Bash"),
        requires_user_interaction: cap
            .requires_user_interaction
            .unwrap_or(scope.as_deref() == Some("userInteraction")),
        side_effect_scope: scope.unwrap_or_else(|| {
            if fallback_read_only {
                "none"
            } else {
                "workspace"
            }
            .into()
        }),
        risk_level: risk,
        needs_approval: spec
            .and_then(|s| s.needs_approval)
            .or(cap.needs_approval)
            .unwrap_or(!fallback_read_only),
        capability_group: cap.permission_capability_group.clone(),
        permission_name: spec.and_then(|s| s.permission.clone()),
    }
}
