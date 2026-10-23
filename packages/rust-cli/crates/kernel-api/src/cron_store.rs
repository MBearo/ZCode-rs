//! 持久定时任务的存储端口（docs/specs/rust-cli/tools-more.md §16）。
//!
//! 唯一事实源是项目根下的 `.zcode/scheduled_tasks.json`；cron 能力（tools 的 `CronBoard`）持有它在本进程
//! 的投影。文件与锁都在执行环境里：本地实现是 env-local 的 `LocalCronStore`，第二阶段在 App 下整体换成
//! App 的实现（`automation/*`），每种环境只装一个。读改写是一次端口操作，在执行侧原子完成；创建者进程的
//! PID 与启动时间只在执行侧出现，读出的每项只给出判定结果（[`CronCreator`]）。

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::env::EnvError;
use crate::ids::SessionId;

/// 创建任务的进程现在的状态（按 `createdByPid` / `createdByProcStart` 判断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronCreator {
    /// 就是本进程（PID 相同，启动时间相同；文件里没有启动时间时按创建时间不早于本进程启动）。
    ThisProcess,
    /// 另一个还在运行的进程（启动时间相同或取不到）。
    Running,
    /// 已退出、PID 被复用（启动时间不同），或文件里没有 PID：任务没有活着的创建者。
    Gone,
}

/// 文件里的一个持久任务（执行侧已校验字段类型）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCronTask {
    pub id: String,
    pub cron: String,
    pub prompt: String,
    /// 创建时间（UTC 毫秒）。
    pub created_at: u64,
    /// 循环任务最近一次触发（UTC 毫秒），调度器回写。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<u64>,
    #[serde(default)]
    pub recurring: bool,
    /// 不受 7 天到期限制（assistant 模式的内置任务；工具不会写它）。
    #[serde(default)]
    pub permanent: bool,
    /// `createdBySessionId`：建它的顶层会话；旧格式与手写的任务没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<SessionId>,
    pub creator: CronCreator,
    /// `createdInProject` 与本项目根相同（执行侧按平台的大小写规则比较）；没有这个字段时为假。
    #[serde(default)]
    pub in_project: bool,
}

/// 新建的持久任务；执行侧补上创建者进程与项目根。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewCronTask {
    pub id: String,
    pub cron: String,
    pub prompt: String,
    pub created_at: u64,
    pub recurring: bool,
    pub created_by: SessionId,
}

/// 一次读改写（执行侧读文件、修改、原子替换）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CronTasksChange {
    /// 追加一项（CronCreate `durable: true`）。
    Add { task: NewCronTask },
    /// 删除这些 ID；一个都不在时不写文件。
    Remove { ids: Vec<String> },
    /// 循环任务触发后回写 `lastFiredAt`。
    MarkFired { ids: Vec<String>, at: u64 },
    /// `createdBySessionId` 是这个会话、但 PID 不是本进程的项改成本进程（resume 后的刷新）。
    Claim { session: SessionId },
}

/// 持久任务的存储：任务文件、调度锁与文件变化。
#[async_trait]
pub trait CronStore: Send + Sync {
    /// 文件里的全部任务（文件不存在或不是合法 JSON 时为空，字段类型不对的项跳过）。
    async fn tasks(&self) -> Result<Vec<StoredCronTask>, EnvError>;

    /// 一次读改写，在执行侧原子完成。
    async fn change(&self, change: CronTasksChange) -> Result<(), EnvError>;

    /// 任务文件的变化（本进程或别的进程写入、删除）：每次产出一项，只是提醒，内容要重读。
    fn changes(&self) -> BoxStream<'static, ()>;

    /// 调度锁：拿到或本来就是 `holder` 的返回 true；另一个活着的持有者在时返回 false。存活按 PID 与
    /// 进程启动时间判断，失效的锁删除后再抢一次。
    async fn lead(&self, holder: &SessionId) -> Result<bool, EnvError>;

    /// 释放锁：只删除持有者是 `holder` 的锁。
    async fn release(&self, holder: &SessionId) -> Result<(), EnvError>;
}

#[cfg(test)]
#[path = "cron_store_tests.rs"]
mod tests;
