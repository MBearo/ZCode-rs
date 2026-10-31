//! 没连上的机器（remote-exec.md §20.6）：子代理的缺省机器断开时，主循环仍要给工具调用一台机器；它的每个方法都返回
//! “没连上”的 io 错误，不落回本机。

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};

use crate::env::{
    CommandEvent, EnvError, EnvFacts, FileContent, OperationContext, ReadFile, StartCommand,
    WorkspaceRef, WriteFile, WriteReceipt,
};
use crate::ids::{OperationId, SessionId};
use crate::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, MachineEnv, Pdf, Search, Shell,
};
use crate::mcp::NoMcpTransport;

/// “没连上”的文字（工具结果里给模型的就是它）。
pub fn not_connected(name: &str) -> String {
    format!("Machine \"{name}\" is not connected.")
}

struct Unavailable(String);

impl Unavailable {
    fn error(&self) -> EnvError {
        EnvError::Io {
            message: not_connected(&self.0),
        }
    }
}

#[async_trait]
impl EnvInfo for Unavailable {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(self.error())
    }
}

#[async_trait]
impl Files for Unavailable {
    async fn read_file(
        &self,
        _workspace: &WorkspaceRef,
        _request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        Err(self.error())
    }

    async fn write_file(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        Err(self.error())
    }
}

#[async_trait]
impl Commands for Unavailable {
    async fn start_command(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: StartCommand,
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
        Err(self.error())
    }

    fn watch_command(
        &self,
        _operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        stream::iter([Err(self.error())]).boxed()
    }

    async fn cancel(&self, _operation: OperationId) -> Result<(), EnvError> {
        Err(self.error())
    }

    async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Err(self.error())
    }
}

impl Search for Unavailable {}
impl Shell for Unavailable {}
impl Background for Unavailable {}
impl Checkpoints for Unavailable {}
impl Pdf for Unavailable {}

impl MachineEnv {
    /// 叫这个名字、但没连上的机器：每个方法都是 [`not_connected`] 的 io 错误（没有覆盖的方法照旧 `Unsupported`）。
    pub fn unavailable(name: &str) -> Self {
        Self::new(
            Arc::new(Unavailable(name.to_owned())),
            Arc::new(NoMcpTransport),
        )
        .named(name)
    }
}
