//! 会话内定时任务的触发（docs/specs/rust-cli/tools-more.md §10.4）。
//!
//! 任务表的 owner 是 cron 能力（[`CronSignals`]）；排队输入的 owner 是 actor。actor 在同一个 `select!`
//! 里收唤醒，只在空闲时取走到期任务（任务只在 REPL 空闲时触发，以优先级 `later` 排队）。
//! 忙时收到的唤醒直接忽略：回合结束时 `start_next` 会再取。

use rust_cli_kernel_api::cron::CronFire;

use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::session_api::Ack;

use crate::actor::{Actor, accepted, rejected};

/// 定时任务开启的回合里，user 消息的来源。
pub(crate) const CRON_SOURCE: &str = "scheduled-task";

/// 唤醒流（流结束后不再等它）。
pub(crate) type CronStream = crate::watched::Watched<()>;

impl Actor {
    /// 用户中断（esc / ctrl+c）：先取消本会话全部 loop 唤醒（任务表的 owner 是 cron 能力，
    /// tools-more.md §18.1），再中止回合或后台工作；空闲时取消了唤醒也算接受。
    pub(crate) async fn interrupt(&mut self) -> Ack {
        let cancelled = self.deps.cron.cancel_loops(&self.meta.id).await;
        match &self.running {
            Some(running) => {
                running.abort.abort(AbortReason::User);
                // 回合里正在等用户回答的交互一并取消，等待中的工具立即得到错误并结束；后台子代理的询问不属于这个
                // 回合，留着（agents.md §4.1.1）。
                self.cancel_turn_asks().await;
                accepted()
            }
            None => match self.job.as_ref().and_then(|job| job.cancel.as_ref()) {
                Some(cancel) => {
                    cancel.cancel();
                    accepted()
                }
                None if cancelled > 0 => accepted(),
                None => rejected("no turn is running"),
            },
        }
    }

    /// 进程要退出（`Command::Shutdown`，修复线 F38，spec §3.5）：以 `Shutdown` 中止在跑的回合（之后这个回合不再记录），
    /// 取消待回答的交互与可取消的后台工作；此后不再开回合。退出本身不需要中止 query；这里中止只为
    /// 经取消路径回收工具的进程树。
    pub(crate) async fn shutdown(&mut self) -> Ack {
        self.shutting_down = true;
        if let Some(running) = &self.running {
            running.abort.abort(AbortReason::Shutdown);
        } else if let Some(cancel) = self.job.as_ref().and_then(|job| job.cancel.as_ref()) {
            cancel.cancel();
        }
        // 全部待回答的交互：含回合之外的会话级询问（agents.md §4.1.1）。
        self.cancel_pending().await;
        accepted()
    }

    pub(crate) async fn on_cron_wakeup(&mut self) {
        if !self.busy() {
            self.start_next().await;
        }
    }

    /// 下一个要触发的任务：先排在 actor 里的，没有时向任务表取走此刻到期的（只在空闲时调用）。
    pub(crate) async fn next_cron(&mut self) -> Option<CronFire> {
        if self.cron_fires.is_empty() && self.job.is_none() {
            let due = self.deps.cron.take_due(&self.meta.id).await;
            self.cron_fires.extend(due);
        }
        self.cron_fires.pop_front()
    }
}

#[cfg(test)]
#[path = "cron_tests.rs"]
mod tests;
