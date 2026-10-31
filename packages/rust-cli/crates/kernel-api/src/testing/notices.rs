//! 后台通知的执行环境替身：测试推入终态通知，会话订阅、ack；`shell_state.background` 是推入后
//! 还没有 ack 的任务（与 env-local 相同：ack 之前都算后台工作）。

use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use tokio::sync::mpsc;

use super::lock;
use crate::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, OperationContext, Outcome, ReadFile,
    StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use crate::env_shell::{ShellNotice, ShellState};
use crate::ids::{OperationId, SessionId, ShellTaskId};
use crate::local_services::{Fetch, Git};
use crate::machine::{Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell};

pub struct NoticeEnvironment {
    sender: mpsc::UnboundedSender<ShellNotice>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<ShellNotice>>>,
    pushed: Mutex<Vec<ShellNotice>>,
    acked: Mutex<Vec<u64>>,
    /// 还在跑的命令，与它还要经过几次状态查询才结束。
    running: Mutex<Vec<(ShellNotice, usize)>>,
    /// `close_session` 的调用（会话与 `forget`），按顺序。
    closed: Mutex<Vec<(SessionId, bool)>>,
}

impl Default for NoticeEnvironment {
    fn default() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
            pushed: Mutex::default(),
            acked: Mutex::default(),
            running: Mutex::default(),
            closed: Mutex::default(),
        }
    }
}

impl NoticeEnvironment {
    /// 一条后台命令结束：记为未 ack，并送给订阅方。
    pub fn finish(&self, notice: ShellNotice) {
        lock(&self.pushed).push(notice.clone());
        let _ = self.sender.send(notice);
    }

    /// 一条后台命令在跑；第 `checks` 次 `shell_state` 报告它之后立即结束（模拟"刚查完就结束"的时序）。
    pub fn finish_after_checks(&self, notice: ShellNotice, checks: usize) {
        lock(&self.running).push((notice, checks));
    }

    pub fn acked(&self) -> Vec<u64> {
        lock(&self.acked).clone()
    }

    /// 收到的 `close_session`（会话与 `forget`），按顺序。
    pub fn closed(&self) -> Vec<(SessionId, bool)> {
        lock(&self.closed).clone()
    }
}

/// 一条成功结束的后台命令通知。
pub fn shell_notice(seq: u64, task: &str) -> ShellNotice {
    ShellNotice {
        seq,
        operation: OperationId::new(format!("op_{task}")),
        task: ShellTaskId::new(task),
        tool_use: None,
        description: format!("Run {task}"),
        output: EnvPath::new(format!("/tmp/{task}.output")),
        outcome: Outcome::Completed,
        exit_code: Some(0),
        kind: Default::default(),
        output_bytes: 0,
    }
}

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "notice test environment".to_owned(),
    }
}

#[async_trait]
impl EnvInfo for NoticeEnvironment {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(unused())
    }
}

#[async_trait]
impl Files for NoticeEnvironment {
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
impl Commands for NoticeEnvironment {
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

    async fn close_session(&self, session: &SessionId, forget: bool) -> Result<(), EnvError> {
        lock(&self.closed).push((session.clone(), forget));
        Ok(())
    }
}

#[async_trait]
impl Shell for NoticeEnvironment {
    async fn shell_state(
        &self,
        workspace: &WorkspaceRef,
        _session: &SessionId,
    ) -> Result<ShellState, EnvError> {
        let acked = self.acked();
        let mut finished = Vec::new();
        let background = {
            let mut running = lock(&self.running);
            let background = running
                .iter()
                .map(|(notice, _)| notice)
                .chain(lock(&self.pushed).iter())
                .filter(|notice| !acked.contains(&notice.seq))
                .map(|notice| notice.task.clone())
                .collect();
            running.retain_mut(|(notice, checks)| {
                *checks = checks.saturating_sub(1);
                if *checks == 0 {
                    finished.push(notice.clone());
                }
                *checks > 0
            });
            background
        };
        for notice in finished {
            self.finish(notice);
        }
        Ok(ShellState {
            cwd: workspace.root.clone(),
            outputs: None,
            background,
        })
    }

    /// 只支持一个订阅方（会话 actor）。
    fn shell_notices(
        &self,
        _session: &SessionId,
        _after: u64,
    ) -> BoxStream<'static, Result<ShellNotice, EnvError>> {
        match lock(&self.receiver).take() {
            Some(mut receiver) => stream::poll_fn(move |cx| receiver.poll_recv(cx))
                .map(Ok)
                .boxed(),
            None => stream::iter([Err(unused())]).boxed(),
        }
    }

    async fn ack_shell_notice(&self, _session: &SessionId, seq: u64) -> Result<(), EnvError> {
        lock(&self.acked).push(seq);
        Ok(())
    }
}

impl Search for NoticeEnvironment {}
impl Background for NoticeEnvironment {}
impl Checkpoints for NoticeEnvironment {}
impl Pdf for NoticeEnvironment {}
impl Git for NoticeEnvironment {}
impl Fetch for NoticeEnvironment {}
