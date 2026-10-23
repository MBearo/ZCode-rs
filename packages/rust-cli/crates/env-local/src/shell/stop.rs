//! TaskStop 停后台命令（docs/specs/rust-cli/shell-permissions.md §2.6）。
//!
//! 后台命令的登记只在这里的账本（会话的 `SessionShell`）里，是唯一事实源；内核不另记任务表。查与
//! 停都在一把锁里完成：命令自己结束与被停止只按拿到锁的先后决定，不靠超时。

use rust_cli_kernel_api::env::{EnvError, OperationContext};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundStatus, BackgroundStop, BackgroundTask,
};

use super::{Shells, State};

/// 在跑的、已结束但通知还没 ack 的，或不认识（含已被停止、通知已 ack）。
fn lookup(state: &State, request: &BackgroundTask) -> Option<BackgroundInfo> {
    let shell = state.sessions.get(&request.session)?;
    let command = |operation| {
        state
            .commands
            .get(operation)
            .map(|running| running.command.clone())
    };
    if let Some(operation) = shell.background.get(&request.task) {
        return Some(BackgroundInfo {
            command: command(operation)?,
            status: BackgroundStatus::Running,
        });
    }
    let notice = shell
        .notices
        .iter()
        .find(|notice| notice.task == request.task)?;
    Some(BackgroundInfo {
        command: command(&notice.operation)?,
        status: BackgroundStatus::ended(&notice.outcome, notice.exit_code),
    })
}

impl Shells {
    pub(crate) fn background_command(&self, request: &BackgroundTask) -> Option<BackgroundInfo> {
        lookup(&self.shared.state(), request)
    }

    /// 还在跑的后台命令从开始到现在的毫秒数；已结束、已停止或不认识时为 None（tui-panels.md §3.3）。
    pub(crate) fn background_elapsed(&self, request: &BackgroundTask) -> Option<u64> {
        let state = self.shared.state();
        let operation = state
            .sessions
            .get(&request.session)?
            .background
            .get(&request.task)?;
        let running = state.commands.get(operation)?;
        Some(u64::try_from(running.started.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    /// 在跑：移出后台登记、记进 `stopped`（收尾时不记通知）、发出取消；终态照常经回执给出。
    pub(crate) fn stop(
        &self,
        context: &OperationContext,
        request: &BackgroundTask,
    ) -> Result<BackgroundStop, EnvError> {
        let mut state = self.shared.state();
        if let Some((known, result)) = state.stops.get(&context.operation) {
            return if known == request {
                Ok(result.clone())
            } else {
                Err(EnvError::OperationConflict {
                    operation: context.operation.clone(),
                })
            };
        }
        let result = match lookup(&state, request) {
            None => BackgroundStop::NotFound,
            Some(BackgroundInfo {
                command,
                status: BackgroundStatus::Running,
            }) => {
                let shell = state.sessions.entry(request.session.clone()).or_default();
                let operation = shell.background.remove(&request.task);
                shell.stopped.insert(request.task.clone());
                if let Some(running) = operation.and_then(|op| state.commands.get(&op)) {
                    running.cancel.cancel();
                }
                BackgroundStop::Stopped { command }
            }
            Some(BackgroundInfo { status, .. }) => BackgroundStop::NotRunning { status },
        };
        state
            .stops
            .insert(context.operation.clone(), (request.clone(), result.clone()));
        Ok(result)
    }
}

#[cfg(test)]
#[path = "../shell_stop_tests.rs"]
mod tests;
