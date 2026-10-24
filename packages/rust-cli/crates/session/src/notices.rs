//! 后台命令的终态通知（docs/specs/rust-cli/query.md §5）。
//!
//! actor 直接持有执行环境的通知流，与命令、主循环的提交请求在同一个 `select!` 里处理，所以通知与
//! 输入队列只有一个 owner。通知进 next 通道（[`crate::lane`]）：运行中在下一个工具边界并入，空闲或
//! 回合结束时开回合；提交之后才 ack（至少一次：重复的按对话里已有的同文本消息去掉）。

use rust_cli_kernel_api::ids::ShellTaskId;
use rust_cli_kernel_api::machine_notices::{self, MachineNotice};
use rust_cli_kernel_api::session_api::KernelEvent;
use rust_cli_query::task_status;

use crate::actor::Actor;
use crate::lane::Waiting;
use crate::watched::Watched;

/// 通知开启的回合里，user 消息的来源（排队命令的 `task-notification` 模式）。
pub(crate) const NOTICE_SOURCE: &str = "task-notification";

/// 执行环境的通知流：本机与连上的远端机器合成一个流（R3，remote-exec.md §20.7.2），每条带机器名。某台机器不提供
/// 通知或断开时，合成流丢掉那台的流、继续等其余的。
pub(crate) type Notices = Watched<MachineNotice>;

/// 下一条通知。
pub(crate) async fn next_notice(stream: &mut Notices) -> MachineNotice {
    stream.next().await
}

impl Actor {
    pub(crate) async fn on_notice(&mut self, notice: MachineNotice) {
        // 关闭中不收（R1-1）：不 ack，通知留在执行环境里，会话再打开时送达。
        if self.closing.is_some() {
            return;
        }
        // Monitor 的终态（工作线 F83）：在看的 monitor 等事件流也结束了再合成（monitors.rs）。
        if notice.notice.kind == rust_cli_kernel_api::env_shell::BackgroundKind::Monitor
            && self.monitor_notice(&notice).await
        {
            return;
        }
        let work = notice.notice.task.to_string();
        let status = task_status(&notice.notice).to_owned();
        if !self.enqueue_notice(Waiting::Shell(notice)).await {
            return;
        }
        self.log
            .append(KernelEvent::BackgroundWork { work, status });
        if self.running.is_none() {
            self.start_next().await;
        }
    }

    /// ack 发给通知所在的机器（R3）。
    pub(crate) async fn ack(&self, notice: &MachineNotice) {
        if let Err(error) =
            machine_notices::ack(self.deps.machines.as_ref(), notice, &self.meta.id).await
        {
            // 没有 ack 的通知会再次送达，那时按 `enqueue_notice` 去重。
            tracing::warn!(%error, seq = notice.notice.seq, machine = %notice.machine, "background notice was not acknowledged");
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
