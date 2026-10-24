//! `/permissions` 的列表与修改（docs/specs/rust-cli/tui-manage.md §1.1）。
//!
//! 判决链只看 `PolicyGate` 的快照；LocalSettings 的持久值只在会话存储（Node 的 `local_setting`）。
//! 修改先落库，再用落库后读回的规则集换上新快照（快照是 db 的投影，不另存）。“总是允许”的回答走同一条路径
//! （[`SharedRules`]）。

use std::sync::Arc;

use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::orchestration::PermissionGate;
use rust_cli_kernel_api::permission::{
    PermissionDecision, PermissionRule, PermissionRules, PermissionRulesChange,
    PermissionRulesQuery, ProjectRules, RuleChange, RuleSource, SourcedRule,
};
use rust_cli_kernel_api::session_api::KernelError;
use rust_cli_kernel_api::store::{ProjectRulesChange, SessionStore, StoreError};
use rust_cli_kernel_api::tool::{Tool, ToolUseContext};
use rust_cli_policy::PolicyGate;
use serde_json::Value;

pub use rust_cli_policy::rules::{format_rule, parse_rule};
// Bash 询问框“不再询问”一行的种子与文字（shell-permissions.md §4.10，工作线 F72）：tui 不依赖 policy。
pub use rust_cli_policy::{bash_rule_label, bash_rule_seed};

fn kernel_error(error: StoreError) -> KernelError {
    match error {
        StoreError::Unsupported { what } => KernelError::Unsupported { what },
        other => KernelError::Failed {
            message: other.to_string(),
        },
    }
}

/// 列表：快照里的配置文件规则、db 现读的 LocalSettings、命令行规则与会话规则（只在内存里，进程里共用）。
pub(crate) async fn listing(
    gate: &PolicyGate,
    store: &dyn SessionStore,
    query: &PermissionRulesQuery,
) -> Result<Vec<SourcedRule>, KernelError> {
    let snapshot = gate.rules();
    let local = match store.project_rules(&query.workspace).await {
        Ok(rules) => rules.sourced(),
        // 不支持的存储：用快照里启动时读到的。
        Err(StoreError::Unsupported { .. }) => snapshot
            .rules
            .iter()
            .filter(|rule| rule.source == RuleSource::LocalSettings)
            .cloned()
            .collect(),
        Err(error) => return Err(kernel_error(error)),
    };
    let from = |sources: &[RuleSource]| {
        snapshot
            .rules
            .iter()
            .filter(|rule| sources.contains(&rule.source))
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut rules = from(&[RuleSource::UserSettings, RuleSource::ProjectSettings]);
    rules.extend(local);
    rules.extend(from(&[RuleSource::CliArg, RuleSource::Session]));
    Ok(rules)
}

/// 换上新快照：其余来源不变，LocalSettings 换成落库后读回的规则集。
fn replace_local(gate: &PolicyGate, local: &ProjectRules) {
    let snapshot = gate.rules();
    let keep = |sources: &[RuleSource]| {
        snapshot
            .rules
            .iter()
            .filter(|rule| sources.contains(&rule.source))
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut rules = keep(&[RuleSource::UserSettings, RuleSource::ProjectSettings]);
    rules.extend(local.sourced());
    rules.extend(keep(&[RuleSource::CliArg, RuleSource::Session]));
    gate.replace_rules(PermissionRules {
        version: snapshot.version + 1,
        rules,
        ..(*snapshot).clone()
    });
}

/// 读回 `workspace` 的本机项目规则集，换上新的 LocalSettings 层（“总是允许”落库之后）。读不出时保留旧快照。
async fn reload_local(gate: &PolicyGate, store: &dyn SessionStore, workspace: &WorkspaceRef) {
    match store.project_rules(workspace).await {
        Ok(rules) => replace_local(gate, &rules),
        Err(StoreError::Unsupported { .. }) => {}
        Err(error) => tracing::warn!(%error, "the local permission rules could not be read back"),
    }
}

/// 执行管线用的判决链：判决交给 [`PolicyGate`]；“总是允许”落库后读回库值、换上 LocalSettings 层
/// （shell-permissions.md §5）。规则只有快照一个来源，进程里的主线程、子代理与之后的会话共用一份权限上下文。
pub(crate) struct SharedRules {
    pub(crate) gate: Arc<PolicyGate>,
    pub(crate) store: Arc<dyn SessionStore>,
}

#[async_trait::async_trait]
impl PermissionGate for SharedRules {
    async fn decide(
        &self,
        tool: &dyn Tool,
        input: &Value,
        context: &ToolUseContext,
    ) -> PermissionDecision {
        self.gate.decide(tool, input, context).await
    }

    async fn check_rules(
        &self,
        tool: &dyn Tool,
        input: &Value,
        context: &ToolUseContext,
    ) -> Option<PermissionDecision> {
        self.gate.check_rules(tool, input, context).await
    }

    async fn local_rules_committed(&self, workspace: &WorkspaceRef) {
        reload_local(&self.gate, self.store.as_ref(), workspace).await;
    }
}

async fn change_local(
    gate: &PolicyGate,
    store: &dyn SessionStore,
    request: &PermissionRulesChange,
    change: ProjectRulesChange,
) -> Result<(), KernelError> {
    let rules = store
        .change_project_rules(&request.workspace, change)
        .await
        .map_err(kernel_error)?;
    replace_local(gate, &rules);
    Ok(())
}

/// 一次修改，返回修改后的列表。
pub(crate) async fn change(
    gate: &PolicyGate,
    store: &dyn SessionStore,
    request: PermissionRulesChange,
) -> Result<Vec<SourcedRule>, KernelError> {
    match &request.change {
        RuleChange::Add { behavior, rules } => {
            let add: Vec<PermissionRule> = rules
                .iter()
                .map(|text| text.trim())
                .filter(|text| !text.is_empty())
                .map(parse_rule)
                .collect();
            let change = ProjectRulesChange {
                behavior: *behavior,
                add,
                remove: Vec::new(),
            };
            change_local(gate, store, &request, change).await?;
        }
        RuleChange::Remove { rule } => match rule.source {
            RuleSource::LocalSettings => {
                let change = ProjectRulesChange {
                    behavior: rule.behavior,
                    add: Vec::new(),
                    remove: vec![rule.rule.clone()],
                };
                change_local(gate, store, &request, change).await?;
            }
            // 命令行与会话规则只在内存里去掉（cliArg、session 两种来源）。
            RuleSource::CliArg | RuleSource::Session => {
                let snapshot = gate.rules();
                let rules = snapshot
                    .rules
                    .iter()
                    .filter(|known| *known != rule)
                    .cloned()
                    .collect();
                gate.replace_rules(PermissionRules {
                    version: snapshot.version + 1,
                    rules,
                    ..(*snapshot).clone()
                });
            }
            RuleSource::UserSettings | RuleSource::ProjectSettings => {
                return Err(KernelError::Failed {
                    message: format!(
                        "{} comes from a read-only source and cannot be modified here",
                        format_rule(&rule.rule)
                    ),
                });
            }
        },
    }
    let query = PermissionRulesQuery {
        workspace: request.workspace,
        session: request.session,
    };
    listing(gate, store, &query).await
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
