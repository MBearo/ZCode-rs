//! 子代理与任务清单的端口（docs/specs/rust-cli/agents.md §8）。
//!
//! - [`SubagentQueries`]：runtime 给 agents 的子循环工厂。子代理的工具集与上下文按 agent 定义替换，
//!   模型、执行管线、压缩、hooks 等其余能力与主循环相同。
//! - [`AgentSignals`]：agents → 会话 actor 的信号（后台子代理的终态通知、任务清单变化）。会话 actor
//!   是输入与通知的唯一 owner（docs/specs/rust-cli/query.md §5）：通知提交为消息之后才 ack。
//! - [`TaskListStore`]：任务清单（Task v2）的存储。文件是唯一事实源；本地实现另把清单投影到 Node 的
//!   `todo` 表，只供显示。

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{AgentId, SessionId, ToolUseId};
use crate::orchestration::{ContextBuilder, QueryRunner, ToolRegistry};
use crate::store::{Fact, StoreError};
use crate::tool::ToolOutput;

/// Agent 工具的 `ToolOutput.display` 里放子代理信息的键（`{agentId, childSessionId, agentType, …}`）。
pub const DISPLAY_KEY: &str = "agent";

/// Agent 工具的结果关联到子会话的事实（[`Fact::SubagentLinked`]）：输出的 display 带 agentId、子会话与类型时有。
/// 提交方（会话 actor）在工具结束时取出，与这次调用的工具结果同批提交。
pub fn subagent_link(tool_use: &ToolUseId, output: &ToolOutput) -> Option<Fact> {
    let display = &output.display[DISPLAY_KEY];
    let text = |key: &str| display[key].as_str().filter(|value| !value.is_empty());
    Some(Fact::SubagentLinked {
        tool_use: tool_use.clone(),
        agent: AgentId::new(text("agentId")?),
        child: SessionId::new(text("childSessionId")?),
        agent_type: text("agentType")?.to_owned(),
    })
}

/// 子循环工厂：用父会话的工具上下文选项改写出子代理的选项。
pub trait SubagentQueries: Send + Sync {
    /// 用 `tools` 与 `context` 替换主循环的工具注册表与上下文组装，得到子代理的 [`QueryRunner`]。
    fn runner(
        &self,
        tools: Arc<dyn ToolRegistry>,
        context: Arc<dyn ContextBuilder>,
    ) -> Arc<dyn QueryRunner>;
}

/// 后台子代理的终态通知（排队优先级 `later`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentNotice {
    /// 在接收会话内单调递增；ack 用它。
    pub seq: u64,
    pub agent: AgentId,
    /// `<status>` 的值：`completed`、`failed` 或 `killed`（会话的 `BackgroundWork` 事件也用它）。
    pub status: String,
    /// 完整的通知文本（含 `<system-reminder>` 包装），作为 meta user 消息提交。
    pub text: String,
}

/// 发给某个会话的信号。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentSignal {
    Notice(AgentNotice),
    /// 任务清单变了；`tasks` 是变化后的完整清单（[`TaskList::tasks`] 的 JSON）。
    TasksChanged {
        tasks: Value,
    },
}

#[async_trait]
pub trait AgentSignals: Send + Sync {
    /// `session` 的信号流：先送达还没有 ack 的通知，再等新的信号。
    fn signals(&self, session: &SessionId) -> BoxStream<'static, AgentSignal>;

    /// `seq` 这条通知已作为消息提交（幂等）。没有 ack 的通知在重新订阅时再次送达。
    async fn ack(&self, session: &SessionId, seq: u64);

    /// 还在跑、或已结束但通知还没有 ack 的后台子代理（`-p` 等它们都处理完才收尾）。
    async fn background(&self, session: &SessionId) -> Vec<AgentId>;

    /// 顶层会话里还在跑的后台子代理的描述与状态（`/tasks`，docs/specs/rust-cli/tui-display.md §3.1）。
    async fn background_entries(
        &self,
        _session: &SessionId,
    ) -> Vec<crate::manage::BackgroundEntry> {
        Vec::new()
    }

    /// 用户停止一个后台任务（`Command::StopTask`）：与 TaskStop 工具同一条路径，按主线程的可见范围找子代理
    /// 与后台命令。成功时是给用户看的结果，失败时是原因。
    async fn stop_task(
        &self,
        _session: &SessionId,
        _task: &str,
        _operation: crate::env::OperationContext,
    ) -> Result<String, String> {
        Err("stopping background tasks is not supported".to_owned())
    }

    /// 顶层会话 `session` 里本进程启动过的子代理（含嵌套的），按启动顺序（tui-panels.md §1.1，L30）。
    async fn subagents(&self, _session: &SessionId) -> Vec<crate::panels::SubagentEntry> {
        Vec::new()
    }

    /// 任何子代理的状态有变化时产生一项（合并多次变化）。
    fn subagent_changes(&self) -> BoxStream<'static, ()> {
        futures_util::stream::pending().boxed()
    }

    /// 顶层会话 `session` 的子代理条目与在跑的后台子代理（`background` 里只有子代理），出自同一次读取（修复线 F34，
    /// tui-panels.md §0）：列表的行与 pill 里的子代理不会一新一旧。缺省实现依次取 [`AgentSignals::subagents`] 与
    /// [`AgentSignals::background_entries`]。
    async fn tasks_snapshot(&self, session: &SessionId) -> crate::panels::TasksSnapshot {
        crate::panels::TasksSnapshot {
            subagents: self.subagents(session).await,
            background: self.background_entries(session).await,
        }
    }

    /// 顶层会话 `session` 里一个子代理的只读快照（子会话的消息，tui-panels.md §1.4）。
    async fn subagent_view(
        &self,
        _session: &SessionId,
        _agent: &AgentId,
    ) -> Result<crate::session_api::SessionView, crate::session_api::KernelError> {
        Err(crate::session_api::KernelError::Unsupported {
            what: "subagent view".to_owned(),
        })
    }
}

/// 没有子代理能力时的空实现：没有信号，也没有后台子代理。
pub struct NoAgentSignals;

#[async_trait]
impl AgentSignals for NoAgentSignals {
    fn signals(&self, _session: &SessionId) -> BoxStream<'static, AgentSignal> {
        futures_util::stream::pending().boxed()
    }

    async fn ack(&self, _session: &SessionId, _seq: u64) {}

    async fn background(&self, _session: &SessionId) -> Vec<AgentId> {
        Vec::new()
    }
}

/// 任务状态（`deleted` 不是状态：删除即从清单里去掉）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }
}

/// 一个任务。字段名是任务 JSON 的固定字段名。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    pub id: String,
    pub subject: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub status: TaskStatus,
    #[serde(default)]
    pub blocks: Vec<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

/// 一份任务清单。`high_water_mark` 是发过的最大 ID，删除后 ID 不回收。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskList {
    #[serde(default)]
    pub high_water_mark: u64,
    #[serde(default)]
    pub tasks: Vec<TaskRecord>,
}

/// 任务清单的存储。清单属于顶层会话（子代理与父会话共用一份）。
#[async_trait]
pub trait TaskListStore: Send + Sync {
    /// 读清单；还没有时返回空清单。
    async fn load(&self, session: &SessionId) -> Result<TaskList, StoreError>;

    /// 整体替换清单（原子）；成功后才返回。
    async fn save(&self, session: &SessionId, list: &TaskList) -> Result<(), StoreError>;
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;
