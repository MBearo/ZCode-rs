//! 执行环境的测试替身。每个替身实现一台机器的 8 个端口与本机的 2 个服务端口（remote-exec.md §15.8.1）；
//! 没有覆盖的方法用端口的缺省（`Unsupported`）。

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};

use crate::env::{
    CommandEvent, EnvError, EnvFacts, FileContent, FileVersion, OperationContext, ReadFile,
    StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use crate::ids::{OperationId, SessionId};
use crate::local_services::{Fetch, Git};
use crate::machine::{Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell};

/// 文本回路用不到执行环境：每个操作都返回 `Unsupported`。
pub struct UnusedEnvironment;

/// 只提供环境事实与规则文件的执行环境：`facts` 返回给定值，`read_file` 从给定的文件表里读，
/// 表里没有的路径返回 `NotFound`；其余操作返回 `Unsupported`。
pub struct FactsEnvironment {
    pub facts: EnvFacts,
    pub files: Vec<(String, String)>,
}

#[async_trait]
impl EnvInfo for FactsEnvironment {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Ok(self.facts.clone())
    }
}

#[async_trait]
impl Files for FactsEnvironment {
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
}

#[async_trait]
impl Commands for FactsEnvironment {
    async fn start_command(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: StartCommand,
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
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

    async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

impl Search for FactsEnvironment {}
impl Shell for FactsEnvironment {}
impl Background for FactsEnvironment {}
impl Checkpoints for FactsEnvironment {}
impl Pdf for FactsEnvironment {}
impl Git for FactsEnvironment {}
impl Fetch for FactsEnvironment {}

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "test environment".to_owned(),
    }
}

#[async_trait]
impl EnvInfo for UnusedEnvironment {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(unused())
    }
}

#[async_trait]
impl Files for UnusedEnvironment {
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
}

#[async_trait]
impl Commands for UnusedEnvironment {
    async fn start_command(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: StartCommand,
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
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

    async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

impl Search for UnusedEnvironment {}
impl Shell for UnusedEnvironment {}
impl Background for UnusedEnvironment {}
impl Checkpoints for UnusedEnvironment {}
impl Pdf for UnusedEnvironment {}
impl Git for UnusedEnvironment {}
impl Fetch for UnusedEnvironment {}
