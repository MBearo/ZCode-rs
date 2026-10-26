//! 会话 actor 给主循环的输出端（[`RunSink`]）：每个请求经通道交给 actor，在 actor 里提交后
//! 才回复，所以提交顺序与 actor 内的状态变更顺序一致（会话状态只有 actor 一个写者）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_kernel_api::message::{CompactTrigger, Message};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::{
    Absorbed, InteractionAnswer, InteractionRequest, Interactions, OrchestrationError, QueryEvent,
    RunSink,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::plan::PlanModeFlags;
use rust_cli_kernel_api::store::StoreError;
use tokio::sync::{mpsc, oneshot};

pub(crate) enum SinkRequest {
    Message {
        message: Message,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    Intent {
        operation: OperationId,
        fingerprint: String,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    Event(QueryEvent),
    Ask {
        request: InteractionRequest,
        reply: oneshot::Sender<Result<InteractionAnswer, OrchestrationError>>,
    },
    /// 工具被取消：撤回它还没回答的询问（与 `Ask` 走同一个通道，所以总排在它的 `Ask` 之后）。
    CancelAsk {
        tool_use: rust_cli_kernel_api::ids::ToolUseId,
    },
    /// 工具边界领取插话与通知（不出队，query.md §4.1、§5）。
    ClaimSteering {
        reply: oneshot::Sender<Option<crate::steering::Claimed>>,
    },
    /// 插话的结局：出队或留下。
    SettleSteering {
        fates: Vec<(rust_cli_kernel_api::ids::CommandId, crate::steering::Fate)>,
        reply: oneshot::Sender<()>,
    },
    /// 一条消息（可以没有）与它的计量一个事务（docs/specs/rust-cli/usage.md §2.5）。
    Metered {
        message: Option<Message>,
        metered: Box<rust_cli_kernel_api::metering::Metered>,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    /// 一组消息一个事务（压缩的边界、摘要与附件）。
    Messages {
        messages: Vec<Message>,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    /// 主循环开始压缩。
    CompactStarted(CompactTrigger),
    /// 工具请求修改权限模式（docs/specs/rust-cli/tools-more.md §2.3）。
    SetMode {
        mode: PermissionMode,
        reply: oneshot::Sender<Result<(), OrchestrationError>>,
    },
    /// 工具请求打开或关掉 plan 开关（EnterPlanMode / ExitPlanMode，修复线 F20）。
    SetPlan {
        enabled: bool,
        reply: oneshot::Sender<Result<(), OrchestrationError>>,
    },
    /// 主循环读判决用的模式（plan 开时是 plan）。
    Mode {
        reply: oneshot::Sender<PermissionMode>,
    },
    /// 主循环组装上下文时读模式与计划模式的会话标志（同一个快照，context.md §13.4）。
    ModeAndPlanFlags {
        reply: oneshot::Sender<(PermissionMode, PlanModeFlags)>,
    },
    /// hook 给出的会话标题（UserPromptSubmit / SessionStart 的 `sessionTitle`）。
    Title(String),
    /// 工具请求修改工作目录（docs/specs/rust-cli/tools-more.md §11.4）。
    SetWorkspace {
        root: rust_cli_kernel_api::env::EnvPath,
        reply: oneshot::Sender<Result<(), OrchestrationError>>,
    },
    /// 主循环读当前的工作目录。
    Workspace {
        reply: oneshot::Sender<rust_cli_kernel_api::env::WorkspaceRef>,
    },
    /// 工具与 hook 读会话当前的主模型（小模型槽位的输入，docs/specs/rust-cli/model.md §6.1）。
    Selection {
        reply: oneshot::Sender<ModelSelection>,
    },
    /// fork 子代理读已提交的对话（修复线 L-A，agents.md §8.1.2）。
    Conversation {
        reply: oneshot::Sender<Vec<Message>>,
    },
    /// Monitor 工具起好的 monitor 交给 actor（工作线 F83，tools-more.md §19.6）。
    ArmMonitor {
        monitor: rust_cli_kernel_api::monitor::MonitorArmed,
        reply: oneshot::Sender<Result<(), OrchestrationError>>,
    },
    /// 主循环读会话的目标条件（docs/specs/rust-cli/goal.md §4）。
    Goal {
        reply: oneshot::Sender<Option<String>>,
    },
    /// 主循环报告目标的判定：actor 提交反馈与 `goal_status`，回复已提交的消息（goal.md §3）。
    GoalChecked {
        condition: String,
        verdict: rust_cli_kernel_api::hooks::StopVerdict,
        reply: oneshot::Sender<Result<Vec<Message>, StoreError>>,
    },
}

pub(crate) struct ActorSink {
    pub(crate) sender: mpsc::Sender<SinkRequest>,
    /// 会话的请求通道（弱引用）：会话级的询问入口经它（`Interactions::lasting`），不让 actor 因此不退出。
    pub(crate) asks: mpsc::WeakSender<crate::actor::Request>,
    /// 会话自己的 ID 与会话关闭之后的询问入口（agents.md §4.1.2，工作线 F72）：会话级的询问入口带上它们。
    pub(crate) session: rust_cli_kernel_api::ids::SessionId,
    pub(crate) detached: Option<Arc<crate::detached::DetachedAsks>>,
}

fn closed() -> StoreError {
    StoreError::Storage {
        message: "the session is closed".to_owned(),
    }
}

impl ActorSink {
    /// 改标题（不等待提交结果：失败只记日志，不影响回合）。
    pub(crate) async fn title(&self, title: String) {
        let _ = self.sender.send(SinkRequest::Title(title)).await;
    }

    async fn call<T>(
        &self,
        request: impl FnOnce(oneshot::Sender<Result<T, StoreError>>) -> SinkRequest,
    ) -> Result<T, StoreError> {
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(request(reply))
            .await
            .map_err(|_| closed())?;
        answer.await.map_err(|_| closed())?
    }
}

#[async_trait]
impl Interactions for ActorSink {
    async fn ask(
        &self,
        _session: &SessionId,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        let gone = || OrchestrationError::Failed {
            message: "the interaction ended without an answer".to_owned(),
        };
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::Ask { request, reply })
            .await
            .map_err(|_| gone())?;
        answer.await.map_err(|_| gone())?
    }

    async fn cancel_ask(
        &self,
        _session: &SessionId,
        tool_use: &rust_cli_kernel_api::ids::ToolUseId,
    ) {
        let request = SinkRequest::CancelAsk {
            tool_use: tool_use.clone(),
        };
        // 回合已经结束（通道关了）时，回合收尾已经取消了全部待回答的交互。
        let _ = self.sender.send(request).await;
    }

    async fn set_mode(
        &self,
        _session: &SessionId,
        mode: PermissionMode,
    ) -> Result<(), OrchestrationError> {
        let gone = || OrchestrationError::Failed {
            message: "the session is closed".to_owned(),
        };
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::SetMode { mode, reply })
            .await
            .map_err(|_| gone())?;
        answer.await.map_err(|_| gone())?
    }

    async fn set_plan(
        &self,
        _session: &SessionId,
        enabled: bool,
    ) -> Result<(), OrchestrationError> {
        let gone = || OrchestrationError::Failed {
            message: "the session is closed".to_owned(),
        };
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::SetPlan { enabled, reply })
            .await
            .map_err(|_| gone())?;
        answer.await.map_err(|_| gone())?
    }

    async fn set_workspace(
        &self,
        _session: &SessionId,
        root: rust_cli_kernel_api::env::EnvPath,
    ) -> Result<(), OrchestrationError> {
        let gone = || OrchestrationError::Failed {
            message: "the session is closed".to_owned(),
        };
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::SetWorkspace { root, reply })
            .await
            .map_err(|_| gone())?;
        answer.await.map_err(|_| gone())?
    }

    /// monitor 的 owner 是会话 actor（工作线 F83）。
    async fn arm_monitor(
        &self,
        _session: &SessionId,
        monitor: rust_cli_kernel_api::monitor::MonitorArmed,
    ) -> Result<(), OrchestrationError> {
        let gone = || OrchestrationError::Failed {
            message: "the session is closed".to_owned(),
        };
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::ArmMonitor { monitor, reply })
            .await
            .map_err(|_| gone())?;
        answer.await.map_err(|_| gone())?
    }

    /// 选择的 owner 是会话 actor：按请求时的值回答，不另存副本。
    async fn current_selection(&self, _session: &SessionId) -> Option<ModelSelection> {
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::Selection { reply })
            .await
            .ok()?;
        answer.await.ok()
    }

    /// 对话的 owner 是会话 actor：按请求时已提交的消息回答（fork 子代理继承它，修复线 L-A）。
    async fn conversation(&self, _session: &SessionId) -> Option<Vec<Message>> {
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::Conversation { reply })
            .await
            .ok()?;
        answer.await.ok()
    }

    /// 会话级的询问入口（docs/specs/rust-cli/agents.md §4.1.1）：回合通道在回合结束时关闭，后台子代理在那之后仍要问。
    fn lasting(&self, _session: &SessionId) -> Option<Arc<dyn Interactions>> {
        Some(Arc::new(SessionAsks {
            requests: self.asks.clone(),
            session: self.session.clone(),
            detached: self.detached.clone(),
        }))
    }
}

/// 会话级的询问入口：询问经会话的请求通道交给 actor，登记为不属于回合的交互，只随会话关闭取消。actor 收不下时
/// （已经退出、正在关闭，或关闭时取消了这条询问）改走进程级的入口（agents.md §4.1.2）。
pub(crate) struct SessionAsks {
    requests: mpsc::WeakSender<crate::actor::Request>,
    session: SessionId,
    detached: Option<Arc<crate::detached::DetachedAsks>>,
}

impl SessionAsks {
    /// 交给会话 actor；actor 收不下（发不出去或回复被丢下）时为 None。
    async fn to_actor(
        &self,
        request: InteractionRequest,
    ) -> Option<Result<InteractionAnswer, OrchestrationError>> {
        let (reply, answer) = oneshot::channel();
        // 只在发送时持有强引用：等回答的这段时间不拖住会话的请求通道。
        let requests = self.requests.upgrade()?;
        requests
            .send(crate::actor::Request::Ask { request, reply })
            .await
            .ok()?;
        drop(requests);
        answer.await.ok()
    }
}

#[async_trait]
impl Interactions for SessionAsks {
    async fn ask(
        &self,
        _session: &SessionId,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        if let Some(answered) = self.to_actor(request.clone()).await {
            return answered;
        }
        // 修复（工作线 F72）：以前父会话关闭之后这里直接按会话已关闭失败，后台子代理按拒绝处理。
        match &self.detached {
            Some(detached) => detached.ask(&self.session, request).await,
            None => Err(OrchestrationError::Failed {
                message: "the session is closed".to_owned(),
            }),
        }
    }

    async fn cancel_ask(
        &self,
        _session: &SessionId,
        tool_use: &rust_cli_kernel_api::ids::ToolUseId,
    ) {
        // 会话已关闭时，关闭已经取消了全部待回答的交互；询问可能已转到进程级的入口。
        if let Some(requests) = self.requests.upgrade() {
            let request = crate::actor::Request::CancelAsk {
                tool_use: tool_use.clone(),
            };
            let _ = requests.send(request).await;
        }
        if let Some(detached) = &self.detached {
            detached.cancel(&self.session, tool_use);
        }
    }

    fn lasting(&self, _session: &SessionId) -> Option<Arc<dyn Interactions>> {
        Some(Arc::new(Self {
            requests: self.requests.clone(),
            session: self.session.clone(),
            detached: self.detached.clone(),
        }))
    }
}

#[async_trait]
impl RunSink for ActorSink {
    async fn commit_message(&self, message: Message) -> Result<(), StoreError> {
        self.call(|reply| SinkRequest::Message { message, reply })
            .await
    }

    async fn commit_intent(
        &self,
        operation: OperationId,
        fingerprint: String,
    ) -> Result<(), StoreError> {
        self.call(|reply| SinkRequest::Intent {
            operation,
            fingerprint,
            reply,
        })
        .await
    }

    async fn commit_messages(&self, messages: Vec<Message>) -> Result<(), StoreError> {
        self.call(|reply| SinkRequest::Messages { messages, reply })
            .await
    }

    async fn commit_metered(
        &self,
        message: Option<Message>,
        metered: rust_cli_kernel_api::metering::Metered,
    ) -> Result<(), StoreError> {
        self.call(|reply| SinkRequest::Metered {
            message,
            metered: Box::new(metered),
            reply,
        })
        .await
    }

    async fn compact_started(&self, trigger: CompactTrigger) {
        let _ = self.sender.send(SinkRequest::CompactStarted(trigger)).await;
    }

    async fn emit(&self, event: QueryEvent) -> bool {
        self.sender.send(SinkRequest::Event(event)).await.is_ok()
    }

    /// 插话（并入前先跑 UserPromptSubmit）与后台通知，按到达顺序（steering.rs，query.md §5）。
    async fn take_absorbed(&self) -> Vec<Absorbed> {
        self.absorbed().await
    }

    async fn current_mode(&self) -> Option<PermissionMode> {
        let (reply, answer) = oneshot::channel();
        self.sender.send(SinkRequest::Mode { reply }).await.ok()?;
        answer.await.ok()
    }

    async fn current_mode_and_plan_flags(&self) -> Option<(PermissionMode, PlanModeFlags)> {
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::ModeAndPlanFlags { reply })
            .await
            .ok()?;
        answer.await.ok()
    }

    async fn current_workspace(&self) -> Option<rust_cli_kernel_api::env::WorkspaceRef> {
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::Workspace { reply })
            .await
            .ok()?;
        answer.await.ok()
    }

    async fn current_goal(&self) -> Option<String> {
        let (reply, answer) = oneshot::channel();
        self.sender.send(SinkRequest::Goal { reply }).await.ok()?;
        answer.await.ok().flatten()
    }

    async fn goal_checked(
        &self,
        condition: String,
        verdict: rust_cli_kernel_api::hooks::StopVerdict,
    ) -> Result<Vec<Message>, StoreError> {
        self.call(|reply| SinkRequest::GoalChecked {
            condition,
            verdict,
            reply,
        })
        .await
    }
}

#[cfg(test)]
#[path = "sink_tests.rs"]
mod tests;
