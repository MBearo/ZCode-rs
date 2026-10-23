//! 会话内定时任务的端口（docs/specs/rust-cli/tools-more.md §10.4）。
//!
//! 两份状态各有一个 owner：任务表在 cron 能力里（CronCreate / CronDelete / CronList 改它），排队输入在
//! 会话 actor。actor 只在空闲时经 [`CronSignals::take_due`] 取走到期的任务，每个任务开一个回合
//! （任务只在 REPL 空闲时触发，触发的 prompt 以优先级 `later` 排队）。

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::ids::SessionId;
use crate::message::Message;

/// 一个到期的任务：它的 prompt 作为 meta user 消息开一个回合。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronFire {
    /// 任务 ID（CronCreate 的结果里给出）。
    pub job: String,
    pub prompt: String,
    /// 任务的种类：决定触发时的界面行与 `/loop` 的折叠（tools-more.md §18.3，L22）。
    #[serde(default)]
    pub kind: CronFireKind,
}

/// 到期任务的种类。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronFireKind {
    /// CronCreate 的会话任务与持久任务（界面行 `Running scheduled task (<时间>)`）。
    #[default]
    Task,
    /// ScheduleWakeup 的 loop 唤醒（`kind: "loop"`）。
    Loop,
    /// 错过的一次性任务的提示（走触发回调，没有界面行）。
    Notice,
}

/// `/loop` 的 noop 折叠（连续 noop 的次数与开始时刻，tools-more.md §18.3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopFold {
    /// 连续没有动作的 tick 数（含刚结束的这一个）。
    pub streak: u32,
    /// 连续开始的时间，与触发行同一种写法（`Oct 18 3:04pm`）。
    pub since: String,
}

#[async_trait]
pub trait CronSignals: Send + Sync {
    /// `session` 的唤醒流：某个任务可能到期时产出一项。只是提醒，到期的任务由 [`take_due`] 取；
    /// 同一个到期时间只提醒一次，actor 忙时忽略即可（回合结束后它自己会取）。
    ///
    /// [`take_due`]: CronSignals::take_due
    fn wakeups(&self, session: &SessionId) -> BoxStream<'static, ()>;

    /// 会话空闲时调用：取走此刻到期的任务，按创建顺序。一次性任务与满期的循环任务随之删除，其余
    /// 循环任务排下一次。
    async fn take_due(&self, session: &SessionId) -> Vec<CronFire>;

    /// 会话关闭（TUI 退出、`/clear`、`/resume` 切走）：停止这个会话的持久任务调度，释放它持有的调度锁
    /// （tools-more.md §16.4）。默认什么都不做。
    async fn session_closed(&self, _session: &SessionId) {}

    /// 用户中断（esc / ctrl+c）：取消这个会话全部 loop 唤醒，清掉在跑的 tick 与 keepalive 计数；
    /// 返回取消的个数（tools-more.md §18.1，L22）。默认什么都不做。
    async fn cancel_loops(&self, _session: &SessionId) -> usize {
        0
    }

    /// 会话从存储打开（resume、fork）：从活动历史复原 CronCreate 建的会话任务（tools-more.md
    /// §18.2，L22）。默认什么都不做。
    async fn resume(&self, _session: &SessionId, _history: &[Message]) {}
}

/// 没有定时任务能力（或 `-p`，任务不触发）：没有唤醒，也没有到期任务。
pub struct NoCron;

#[async_trait]
impl CronSignals for NoCron {
    fn wakeups(&self, _session: &SessionId) -> BoxStream<'static, ()> {
        futures_util::stream::pending().boxed()
    }

    async fn take_due(&self, _session: &SessionId) -> Vec<CronFire> {
        Vec::new()
    }
}
