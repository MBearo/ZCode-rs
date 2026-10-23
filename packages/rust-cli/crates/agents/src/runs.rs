//! 正在进行的运行（主循环与子循环）：按 `RunId` 记下本次运行的会话、工作区、模型与模式
//! （docs/specs/rust-cli/agents.md §4.1）。
//!
//! Agent 工具要用父运行的模型与模式，`ToolUseContext` 只带 `run`。运行开始时登记、结束（或被丢弃）时
//! 删除，所以查到的总是这次运行自己的值，不会过期。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::{RunId, SessionId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::{
    QueryError, QueryRequest, QueryRunner, QueryStop, RunSink,
};
use rust_cli_kernel_api::permission::PermissionMode;

/// 一次运行的事实（取自它的 [`QueryRequest`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunInfo {
    pub(crate) session: SessionId,
    pub(crate) workspace: WorkspaceRef,
    pub(crate) selection: ModelSelection,
    pub(crate) mode: PermissionMode,
}

#[derive(Default)]
pub(crate) struct RunBook(Mutex<HashMap<RunId, RunInfo>>);

impl RunBook {
    fn runs(&self) -> std::sync::MutexGuard<'_, HashMap<RunId, RunInfo>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn get(&self, run: &RunId) -> Option<RunInfo> {
        self.runs().get(run).cloned()
    }
}

/// 运行结束或 future 被丢弃时删除登记。
struct Entry<'a> {
    book: &'a RunBook,
    run: RunId,
}

impl Drop for Entry<'_> {
    fn drop(&mut self) {
        self.book.runs().remove(&self.run);
    }
}

/// 给 [`QueryRunner`] 包一层登记（主循环由 runtime 包，子循环由 agents 自己包）。
pub struct Tracked {
    pub(crate) inner: Arc<dyn QueryRunner>,
    pub(crate) book: Arc<RunBook>,
}

#[async_trait]
impl QueryRunner for Tracked {
    async fn run(
        &self,
        request: QueryRequest,
        sink: Arc<dyn RunSink>,
        abort: Abort,
    ) -> Result<QueryStop, QueryError> {
        let info = RunInfo {
            session: request.session.clone(),
            workspace: request.workspace.clone(),
            selection: request.selection.clone(),
            mode: request.mode,
        };
        self.book.runs().insert(request.run.clone(), info);
        let _entry = Entry {
            book: &self.book,
            run: request.run.clone(),
        };
        self.inner.run(request, sink, abort).await
    }
}
