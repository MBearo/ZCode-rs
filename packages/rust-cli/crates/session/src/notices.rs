//! 后台命令的终态通知（docs/specs/rust-cli/query.md §5）。
//!
//! actor 直接持有执行环境的通知流，与命令、主循环的提交请求在同一个 `select!` 里处理，所以通知与
//! 输入队列只有一个 owner。通知进 next 通道（[`crate::lane`]）：运行中在下一个工具边界并入，空闲或
//! 回合结束时开回合；提交之后才 ack（至少一次：重复的按对话里已有的同文本消息去掉）。

use rust_cli_kernel_api::env::EnvError;
use rust_cli_kernel_api::env_shell::ShellNotice;
use rust_cli_kernel_api::ids::ShellTaskId;
use rust_cli_kernel_api::session_api::KernelEvent;
use rust_cli_query::task_status;

use crate::actor::Actor;
use crate::lane::Waiting;
use crate::watched::Watched;

/// 通知开启的回合里，user 消息的来源（排队命令的 `task-notification` 模式）。
pub(crate) const NOTICE_SOURCE: &str = "task-notification";

/// 执行环境的通知流。
pub(crate) type Notices = Watched<Result<ShellNotice, EnvError>>;

/// 下一条通知；流结束或执行环境不提供通知时，以后不再等它。
pub(crate) async fn next_notice(stream: &mut Notices) -> ShellNotice {
    loop {
        match stream.next().await {
            Ok(notice) => return notice,
            Err(error) => {
                tracing::debug!(%error, "background notices are not available");
                stream.stop();
            }
        }
    }
}

impl Actor {
    pub(crate) async fn on_notice(&mut self, notice: ShellNotice) {
        // 关闭中不收（R1-1）：不 ack，通知留在执行环境里，会话再打开时送达。
        if self.closing.is_some() {
            return;
        }
        // Monitor 的终态（工作线 F83）：在看的 monitor 等事件流也结束了再合成（monitors.rs）。
        if notice.kind == rust_cli_kernel_api::env_shell::BackgroundKind::Monitor
            && self.monitor_notice(&notice).await
        {
            return;
        }
        let work = notice.task.to_string();
        let status = task_status(&notice).to_owned();
        if !self.enqueue_notice(Waiting::Shell(notice)).await {
            return;
        }
        self.log
            .append(KernelEvent::BackgroundWork { work, status });
        if self.running.is_none() {
            self.start_next().await;
        }
    }

    pub(crate) async fn ack(&self, seq: u64) {
        if let Err(error) = self
            .deps
            .machine
            .shell()
            .ack_shell_notice(&self.meta.id, seq)
            .await
        {
            // 没有 ack 的通知会再次送达，那时按 `enqueue_notice` 去重。
            tracing::warn!(%error, seq, "background notice was not acknowledged");
        }
    }

    /// 执行环境里还没有处理完的后台命令；执行环境不支持时为空。
    pub(crate) async fn background(&self) -> Vec<ShellTaskId> {
        self.deps
            .machine
            .shell()
            .shell_state(&self.meta.workspace, &self.meta.id)
            .await
            .map(|state| state.background)
            .unwrap_or_default()
    }
}
