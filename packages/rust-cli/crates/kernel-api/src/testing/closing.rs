//! 关闭会话时停住的执行环境（docs/specs/rust-cli/session-lifecycle.md §3.3.1，修复线 F33）：会话关闭中途再打开
//! 同一个会话的竞态要确定地发生——测试让 `close_session` 停住，做完要验证的操作再放行，或让它失败。

use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use tokio::sync::watch;

use crate::env::{
    CommandEvent, EnvError, EnvFacts, Environment, FileContent, OperationContext, Outcome,
    ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use crate::ids::{OperationId, SessionId};
use crate::testing::lock;

/// `close_session` 停在闸门上，直到 [`ClosingEnvironment::release`]（成功返回）或 [`ClosingEnvironment::fail`]
/// （返回执行环境的错误）；放行之后的调用不再停。`start_command` 记下 hook 的 stdin（看得出跑了哪个 hook）后返回
/// `Unsupported`（hook 按失败处理）；其余操作同 [`super::UnusedEnvironment`]。
pub struct ClosingEnvironment {
    hold: watch::Sender<Hold>,
    hooks: Mutex<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Hold {
    /// `close_session` 被调用的次数（停着的也算）。
    calls: usize,
    /// None：停住；Some(true)：放行；Some(false)：失败。
    outcome: Option<bool>,
}

impl ClosingEnvironment {
    /// `close_session` 从一开始就停住。
    pub fn held() -> Self {
        Self {
            hold: watch::Sender::new(Hold::default()),
            hooks: Mutex::default(),
        }
    }

    /// 等到 `close_session` 被调用过 `count` 次（停着的也算）。
    pub async fn closing(&self, count: usize) {
        let mut hold = self.hold.subscribe();
        let _ = hold.wait_for(|hold| hold.calls >= count).await;
    }

    /// `close_session` 被调用的次数。
    pub fn close_calls(&self) -> usize {
        self.hold.borrow().calls
    }

    /// 放行：停着的与之后的 `close_session` 成功返回。
    pub fn release(&self) {
        self.hold.send_modify(|hold| hold.outcome = Some(true));
    }

    /// 关闭失败：停着的与之后的 `close_session` 返回执行环境的错误。
    pub fn fail(&self) {
        self.hold.send_modify(|hold| hold.outcome = Some(false));
    }

    /// 收到的 hook 命令的 stdin，按调用顺序（hook 的输入是 JSON，带 `hook_event_name`）。
    pub fn hook_inputs(&self) -> Vec<String> {
        lock(&self.hooks).clone()
    }
}

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "closing test environment".to_owned(),
    }
}

#[async_trait]
impl Environment for ClosingEnvironment {
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
        request: StartCommand,
    ) -> Result<(), EnvError> {
        if let Some(hook) = request.hook {
            lock(&self.hooks).push(hook.stdin);
        }
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
        self.hold.send_modify(|hold| hold.calls += 1);
        let mut hold = self.hold.subscribe();
        let outcome = hold
            .wait_for(|hold| hold.outcome.is_some())
            .await
            .map(|hold| hold.outcome);
        match outcome {
            Ok(Some(true)) => Ok(()),
            _ => Err(EnvError::Io {
                message: "the test environment failed to close the session".to_owned(),
            }),
        }
    }
}
