//! 子循环的输出端：消息与意图提交到子会话（提交屏障：存储成功后才返回），进度转给父工具调用，
//! 交互转给父会话：前台经父工具调用，后台经父会话的会话级入口（交互界面里，docs/specs/rust-cli/agents.md
//! §4.1.1）；没有入口的后台子代理（`-p`）按“没有人能回答”处理。会话的读与命令（`SessionOps`）：前台子代理的工作目录、
//! 定时任务的 owner 与主模型跟着父会话，模式与工作区是子代理自己的，plan 开关与对话不归子代理。

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::ids::{AgentId, MessageId, SessionId};
use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock, Message};
use rust_cli_kernel_api::orchestration::{
    Absorbed, Answer, InteractionAnswer, InteractionRequest, Interactions, OrchestrationError,
    PermissionAnswer, QueryEvent, RunSink, RunView, SessionOps, Steered,
};
use rust_cli_kernel_api::run_ledger::RunLedger;
use rust_cli_kernel_api::store::{Fact, SessionStore, StoreError};
use rust_cli_kernel_api::tool::ToolProgress;
use serde_json::json;
use tokio::sync::mpsc;

use crate::doing::Doing;
use crate::live::Live;

/// 父会话的会话级询问入口（`Interactions::lasting`）：后台子代理在父回合结束之后经它询问。子代理的条目要能复制、
/// 打印，所以包一层。
#[derive(Clone)]
pub(crate) struct Asks(pub(crate) Arc<dyn Interactions>);

impl std::fmt::Debug for Asks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Asks")
    }
}

/// 前台子代理的父工具调用：进度、询问与会话操作都交给它（父工具调用的 `ToolUseContext` 的两个端口）。
pub(crate) struct Parent {
    /// 父工具调用所在的会话（`owner_session` 问父会话时用）。
    pub(crate) session: SessionId,
    pub(crate) asks: Arc<dyn Interactions>,
    pub(crate) ops: Arc<dyn SessionOps>,
    pub(crate) progress: mpsc::Sender<ToolProgress>,
}

/// 一条 assistant 消息里的 tool_use 块数（通知与结果的 `tool_uses`，agents.md §2）。
pub(crate) fn tool_use_blocks(content: &[ContentBlock]) -> usize {
    content
        .iter()
        .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
        .count()
}

#[derive(Default)]
struct Record {
    last: Option<AssistantMessage>,
    tool_uses: usize,
    /// 提交核心（kernel-api `RunLedger`，与会话 actor 同一份，工作线 F130）：子会话的当前轮（随每个批次交给存储，
    /// node-store.md §4，修复线 F6）、已结束而结果还没提交的工具带出的 checkpoint 与子代理关联。锁不跨 await。
    ledger: RunLedger,
}

pub(crate) struct ChildSink {
    pub(crate) store: Arc<dyn SessionStore>,
    pub(crate) ids: Arc<dyn Ids>,
    pub(crate) live: Arc<Live>,
    pub(crate) agent: AgentId,
    pub(crate) session: SessionId,
    /// 这次运行开头的 user 消息：子会话里写文件的 checkpoint 挂在它上面（Node `messageId`）。
    prompt: MessageId,
    /// None：后台子代理。
    pub(crate) parent: Option<Parent>,
    /// 后台子代理询问用的父会话入口；没有时（`-p`）不询问。
    asks: Option<Asks>,
    /// 写进询问的子代理名字（agent 类型）。
    name: String,
    record: Mutex<Record>,
    /// 子代理里的 Monitor 交给 agents（agents.md §5.3，工作线 F83）；没有时（测试）不支持。
    core: Option<Arc<crate::Core>>,
}

impl ChildSink {
    pub(crate) fn new(
        store: Arc<dyn SessionStore>,
        ids: Arc<dyn Ids>,
        live: Arc<Live>,
        agent: AgentId,
        session: SessionId,
        prompt: MessageId,
        parent: Option<Parent>,
    ) -> Self {
        Self {
            store,
            ids,
            live,
            agent,
            session,
            prompt,
            parent,
            asks: None,
            name: String::new(),
            record: Mutex::default(),
            core: None,
        }
    }

    /// 子代理里的 Monitor 经 agents 起转发任务（`spawn::drive` 给出）。
    pub(crate) fn with_core(self, core: Arc<crate::Core>) -> Self {
        Self {
            core: Some(core),
            ..self
        }
    }

    /// 这次运行开头时子会话的提交核心（`spawn::create` / `resume` 提交输入时用的那一份，带着当前轮）。
    pub(crate) fn with_ledger(self, ledger: RunLedger) -> Self {
        Self {
            record: Mutex::new(Record {
                ledger,
                ..Record::default()
            }),
            ..self
        }
    }

    /// 写进询问的名字与后台询问的入口（`spawn::drive` 按子代理的条目给出）。
    pub(crate) fn asking(self, name: &str, asks: Option<Asks>) -> Self {
        Self {
            name: name.to_owned(),
            asks,
            ..self
        }
    }

    /// 询问要交给谁：前台是父工具调用，后台是父会话的入口；都没有时为 None。
    fn asker(&self) -> Option<&Arc<dyn Interactions>> {
        match (&self.parent, &self.asks) {
            (Some(parent), _) => Some(&parent.asks),
            (None, Some(asks)) => Some(&asks.0),
            (None, None) => None,
        }
    }

    fn record(&self) -> std::sync::MutexGuard<'_, Record> {
        self.record.lock_unpoisoned()
    }

    /// 本次运行最后一条 assistant 消息与 tool_use 块数。
    pub(crate) fn outcome(&self) -> (Option<AssistantMessage>, usize) {
        let record = self.record();
        (record.last.clone(), record.tool_uses)
    }

    /// 提交到子会话：当前轮随批次给出（修复线 F6，审查 R5-3），提交成功后才记下。子循环按顺序提交，锁只在算批次与
    /// 记下轮次时拿，不跨 await。
    pub(crate) async fn commit(&self, facts: Vec<Fact>) -> Result<(), StoreError> {
        let batch = self.record().ledger.batch(None, facts, self.ids.as_ref());
        let turn = batch.turn.clone();
        self.store.commit(&self.session, batch).await?;
        self.record().ledger.committed(turn);
        Ok(())
    }
}

/// 经父会话询问的这段时间：开始时记一次等待，结束（含被取消、future 被丢弃）时复原。
struct Waiting<'a> {
    live: &'a Live,
    agent: &'a AgentId,
}

impl<'a> Waiting<'a> {
    fn start(live: &'a Live, agent: &'a AgentId) -> Self {
        live.waiting(agent, true);
        Self { live, agent }
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.live.waiting(self.agent, false);
    }
}

#[async_trait]
impl Interactions for ChildSink {
    async fn ask(
        &self,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        let Some(asker) = self.asker() else {
            // 没有入口的后台子代理（`-p`）的交互没有人回答：权限按“不能询问”（`NoPrompts`，管线给拒绝的文字，
            // agents.md §4.1.1），提问按不回答，计划审批按继续计划。
            return Ok(InteractionAnswer {
                interaction: self.ids.interaction(),
                answer: Answer::nobody(&request, PermissionAnswer::NoPrompts),
            });
        };
        // 修复线 F67：以前后台子代理一律按没人回答处理，交互界面里也不询问；现在经父会话的会话级入口询问，
        // 界面在标题后标出发起询问的子代理（最里层写入，外层转发时不改）。
        let request = match request {
            InteractionRequest::Permission {
                tool_use,
                tool,
                input,
                message,
                suggestions,
                rule_seed,
                asked_by: None,
                machine,
            } => InteractionRequest::Permission {
                machine,
                tool_use,
                tool,
                input,
                message,
                suggestions,
                rule_seed,
                asked_by: Some(self.name.clone()),
            },
            other => other,
        };
        // 等用户回答期间列表显示 ` · waiting`（tui-panels.md §1.1）；被取消时 guard 复原。
        let _waiting = Waiting::start(&self.live, &self.agent);
        asker.ask(request).await
    }

    /// 询问转给父会话，撤回也转过去（没有入口的后台子代理的询问当场按没人回答处理，没有可撤回的）。
    async fn cancel_ask(&self, tool_use: &rust_cli_kernel_api::ids::ToolUseId) {
        if let Some(asker) = self.asker() {
            asker.cancel_ask(tool_use).await;
        }
    }

    /// 子代理里再派生的后台子代理也经最外层会话的入口询问：前台转给父工具调用，后台就是自己的入口。
    fn lasting(&self) -> Option<Arc<dyn Interactions>> {
        match &self.parent {
            Some(parent) => parent.asks.lasting(),
            None => self.asks.as_ref().map(|asks| asks.0.clone()),
        }
    }
}

/// 子代理的会话操作：前台跟着父工具调用的会话（工作目录、定时任务的 owner、主模型），其余是子代理自己的。
#[async_trait]
impl SessionOps for ChildSink {
    /// 子代理自己的模式与工作区（派生时定下，一次运行里不变）；主模型是父会话现在的（前台；子代理的旁路请求跟随它，
    /// model.md §6.1），后台没有；子代理没有目标与计划模式的会话标志。
    async fn view(&self) -> Option<RunView> {
        let agent = self.live.get(&self.agent)?;
        let selection = match &self.parent {
            Some(parent) => parent.ops.view().await.and_then(|view| view.selection),
            None => None,
        };
        Some(RunView {
            selection,
            ..RunView::new(agent.mode, agent.workspace)
        })
    }

    /// 前台子代理的会话级状态（定时任务、worktree）记在父会话名下（docs/specs/rust-cli/tools-more.md
    /// §10.2）；后台子代理没有父调用，是它自己。
    async fn owner_session(&self, session: &SessionId) -> SessionId {
        match &self.parent {
            Some(parent) => parent.ops.owner_session(&parent.session).await,
            None => session.clone(),
        }
    }

    /// 对话的 owner 是会话 actor；子代理里不能 fork（agents.md §8.1.2）。
    async fn conversation(&self) -> Option<Vec<Message>> {
        None
    }

    /// 子代理的模式不归会话 actor 管（ExitPlanMode 在子代理里只回报批准）。
    async fn set_plan(&self, _enabled: bool) -> Result<(), OrchestrationError> {
        Err(OrchestrationError::Unsupported {
            what: "changing plan mode".to_owned(),
        })
    }

    /// 前台子代理改的是父会话的工作目录（工作目录是进程级的，tools-more.md §11.4）。
    async fn set_workspace(
        &self,
        root: rust_cli_kernel_api::env::EnvPath,
    ) -> Result<(), OrchestrationError> {
        match &self.parent {
            Some(parent) => parent.ops.set_workspace(root).await,
            None => Err(OrchestrationError::Unsupported {
                what: "changing the working directory from a background agent".to_owned(),
            }),
        }
    }

    /// 子代理里的 Monitor：状态与送达在 agents（agents.md §5.3，工作线 F83）。
    async fn arm_monitor(
        &self,
        monitor: rust_cli_kernel_api::monitor::MonitorArmed,
        events: rust_cli_kernel_api::machine::CommandEvents,
    ) -> Result<(), OrchestrationError> {
        match &self.core {
            Some(core) => crate::monitors::arm(core, &self.agent, monitor, events),
            None => Err(OrchestrationError::Unsupported {
                what: "monitors".to_owned(),
            }),
        }
    }
}

impl ChildSink {
    /// 一条消息、它的 checkpoint 与计量在一个事务里（`usage` 排在最后，usage.md §2.5）。`speed` 是这条回复所属的完成的
    /// 子代理请求的输出速度（F112），提交成功后与版本一起记进 `Live`。
    async fn commit_with(
        &self,
        message: Message,
        usage: Vec<Fact>,
        speed: Option<rust_cli_kernel_api::metering::OutputSpeed>,
    ) -> Result<(), StoreError> {
        // 工具结果与它的 checkpoint、子代理关联在一个事务里提交（session-lifecycle.md §4.2）。子代理的提交不经会话
        // actor，规则是 kernel-api `RunLedger` 的同一份。缺陷修复（D5）：以前这里只配对 checkpoint、不记子代理关联，
        // 子代理再派生的子代理（孙会话）在子会话里没有 `SubagentLinked`，App 的子代理面板找不到它。
        let settled = self.record().ledger.settle(
            message.clone(),
            Some(&self.prompt),
            usage,
            self.ids.as_ref(),
        );
        self.commit(settled.facts).await?;
        // 提交之后才加版本：只读查看看到新版本时去取，存储里一定已有这条消息（tui-panels.md §1.4）。
        self.live.committed(&self.agent, speed);
        if let Message::Assistant(assistant) = message {
            let mut record = self.record();
            record.tool_uses += tool_use_blocks(&assistant.content);
            record.last = Some(assistant);
        }
        Ok(())
    }
}

#[async_trait]
impl RunSink for ChildSink {
    async fn commit_message(&self, message: Message) -> Result<(), StoreError> {
        self.commit_with(message, Vec::new(), None).await
    }

    /// 子代理的请求与工具调用写在子会话（Node `query_source = subagent`，usage.md §2.1）。完成的子代理请求另把输出速度
    /// 记进 `Live`（F112，查看时 footer 的 TPS）；失败、中断的请求与子循环里的压缩、旁路请求不记。
    async fn commit_metered(
        &self,
        message: Option<Message>,
        metered: rust_cli_kernel_api::metering::Metered,
    ) -> Result<(), StoreError> {
        let speed = match &metered {
            rust_cli_kernel_api::metering::Metered::Model(call) => matches!(
                call.source,
                rust_cli_kernel_api::model::QuerySource::Subagent(_)
            )
            .then(|| rust_cli_kernel_api::metering::OutputSpeed::of(call))
            .flatten(),
            rust_cli_kernel_api::metering::Metered::Tool(_) => None,
        };
        let fact = Fact::from(metered);
        match message {
            Some(message) => self.commit_with(message, vec![fact], speed).await,
            None => self.commit(vec![fact]).await,
        }
    }

    async fn emit(&self, event: QueryEvent) -> bool {
        // `ToolFinished` 先于工具结果的提交到达（同一个任务里按顺序调用）：记下 checkpoint 与子代理关联。
        if let QueryEvent::ToolFinished { tool_use, output } = &event {
            self.record().ledger.tool_finished(tool_use, output);
        }
        // 列表行的当前活动（前台与后台都记，L35，stall-visibility.md §4.3）。
        if let Some(doing) = Doing::after(&event) {
            self.live.doing(&self.agent, doing);
        }
        // 进度只给 UI：通道满了就丢，不拖慢子循环。
        if let Some(parent) = &self.parent {
            let data = json!({ "agentId": self.agent, "event": event });
            let _ = parent.progress.try_send(ToolProgress { data });
        }
        true
    }

    /// 一组消息逐条提交到子会话（子代理里的压缩：每条一个事务，同以前的缺省）。
    async fn commit_messages(&self, messages: Vec<Message>) -> Result<(), StoreError> {
        for message in messages {
            self.commit_message(message).await?;
        }
        Ok(())
    }

    /// 子代理里的压缩不显示进度。
    async fn compact_started(&self, _trigger: rust_cli_kernel_api::message::CompactTrigger) {}

    async fn compact_failed(
        &self,
        _trigger: rust_cli_kernel_api::message::CompactTrigger,
        _message: String,
    ) {
    }

    /// 邮箱里的 SendMessage（插话，没有 UserPromptSubmit）与它派生的后台子代理的通知（agents.md §5.1：全文，在工具
    /// 边界作为 `task-notification` 并入），先插话后通知。
    async fn take_absorbed(&self) -> Vec<Absorbed> {
        let steered = self.live.take_mailbox(&self.agent);
        let notices = self
            .live
            .take_notices(&self.agent)
            .into_iter()
            .map(|notice| rust_cli_prompts::agents::notification_wrap(&notice.envelope))
            .collect();
        Absorbed::ordered(steered.into_iter().map(Steered::plain).collect(), notices)
    }

    /// 子代理没有目标（goal.md §4）：不提交。
    async fn goal_checked(
        &self,
        _condition: String,
        _verdict: rust_cli_kernel_api::hooks::StopVerdict,
    ) -> Result<Vec<Message>, StoreError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
#[path = "child_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "child_owner_tests.rs"]
mod owner_tests;

#[cfg(test)]
#[path = "child_ask_tests.rs"]
mod ask_tests;
