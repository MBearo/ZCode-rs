//! R5-13：Grep / Glob 等执行环境的遍历时被取消（esc），调用立即以取消结束，不再等遍历做完；丢下的遍历
//! future 由执行环境停掉（env-local `search`）。
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{
    EnvError, FileContent, OperationContext, ReadFile, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_files::{
    FileStat, GlobFiles, GlobMatches, GrepOutcome, GrepSearch, StatFile,
};
use rust_cli_kernel_api::machine::{Files, Search};
use rust_cli_kernel_api::testing::{ScriptedInteractions, unsupported_machine};
use rust_cli_kernel_api::tool::{Tool, ToolError};
use serde_json::json;
use tokio::sync::mpsc;

use crate::test_more::Ctx;

/// 遍历永远做不完的文件与搜索端口（很大的目录树）。
struct Endless;

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "not used by this test".to_owned(),
    }
}

#[async_trait]
impl Files for Endless {
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

    async fn stat(
        &self,
        _workspace: &WorkspaceRef,
        _request: StatFile,
    ) -> Result<Option<FileStat>, EnvError> {
        Ok(None)
    }
}

#[async_trait]
impl Search for Endless {
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
    let endless = Arc::new(Endless);
    ctx.set_machine(
        unsupported_machine()
            .with_files(endless.clone())
            .with_search(endless),
    );
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
