//! LocalSettings 层（本机项目规则集）的读回与替换（shell-permissions.md §5、tui-manage.md §1.1）。
//!
//! 规则只有快照一个来源：“总是允许”与 `/permissions` 的修改先落库，再用落库后读回的规则集换上新的 LocalSettings 层
//! （快照是库的投影，不另存）。判决链不做 IO，库由 [`LocalRulesSource`] 读（runtime 用会话存储实现）。

use async_trait::async_trait;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::permission::{PermissionRules, ProjectRules, RuleSource};

use crate::PolicyGate;

/// 本机项目规则集从哪里读回。
#[async_trait]
pub trait LocalRulesSource: Send + Sync {
    /// `workspace` 的本机项目规则集；读不出时为 None（保留旧快照）。
    async fn read(&self, workspace: &WorkspaceRef) -> Option<ProjectRules>;
}

impl PolicyGate {
    /// 接上本机项目规则集的来源（[`PermissionGate::local_rules_committed`] 从它读回）。
    ///
    /// [`PermissionGate::local_rules_committed`]: rust_cli_kernel_api::orchestration::PermissionGate::local_rules_committed
    #[must_use]
    pub fn with_local_rules(mut self, source: std::sync::Arc<dyn LocalRulesSource>) -> Self {
        self.local = Some(source);
        self
    }

    /// 换上新快照：其余来源不变，LocalSettings 换成 `local`（落库后读回的规则集）。
    pub fn replace_local(&self, local: &ProjectRules) {
        let snapshot = self.rules();
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
        self.replace_rules(PermissionRules {
            version: snapshot.version + 1,
            rules,
            ..(*snapshot).clone()
        });
    }

    /// 从来源读回 `workspace` 的规则集、换上 LocalSettings 层；没有来源或读不出时保留旧快照。
    pub(crate) async fn reload_local(&self, workspace: &WorkspaceRef) {
        let Some(source) = &self.local else {
            return;
        };
        if let Some(rules) = source.read(workspace).await {
            self.replace_local(&rules);
        }
    }
}
