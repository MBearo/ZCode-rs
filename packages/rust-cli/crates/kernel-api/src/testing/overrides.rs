//! 端口的测试代理（工作线 F134，审查 K6）：端口 trait 的方法都没有缺省体，生产实现漏写一个方法是编译错误；测试替身
//! 改写这里的代理 trait，只覆盖关心的方法，其余全委托（输出端委托给一个内层输出端，会话句柄与工具注册表委托给中性的
//! 缺省）。实现代理 trait 的类型就是对应的端口（下面的 blanket impl）。会话操作的代理在 `session.rs`（`OpsOverride`），
//! 询问的代理在 `interactions.rs`（`AskOverride`，工作线 F137）。

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;

use crate::hooks::{Absorbed, StopVerdict};
use crate::ids::{AgentId, SessionId};
use crate::mcp_state::McpConnections;
use crate::message::{CompactTrigger, Message};
use crate::metering::Metered;
use crate::orchestration::{
    Interactions, QueryEvent, RunSink, SessionOps, ToolRegistry, ToolSnapshot,
};
use crate::session_api::{
    Ack, CommandEnvelope, EndReason, KernelError, RewindOption, SequencedEvent, SessionHandle,
    SessionView,
};
use crate::store::StoreError;
use crate::tool::Tool;

/// 主循环输出端的测试代理：输出端自己的方法缺省转给 [`SinkOverride::sink`]（通常是 `RecordingSink`）。实现它的类型
/// 再实现 [`Interactions`]（例如 `AskOverride`）与 `SessionOps`（例如 `OpsOverride`）就是 [`RunSink`]：一个端口
/// trait 一个代理（工作线 F137：以前这里连询问一起代理，与询问的代理冲突）。
#[async_trait]
pub trait SinkOverride: Send + Sync {
    fn sink(&self) -> &dyn RunSink;

    async fn commit_message(&self, message: Message) -> Result<(), StoreError> {
        self.sink().commit_message(message).await
    }

    async fn emit(&self, event: QueryEvent) -> bool {
        self.sink().emit(event).await
    }

    async fn commit_messages(&self, messages: Vec<Message>) -> Result<(), StoreError> {
        self.sink().commit_messages(messages).await
    }

    async fn commit_metered(
        &self,
        message: Option<Message>,
        metered: Metered,
    ) -> Result<(), StoreError> {
        self.sink().commit_metered(message, metered).await
    }

    async fn compact_started(&self, trigger: CompactTrigger) {
        self.sink().compact_started(trigger).await;
    }

    async fn compact_failed(&self, trigger: CompactTrigger, message: String) {
        self.sink().compact_failed(trigger, message).await;
    }

    async fn take_absorbed(&self) -> Vec<Absorbed> {
        self.sink().take_absorbed().await
    }

    async fn goal_checked(
        &self,
        condition: String,
        verdict: StopVerdict,
    ) -> Result<Vec<Message>, StoreError> {
        self.sink().goal_checked(condition, verdict).await
    }
}

#[async_trait]
impl<T: SinkOverride + Interactions + SessionOps> RunSink for T {
    async fn commit_message(&self, message: Message) -> Result<(), StoreError> {
        SinkOverride::commit_message(self, message).await
    }

    async fn emit(&self, event: QueryEvent) -> bool {
        SinkOverride::emit(self, event).await
    }

    async fn commit_messages(&self, messages: Vec<Message>) -> Result<(), StoreError> {
        SinkOverride::commit_messages(self, messages).await
    }

    async fn commit_metered(
        &self,
        message: Option<Message>,
        metered: Metered,
    ) -> Result<(), StoreError> {
        SinkOverride::commit_metered(self, message, metered).await
    }

    async fn compact_started(&self, trigger: CompactTrigger) {
        SinkOverride::compact_started(self, trigger).await;
    }

    async fn compact_failed(&self, trigger: CompactTrigger, message: String) {
        SinkOverride::compact_failed(self, trigger, message).await;
    }

    async fn take_absorbed(&self) -> Vec<Absorbed> {
        SinkOverride::take_absorbed(self).await
    }

    async fn goal_checked(
        &self,
        condition: String,
        verdict: StopVerdict,
    ) -> Result<Vec<Message>, StoreError> {
        SinkOverride::goal_checked(self, condition, verdict).await
    }
}

fn unsupported(what: &str) -> KernelError {
    KernelError::Unsupported {
        what: what.to_owned(),
    }
}

/// 会话句柄的测试代理：替身写会话的四个基本方法（ID、命令、订阅、视图），其余是中性的缺省（`Unsupported`、空、
/// 不结束的流）。实现它的类型就是 [`SessionHandle`]。
#[async_trait]
pub trait HandleOverride: Send + Sync {
    fn id(&self) -> &SessionId;

    async fn send(&self, command: CommandEnvelope) -> Ack;

    fn subscribe(&self, after: Option<u64>) -> BoxStream<'static, SequencedEvent>;

    async fn view(&self) -> Result<SessionView, KernelError>;

    async fn close(&self, _reason: EndReason) -> Vec<String> {
        Vec::new()
    }

    async fn rewind_options(&self) -> Result<Vec<RewindOption>, KernelError> {
        Err(unsupported("rewind options"))
    }

    async fn goal(&self) -> Result<crate::goal::GoalView, KernelError> {
        Err(unsupported("goal"))
    }

    async fn background_tasks(&self) -> Result<Vec<crate::manage::BackgroundEntry>, KernelError> {
        Err(unsupported("background tasks"))
    }

    async fn subagents(&self) -> Result<Vec<crate::panels::SubagentEntry>, KernelError> {
        Err(unsupported("subagents"))
    }

    fn subagent_changes(&self) -> BoxStream<'static, ()> {
        futures_util::stream::pending().boxed()
    }

    async fn tasks_snapshot(&self) -> Result<crate::panels::TasksSnapshot, KernelError> {
        Ok(crate::panels::TasksSnapshot {
            subagents: HandleOverride::subagents(self).await?,
            background: HandleOverride::background_tasks(self).await?,
        })
    }

    async fn subagent_view(&self, _agent: &AgentId) -> Result<SessionView, KernelError> {
        Err(unsupported("subagent view"))
    }

    async fn shell_detail(
        &self,
        _task: &str,
    ) -> Result<Option<crate::panels::ShellDetail>, KernelError> {
        Err(unsupported("shell detail"))
    }
}

#[async_trait]
impl<T: HandleOverride> SessionHandle for T {
    fn id(&self) -> &SessionId {
        HandleOverride::id(self)
    }

    async fn send(&self, command: CommandEnvelope) -> Ack {
        HandleOverride::send(self, command).await
    }

    fn subscribe(&self, after: Option<u64>) -> BoxStream<'static, SequencedEvent> {
        HandleOverride::subscribe(self, after)
    }

    async fn view(&self) -> Result<SessionView, KernelError> {
        HandleOverride::view(self).await
    }

    async fn close(&self, reason: EndReason) -> Vec<String> {
        HandleOverride::close(self, reason).await
    }

    async fn rewind_options(&self) -> Result<Vec<RewindOption>, KernelError> {
        HandleOverride::rewind_options(self).await
    }

    async fn goal(&self) -> Result<crate::goal::GoalView, KernelError> {
        HandleOverride::goal(self).await
    }

    async fn background_tasks(&self) -> Result<Vec<crate::manage::BackgroundEntry>, KernelError> {
        HandleOverride::background_tasks(self).await
    }

    async fn subagents(&self) -> Result<Vec<crate::panels::SubagentEntry>, KernelError> {
        HandleOverride::subagents(self).await
    }

    fn subagent_changes(&self) -> BoxStream<'static, ()> {
        HandleOverride::subagent_changes(self)
    }

    async fn tasks_snapshot(&self) -> Result<crate::panels::TasksSnapshot, KernelError> {
        HandleOverride::tasks_snapshot(self).await
    }

    async fn subagent_view(&self, agent: &AgentId) -> Result<SessionView, KernelError> {
        HandleOverride::subagent_view(self, agent).await
    }

    async fn shell_detail(
        &self,
        task: &str,
    ) -> Result<Option<crate::panels::ShellDetail>, KernelError> {
        HandleOverride::shell_detail(self, task).await
    }
}

/// 工具注册表的测试代理：替身写工具清单与按名查找，其余是中性的缺省（没有藏起或扣下的工具、按名字排序、快照就是
/// 清单、没有 MCP）。实现它的类型就是 [`ToolRegistry`]。
pub trait RegistryOverride: Send + Sync {
    fn tools(&self) -> Vec<Arc<dyn Tool>>;

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>>;

    fn withheld(&self, _name: &str) -> Option<Arc<dyn Tool>> {
        None
    }

    fn hides(&self, _name: &str) -> bool {
        false
    }

    fn keeps_order(&self) -> bool {
        false
    }

    fn snapshot(&self, _session: &SessionId) -> ToolSnapshot {
        ToolSnapshot {
            tools: self.tools(),
            mcp: None,
        }
    }

    fn mcp(&self) -> Option<Arc<dyn McpConnections>> {
        None
    }
}

impl<T: RegistryOverride> ToolRegistry for T {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        RegistryOverride::tools(self)
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        RegistryOverride::find(self, name)
    }

    fn withheld(&self, name: &str) -> Option<Arc<dyn Tool>> {
        RegistryOverride::withheld(self, name)
    }

    fn hides(&self, name: &str) -> bool {
        RegistryOverride::hides(self, name)
    }

    fn keeps_order(&self) -> bool {
        RegistryOverride::keeps_order(self)
    }

    fn snapshot(&self, session: &SessionId) -> ToolSnapshot {
        RegistryOverride::snapshot(self, session)
    }

    fn mcp(&self) -> Option<Arc<dyn McpConnections>> {
        RegistryOverride::mcp(self)
    }
}
