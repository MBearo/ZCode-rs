//! `/cd` 的规则文件（docs/specs/rust-cli/tui-manage.md §7.1）：从文件系统根到新目录的每一层，依次 Project 文件、
//! Local 文件、`.zcode/rules` 的无条件规则；
//! 已经在 `instructions` 里宣布过的路径不再列。结果用 `instructions` 的格式。

use rust_cli_base::names::{DOT_DIR, LOCAL_RULES_FILE, RULES_FILE};
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::message::Message;

use crate::PromptAssembler;
use crate::instructions;
use crate::rules::{Kind, RULES_DIR, RuleFile, Scope, Walk, unconditional};

impl Scope<'_> {
    async fn directory(&self) -> Vec<RuleFile> {
        let style = self.style;
        let mut walk = Walk::new(self);
        let mut out = Vec::new();
        for dir in style.ancestors(&self.cwd) {
            for file in [
                style.join(&dir, &[RULES_FILE]),
                style.join(&dir, &[DOT_DIR, RULES_FILE]),
            ] {
                out.extend(walk.file(file, Kind::Project, false).await);
            }
            let local = style.join(&dir, &[LOCAL_RULES_FILE]);
            out.extend(walk.file(local, Kind::Local, false).await);
            let rules = style.join(&dir, &[DOT_DIR, RULES_DIR]);
            out.extend(unconditional(walk.rules(rules, Kind::Project, false).await));
        }
        out
    }
}

impl PromptAssembler {
    /// 新目录 `workspace.root` 的规则文件文字；没有新的文件（或读不到环境事实）时为 None。`history` 是会话的
    /// 活动历史（找已宣布过的路径）。
    pub async fn directory_rules(
        &self,
        workspace: &WorkspaceRef,
        history: &[Message],
    ) -> Option<String> {
        let facts = self.ports.info.facts(workspace).await.ok()?;
        let scope = Scope {
            files: self.ports.files.as_ref(),
            workspace,
            style: PathStyle::of_platform(&facts.platform),
            cwd: workspace.root.as_str().to_owned(),
            home: facts.home.as_ref().map(|home| home.as_str().to_owned()),
        };
        let known: Vec<String> = instructions::announced(history)
            .unwrap_or_default()
            .into_iter()
            .map(|file| file.path)
            .collect();
        let files: Vec<RuleFile> = scope
            .directory()
            .await
            .into_iter()
            .filter(|file| !known.contains(&file.path) && !file.content.trim().is_empty())
            .collect();
        instructions::listed(&files)
    }
}

/// 会话 actor 判定 `/cd` 可以移动后，用自己的活动历史调用（F1，R1-3：不再把历史复制给 runtime）。
#[async_trait::async_trait]
impl rust_cli_kernel_api::orchestration::DirectoryRules for PromptAssembler {
    async fn directory_rules(
        &self,
        workspace: &WorkspaceRef,
        history: &[Message],
    ) -> Option<String> {
        PromptAssembler::directory_rules(self, workspace, history).await
    }
}

#[cfg(test)]
#[path = "directory_tests.rs"]
mod tests;
