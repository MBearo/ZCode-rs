//! TaskStop 停后台命令（docs/specs/rust-cli/shell-permissions.md §2.6）。
//!
//! 后台命令的登记只在这里的账本（会话的 `SessionShell`）里，是唯一事实源；内核不另记任务表。查与
//! 停都在一把锁里完成：命令自己结束与被停止只按拿到锁的先后决定，不靠超时。

use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundStatus, BackgroundStop, BackgroundTask,
};

use super::{Shells, State};

/// 在跑的、已结束但通知还没 ack 的，或不认识（含已被停止、通知已 ack）。种类与说明是启动时的：在跑的取账本里的命令，
/// 已结束的取通知（两处都是启动时记下的同一份，修复线 F117）；已结束的命令原文也跟通知一起记着（登记已删，F119）。
fn lookup(state: &State, request: &BackgroundTask) -> Option<BackgroundInfo> {
    let shell = state.sessions.get(&request.session)?;
    if let Some(operation) = shell.background.get(&request.task) {
        let running = state.commands.get(operation)?;
        return Some(BackgroundInfo {
            command: running.command.clone(),
            status: BackgroundStatus::Running,
            kind: running.kind,
            description: running.description.clone(),
        });
    }
    let pending = shell
        .notices
        .iter()
        .find(|pending| pending.notice.task == request.task)?;
    let notice = &pending.notice;
    Some(BackgroundInfo {
        command: pending.command.clone(),
        status: BackgroundStatus::ended(&notice.outcome, notice.exit_code),
        kind: notice.kind,
        description: notice.description.clone(),
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
    pub(crate) fn stop(&self, request: &BackgroundTask) -> BackgroundStop {
        let mut state = self.shared.state();
        match lookup(&state, request) {
            None => BackgroundStop::NotFound,
            Some(BackgroundInfo {
                command,
                status: BackgroundStatus::Running,
                ..
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
        }
    }

    /// 这个会话在跑、能转的前台命令都转到后台（修复线 L-A，tui-input.md §11）：记下原因，监督任务看到后转、看输出的
    /// 一方说明原因。已经在后台的、已结束的、不能转的（hook 命令）不动。同一把锁里判断，与命令自己结束只按先后。
    pub(crate) fn background_foreground(
        &self,
        session: &rust_cli_kernel_api::ids::SessionId,
        reason: rust_cli_kernel_api::env::BackgroundReason,
    ) -> Vec<rust_cli_kernel_api::ids::ShellTaskId> {
        let state = self.shared.state();
        let background = state.sessions.get(session).map(|shell| &shell.background);
        state
            .commands
            .values()
            .filter(|running| {
                running.session == *session
                    && running.movable
                    && !*running.backgrounded.borrow()
                    && running.moved_by.borrow().is_none()
                    && !background.is_some_and(|tasks| tasks.contains_key(&running.task))
            })
            .map(|running| {
                running.moved_by.send_replace(Some(reason));
                running.task.clone()
            })
            .collect()
    }
}
