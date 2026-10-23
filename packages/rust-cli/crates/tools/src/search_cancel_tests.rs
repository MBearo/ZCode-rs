//! R5-13：Grep / Glob 等执行环境的遍历时被取消（esc），调用立即以取消结束，不再等遍历做完；丢下的遍历
//! future 由执行环境停掉（env-local `search`）。
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, Environment, FileContent, OperationContext, Outcome,
    ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_files::{
    FileStat, GlobFiles, GlobMatches, GrepOutcome, GrepSearch, StatFile,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_kernel_api::testing::ScriptedInteractions;
use rust_cli_kernel_api::tool::{Tool, ToolError};
use serde_json::json;
use tokio::sync::mpsc;

use crate::test_more::Ctx;

/// 遍历永远做不完的执行环境（很大的目录树）。
struct Endless;

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "not used by this test".to_owned(),
    }
}

#[async_trait]
impl Environment for Endless {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(unused())
    }

    async fn read_file(
        &self,
        _workspace: &WorkspaceRef,
        _request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        Err(unused())
    }

    async fn write_file(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        Err(unused())
    }

    async fn start_command(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: StartCommand,
    ) -> Result<(), EnvError> {
        Err(unused())
    }

    fn watch_command(
        &self,
        _operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        stream::iter([Err(unused())]).boxed()
    }

    async fn cancel(&self, _operation: OperationId) -> Result<(), EnvError> {
        Err(unused())
    }

    async fn operation_status(&self, _operation: OperationId) -> Result<Outcome, EnvError> {
        Err(unused())
    }

    async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Ok(())
    }

    async fn stat(
        &self,
        _workspace: &WorkspaceRef,
        _request: StatFile,
    ) -> Result<Option<FileStat>, EnvError> {
        Ok(None)
    }

    async fn glob(
        &self,
        _workspace: &WorkspaceRef,
        _request: GlobFiles,
    ) -> Result<GlobMatches, EnvError> {
        std::future::pending().await
    }

    async fn grep(
        &self,
        _workspace: &WorkspaceRef,
        _request: GrepSearch,
    ) -> Result<GrepOutcome, EnvError> {
        std::future::pending().await
    }
}

async fn cancelled(tool: Arc<dyn Tool>, input: serde_json::Value) -> Result<(), ToolError> {
    let mut ctx = Ctx::new(Arc::new(ScriptedInteractions::default()));
    ctx.env = Arc::new(Endless);
    let context = ctx.build();
    let cancel = context.cancel.clone();
    let (progress, _updates) = mpsc::channel(4);
    let call = tokio::spawn(async move { tool.call(input, context, progress).await.map(drop) });
    cancel.cancel();
    // 期限只用于让失败的测试结束：修复前调用一直等遍历。
    tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("the call ends after the cancel")
        .unwrap()
}

#[tokio::test]
async fn a_cancelled_grep_ends_without_waiting_for_the_walk() {
    let result = cancelled(Arc::new(crate::GrepTool::new()), json!({"pattern": "x"})).await;
    assert!(matches!(result, Err(ToolError::Cancelled)), "{result:?}");
}

#[tokio::test]
async fn a_cancelled_glob_ends_without_waiting_for_the_walk() {
    let result = cancelled(
        Arc::new(crate::GlobTool::new()),
        json!({"pattern": "**/*.rs"}),
    )
    .await;
    assert!(matches!(result, Err(ToolError::Cancelled)), "{result:?}");
}
