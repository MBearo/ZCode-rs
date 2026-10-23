//! worktree 工具测试用的替身：记下工作目录修改的会话、脚本化 worktree 操作的执行环境。

use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, Environment, FileContent, OperationContext, Outcome,
    ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_files::PathFacts;
use rust_cli_kernel_api::env_worktree::{
    WorktreeChanges, WorktreeCreate, WorktreeCreated, WorktreeEntry, WorktreeInspect, WorktreeList,
    WorktreeRemove,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError,
};

use crate::test_more::TextEnv;

pub(super) const ROOT: &str = "/Users/dev/work";
/// Windows 的工作区根（本机写法）；git 列出的是 `C:/Users/dev/work` 这种写法（F57）。
pub(super) const WINDOWS_ROOT: &str = r"C:\Users\dev\work";

/// 记下工作目录的修改。
#[derive(Default)]
pub(super) struct Session(pub(super) Mutex<Vec<String>>);

#[async_trait]
impl Interactions for Session {
    async fn ask(
        &self,
        _session: &SessionId,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        unreachable!("not asked")
    }

    async fn set_workspace(
        &self,
        _session: &SessionId,
        root: EnvPath,
    ) -> Result<(), OrchestrationError> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(root.as_str().to_owned());
        Ok(())
    }
}

/// 脚本化的 worktree 操作；其余操作交给 `TextEnv`。
pub(super) struct Git {
    inner: TextEnv,
    pub(super) existed: bool,
    pub(super) changes: Option<WorktreeChanges>,
    pub(super) removed: Mutex<Vec<String>>,
    /// 按 Git for Windows 的写法列出（`C:/…`，F57）。
    pub(super) windows: bool,
}

impl Git {
    pub(super) fn new() -> Self {
        Self {
            inner: TextEnv::new(&[]),
            existed: false,
            changes: Some(WorktreeChanges {
                changed_files: 0,
                commits: 0,
            }),
            removed: Mutex::default(),
            windows: false,
        }
    }
}

#[async_trait]
impl Environment for Git {
    async fn facts(&self, workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        self.inner.facts(workspace).await
    }

    async fn read_file(
        &self,
        workspace: &WorkspaceRef,
        request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        self.inner.read_file(workspace, request).await
    }

    async fn write_file(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        self.inner.write_file(workspace, context, request).await
    }

    async fn start_command(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<(), EnvError> {
        self.inner.start_command(workspace, context, request).await
    }

    fn watch_command(
        &self,
        operation: OperationId,
        cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        self.inner.watch_command(operation, cursor)
    }

    async fn cancel(&self, operation: OperationId) -> Result<(), EnvError> {
        self.inner.cancel(operation).await
    }

    async fn operation_status(&self, operation: OperationId) -> Result<Outcome, EnvError> {
        self.inner.operation_status(operation).await
    }

    async fn close_session(&self, session: &SessionId, forget: bool) -> Result<(), EnvError> {
        self.inner.close_session(session, forget).await
    }

    async fn path_facts(&self, workspace: &WorkspaceRef) -> Result<PathFacts, EnvError> {
        self.inner.path_facts(workspace).await
    }

    async fn worktree_create(
        &self,
        _context: OperationContext,
        request: WorktreeCreate,
    ) -> Result<WorktreeCreated, EnvError> {
        let flat = request.name.replace('/', "+");
        Ok(WorktreeCreated::Ready {
            path: EnvPath::new(format!("{ROOT}/.zcode/worktrees/{flat}")),
            branch: format!("worktree-{flat}"),
            head_commit: Some("abc".to_owned()),
            existed: self.existed,
        })
    }

    async fn worktree_list(&self, _cwd: EnvPath) -> Result<WorktreeList, EnvError> {
        let entry = |path: &str, branch: &str| WorktreeEntry {
            path: EnvPath::new(path),
            branch: Some(branch.to_owned()),
        };
        // Git for Windows 的 `worktree list` 与 `rev-parse` 给出 `C:/…`。
        let (root, elsewhere) = if self.windows {
            ("C:/Users/dev/work", "C:/Users/dev/elsewhere")
        } else {
            (ROOT, "/Users/dev/elsewhere")
        };
        Ok(WorktreeList {
            root: Some(EnvPath::new(root)),
            entries: vec![
                entry(root, "main"),
                entry(&format!("{root}/.zcode/worktrees/old"), "worktree-old"),
                entry(elsewhere, "side"),
            ],
        })
    }

    async fn worktree_changes(
        &self,
        _request: WorktreeInspect,
    ) -> Result<Option<WorktreeChanges>, EnvError> {
        Ok(self.changes)
    }

    async fn worktree_remove(
        &self,
        _context: OperationContext,
        request: WorktreeRemove,
    ) -> Result<bool, EnvError> {
        self.removed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.path.as_str().to_owned());
        Ok(true)
    }
}
