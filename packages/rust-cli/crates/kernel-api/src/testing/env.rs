//! 执行环境的测试替身。

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};

use crate::env::{
    CommandEvent, EnvError, EnvFacts, Environment, FileContent, FileVersion, OperationContext,
    Outcome, ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use crate::ids::{OperationId, SessionId};

/// 文本回路用不到执行环境：每个操作都返回 `Unsupported`。
pub struct UnusedEnvironment;

/// 只提供环境事实与规则文件的执行环境：`facts` 返回给定值，`read_file` 从给定的文件表里读，
/// 表里没有的路径返回 `NotFound`；其余操作返回 `Unsupported`。
pub struct FactsEnvironment {
    pub facts: EnvFacts,
    pub files: Vec<(String, String)>,
}

#[async_trait]
impl Environment for FactsEnvironment {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Ok(self.facts.clone())
    }

    async fn read_file(
        &self,
        _workspace: &WorkspaceRef,
        request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        self.files
            .iter()
            .find(|(path, _)| path == request.path.as_str())
            .map(|(_, text)| FileContent {
                bytes: text.clone().into_bytes(),
                version: FileVersion::new("v1"),
            })
            .ok_or_else(|| EnvError::NotFound {
                path: request.path.as_str().to_owned(),
            })
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
}

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "test environment".to_owned(),
    }
}

#[async_trait]
impl Environment for UnusedEnvironment {
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
}
