//! worktree 工具测试用的替身：记下工作目录修改的会话、脚本化 worktree 操作的执行环境。

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvError, EnvPath, OperationContext};
use rust_cli_kernel_api::env_worktree::{
    WorktreeChanges, WorktreeCreate, WorktreeCreated, WorktreeEntry, WorktreeInspect, WorktreeList,
    WorktreeRemove,
};
use rust_cli_kernel_api::machine::{Git as GitPort, MachineEnv};
use rust_cli_kernel_api::orchestration::OrchestrationError;

use rust_cli_kernel_api::testing::unsupported_machine;

use crate::test_more::TextEnv;

pub(super) const ROOT: &str = "/Users/dev/work";
/// Windows 的工作区根（本机写法）；git 列出的是 `C:/Users/dev/work` 这种写法（F57）。
pub(super) const WINDOWS_ROOT: &str = r"C:\Users\dev\work";

/// 记下工作目录的修改。
#[derive(Default)]
pub(super) struct Session(pub(super) Mutex<Vec<String>>);

#[async_trait]
impl rust_cli_kernel_api::testing::OpsOverride for Session {
    async fn set_workspace(&self, root: EnvPath) -> Result<(), OrchestrationError> {
        self.0.lock_unpoisoned().push(root.as_str().to_owned());
        Ok(())
    }
}

/// 脚本化的 worktree 操作（只有 Git 端口；环境事实由 [`Git::machine`] 交给 `TextEnv`）。
pub(super) struct Git {
    pub(super) existed: bool,
    pub(super) changes: Option<WorktreeChanges>,
    pub(super) removed: Mutex<Vec<String>>,
    /// 按 Git for Windows 的写法列出（`C:/…`，F57）。
    pub(super) windows: bool,
}

impl Git {
    pub(super) fn new() -> Self {
        Self {
            existed: false,
            changes: Some(WorktreeChanges {
                changed_files: 0,
                commits: 0,
            }),
            removed: Mutex::default(),
            windows: false,
        }
    }

    /// 环境事实是 `TextEnv` 的、Git 是自己的机器；其余端口返回 `Unsupported`。
    pub(super) fn machine(self: Arc<Self>) -> MachineEnv {
        unsupported_machine()
            .with_info(Arc::new(TextEnv::new(&[])))
            .with_git(self)
    }
}

#[async_trait]
impl GitPort for Git {
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
            .lock_unpoisoned()
            .push(request.path.as_str().to_owned());
        Ok(true)
    }
}
