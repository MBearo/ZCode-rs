//! Bash permission inputs (Node `resolveBashPermissionCapability` and
//! `resolveBashPermissionRulePolicy`).
use crate::{contract::ToolPermission, domain::permission::ToolCapability};
use serde_json::Value;
use std::path::Path;

pub(super) async fn resolve(
    cwd: &Path,
    name: &str,
    args: &Value,
    capability: ToolCapability,
) -> ToolPermission {
    let command = (name == "Bash").then(|| args["command"].as_str()).flatten();
    let Some(command) = command else {
        let suggestions = crate::domain::permission::default_updates(
            name,
            args,
            capability.permission_capability_group.as_deref(),
        );
        return ToolPermission {
            capability,
            rules: None,
            suggestions,
        };
    };
    let analysis = zcode_cli_bash::analyze(command);
    // git 运行环境只在命令调用 git 时检查，整条命令与逐段判定共用一次结果。
    let git_unsafe = zcode_cli_bash::invokes_git(&analysis.commands)
        && super::git_safety::unsafe_context(cwd).await;
    let capability = if zcode_cli_bash::is_read_only_analysis(&analysis, git_unsafe) {
        capability.read_only_command()
    } else {
        capability
    };
    let rules = zcode_cli_bash::BashRules::new(command, git_unsafe);
    ToolPermission {
        capability,
        suggestions: rules.suggestions.clone(),
        rules: Some(Box::new(rules)),
    }
}
