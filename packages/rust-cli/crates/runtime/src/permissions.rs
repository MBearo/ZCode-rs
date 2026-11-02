//! `/permissions` 的列表与修改（docs/specs/rust-cli/tui-manage.md §1.1）。
//!
//! 判决链只看 `PolicyGate` 的快照；LocalSettings 的持久值只在会话存储（Node 的 `local_setting`）。
//! 修改先落库，再用落库后读回的规则集换上新快照（快照是 db 的投影，不另存，policy `PolicyGate::replace_local`）。
//! “总是允许”的回答走同一条路径（判决链经 [`StoredRules`] 读回）。

use std::sync::Arc;

use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::permission::{
    PermissionRule, PermissionRules, PermissionRulesChange, PermissionRulesQuery, ProjectRules,
    RuleChange, RuleSource, SourcedRule,
};
use rust_cli_kernel_api::session_api::KernelError;
use rust_cli_kernel_api::store::{LocalSettings, ProjectRulesChange};
use rust_cli_policy::{LocalRulesSource, PolicyGate};

use rust_cli_kernel_api::permission::syntax::{format_rule, parse_rule};

/// 列表：快照里的配置文件规则、db 现读的 LocalSettings、命令行规则与会话规则（只在内存里，进程里共用）。
pub(crate) async fn listing(
    gate: &PolicyGate,
    settings: &dyn LocalSettings,
    query: &PermissionRulesQuery,
) -> Result<Vec<SourcedRule>, KernelError> {
    let snapshot = gate.rules();
    // 审查 K7：以前这里另有“存储不支持时用快照里启动时读到的”一支；本机设置的方法改成必需之后没有不支持的存储。
    let local = settings.project_rules(&query.workspace).await?.sourced();
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

/// 本机项目规则集的来源：会话存储（Node 的 `local_setting`）。判决链在“总是允许”落库后从这里读回 LocalSettings 层
/// （shell-permissions.md §5）。
///
/// F125（T1）：以前这里是包在 [`PolicyGate`] 外面、交给执行管线的一层判决链，只为“落库后读回”多转发一个方法，却漏转发了
/// 当时的 `ask_once`（缺陷 D1；F137 起它并进判决结果）。现在管线直接拿判决链，读回经 policy 定义的这个小端口。
pub(crate) struct StoredRules(pub(crate) Arc<dyn LocalSettings>);

#[async_trait::async_trait]
impl LocalRulesSource for StoredRules {
    async fn read(&self, workspace: &WorkspaceRef) -> Option<ProjectRules> {
        match self.0.project_rules(workspace).await {
            Ok(rules) => Some(rules),
            Err(error) => {
                tracing::warn!(%error, "the local permission rules could not be read back");
                None
            }
        }
    }
}

async fn change_local(
    gate: &PolicyGate,
    settings: &dyn LocalSettings,
    request: &PermissionRulesChange,
    change: ProjectRulesChange,
) -> Result<(), KernelError> {
    let rules = settings
        .change_project_rules(&request.workspace, change)
        .await?;
    gate.replace_local(&rules);
    Ok(())
}

/// 一次修改，返回修改后的列表。
pub(crate) async fn change(
    gate: &PolicyGate,
    settings: &dyn LocalSettings,
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
            change_local(gate, settings, &request, change).await?;
        }
        RuleChange::Remove { rule } => match rule.source {
            RuleSource::LocalSettings => {
                let change = ProjectRulesChange {
                    behavior: rule.behavior,
                    add: Vec::new(),
                    remove: vec![rule.rule.clone()],
                };
                change_local(gate, settings, &request, change).await?;
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
    listing(gate, settings, &query).await
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
