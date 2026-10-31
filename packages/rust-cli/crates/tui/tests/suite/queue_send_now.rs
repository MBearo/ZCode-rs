//! 立即发送的键在输入框为空时的替身（修复线 F105，docs/specs/rust-cli/tui.md §5.1.1）：执行环境记下“把前台命令转到
//! 后台”的请求，其余操作同 [`UnusedEnvironment`]。测试在 `queue.rs`。
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::env::{
    BackgroundReason, CommandEvent, EnvError, EnvFacts, FileContent, OperationContext, ReadFile,
    StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId, ShellTaskId};
use rust_cli_kernel_api::local_services::{Fetch, Git};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell,
};
use rust_cli_kernel_api::testing::UnusedEnvironment;

/// 记下每次 `background_foreground` 的原因（没有前台命令，转了 0 个）。
#[derive(Default)]
pub struct MovesEnv {
    moved: Mutex<Vec<BackgroundReason>>,
}

impl MovesEnv {
    pub fn moved(&self) -> Vec<BackgroundReason> {
        self.moved.lock().unwrap().clone()
    }

    /// 等到记下 `count` 次请求（最多 10 秒，同界面的等待）。
    pub async fn wait_moved(&self, count: usize) {
        let wait = async {
            while self.moved.lock().unwrap().len() < count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        if tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .is_err()
        {
            panic!(
                "timed out waiting for {count} background requests: {:?}",
                self.moved()
            );
        }
    }
}

#[async_trait]
impl Background for MovesEnv {
    async fn background_foreground(
        &self,
        _session: &SessionId,
        reason: BackgroundReason,
    ) -> Result<Vec<ShellTaskId>, EnvError> {
        self.moved.lock().unwrap().push(reason);
        Ok(Vec::new())
    }
}

#[async_trait]
impl EnvInfo for MovesEnv {
    async fn facts(&self, workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        UnusedEnvironment.facts(workspace).await
    }
}

#[async_trait]
impl Files for MovesEnv {
    async fn read_file(
        &self,
        workspace: &WorkspaceRef,
        request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        UnusedEnvironment.read_file(workspace, request).await
    }

    async fn write_file(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        UnusedEnvironment
            .write_file(workspace, context, request)
            .await
    }
}

#[async_trait]
impl Commands for MovesEnv {
    async fn start_command(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
        UnusedEnvironment
            .start_command(workspace, context, request)
            .await
    }

    fn watch_command(
        &self,
        operation: OperationId,
        cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        UnusedEnvironment.watch_command(operation, cursor)
    }

    async fn cancel(&self, operation: OperationId) -> Result<(), EnvError> {
        UnusedEnvironment.cancel(operation).await
    }

    async fn close_session(&self, session: &SessionId, forget: bool) -> Result<(), EnvError> {
        UnusedEnvironment.close_session(session, forget).await
    }
}

impl Search for MovesEnv {}
impl Shell for MovesEnv {}
impl Checkpoints for MovesEnv {}
impl Pdf for MovesEnv {}
impl Git for MovesEnv {}
impl Fetch for MovesEnv {}
