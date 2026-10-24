//! ExitWorktree（docs/specs/rust-cli/tools-more.md §11.3）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env_worktree::{WorktreeInspect, WorktreeRemove};
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::common::{Definition, definition, failed, parse, text};
use crate::worktree::{WorktreeBoard, WorktreeSession, invalid, owner, switch};
use crate::worktree_texts as texts;

pub const EXIT: &str = "ExitWorktree";

#[derive(Deserialize)]
struct ExitInput {
    action: String,
    #[serde(default)]
    discard_changes: Option<bool>,
}

pub struct ExitWorktreeTool {
    definition: Definition,
    board: Arc<WorktreeBoard>,
}

impl ExitWorktreeTool {
    pub(crate) fn new(board: &Arc<WorktreeBoard>) -> Self {
        Self {
            definition: definition(EXIT),
            board: board.clone(),
        }
    }
}

#[async_trait]
impl Tool for ExitWorktreeTool {
    fn name(&self) -> &str {
        EXIT
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("exit a worktree session and return to the original directory")
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(100_000)
    }

    async fn validate_input(&self, input: &Value, context: &ToolUseContext) -> Validation {
        let Ok(input) = parse::<ExitInput>(input) else {
            return Validation::Ok;
        };
        let Some(session) = self.board.get(&owner(context).await) else {
            return invalid(texts::NO_SESSION.to_owned(), Some(1));
        };
        if input.action != "remove" {
            return Validation::Ok;
        }
        if session.entered_existing {
            let message = texts::not_owner(session.path.as_str(), session.original.as_str());
            return invalid(message, Some(4));
        }
        if input.discard_changes == Some(true) {
            return Validation::Ok;
        }
        match inspect(context, &session).await {
            None => invalid(texts::unverified(session.path.as_str()), Some(3)),
            Some((files, commits)) if files > 0 || commits > 0 => invalid(
                texts::has_work(files, commits, session.branch.as_deref()),
                Some(2),
            ),
            Some(_) => Validation::Ok,
        }
    }

    /// 没有专属的权限检查，默认放行。
    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let input: ExitInput = parse(&input)?;
        let owner = owner(&context).await;
        let Some(session) = self.board.get(&owner) else {
            return Err(failed("Not in a worktree session".to_owned()));
        };
        let (files, commits) = inspect(&context, &session).await.unwrap_or((0, 0));
        // 先切回原目录（shell cwd 不留在要删的目录里），成功后才清除记录。
        switch(&context, session.original.clone()).await?;
        self.board.sessions().remove(&owner);
        let (path, original) = (session.path.as_str(), session.original.as_str());
        let message = if input.action == "keep" {
            texts::kept(path, session.branch.as_deref(), original)
        } else {
            let request = WorktreeRemove {
                path: session.path.clone(),
                branch: session.branch.clone(),
            };
            let removed = context
                .local
                .git()
                .worktree_remove(context.operation.clone(), request)
                .await
                .unwrap_or(false);
            if removed {
                texts::removed(path, files, commits, original)
            } else {
                texts::not_removed(path, original)
            }
        };
        Ok(text(message))
    }
}

/// worktree 里未保存的工作（未提交文件数、新提交数）；查不到时 None。
async fn inspect(context: &ToolUseContext, session: &WorktreeSession) -> Option<(u64, u64)> {
    let request = WorktreeInspect {
        path: session.path.clone(),
        base: session.head_commit.clone(),
    };
    let found = context.local.git().worktree_changes(request).await.ok()??;
    Some((found.changed_files, found.commits))
}
