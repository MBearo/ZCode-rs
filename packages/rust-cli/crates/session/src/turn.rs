//! 回合内的逻辑：开始回合、处理主循环的提交请求、待回答的交互、回合收尾。
//!
//! 每条消息先提交到存储，成功后才进入内存历史并发出事件；提交失败时中止本回合并以失败收尾。

use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::hooks::HookOutcome;
use rust_cli_kernel_api::ids::{CommandId, InteractionId, OperationId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::metering::{CallStatus, Metered};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, OrchestrationError, QueryEvent, QueryRequest, QueryStop,
};
use rust_cli_kernel_api::session_api::{Ack, KernelEvent, TurnOutcome};
use rust_cli_kernel_api::store::{Fact, StoreError};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::actor::{Actor, Pending, Running, accepted, rejected};
use crate::lane::Waiting;
use crate::sink::SinkRequest;

/// 主循环提交请求的缓冲；满了时主循环等待，不丢请求。人类输入的 Bash 与 hook 的回合任务用同一个值（F55）。
pub(crate) const SINK_REQUESTS: usize = 256;
/// 待回答交互在存储里的 entry 种类（node-store 映射成 `rust-cli/<kind>`）。
const PENDING_INTERACTION: &str = "pending_interaction";
const INTERACTION_RESOLVED: &str = "interaction_resolved";

fn store_error(reason: String) -> StoreError {
    StoreError::Storage { message: reason }
}

fn metered_fact(metered: Metered) -> Fact {
    match metered {
        Metered::Model(call) => Fact::ModelCalled { call },
        Metered::Tool(call) => Fact::ToolCalled { call },
    }
}

/// 开启一个回合的输入。
pub(crate) enum TurnInput {
    Human {
        command: CommandId,
        text: String,
        /// 插话在并入点已跑过的 UserPromptSubmit 结果（query.md §4.1）；有值时不再跑 hook。
        screened: Option<Box<HookOutcome>>,
    },
    /// 回合结束后还没并入的后台通知（命令与子代理，到达顺序）：连续的几条合成一个回合，每条一条
    /// meta user 消息、只调一次模型（docs/specs/rust-cli/query.md §5，通知合并）。
    Notices(Vec<Waiting>),
    /// 到期的定时任务：prompt 原文作为 meta user 消息（docs/specs/rust-cli/tools-more.md §10.4）。
    Cron(rust_cli_kernel_api::cron::CronFire),
    /// 排队的 bash 模式命令（tui-input.md §2.2）。
    Bash { command: CommandId, line: String },
    /// 带内容块的人类输入（粘贴的图片，tui-input.md §3.2）。
    Content {
        command: CommandId,
        content: Vec<ContentBlock>,
    },
}

impl Actor {
    pub(crate) async fn start_turn(&mut self, input: TurnInput) -> Result<(), String> {
        let meta = |source: &str| UserOrigin::Meta {
            source: source.to_owned(),
        };
        // 定时任务触发：界面行与 `/loop` 的折叠说明（排在 prompt 之前，tools-more.md §18.3）。
        let mut fired = None;
        let (texts, notices) = match input {
            // 人类输入先跑 UserPromptSubmit（hooks.rs），输入在回合任务里提交。
            TurnInput::Human {
                command,
                text,
                screened,
            } => {
                self.start_human_turn(command, vec![ContentBlock::Text { text }], screened);
                return Ok(());
            }
            TurnInput::Content { command, content } => {
                self.start_human_turn(command, content, None);
                return Ok(());
            }
            TurnInput::Bash { command, line } => {
                self.start_bash_turn(command, line);
                return Ok(());
            }
            TurnInput::Cron(fire) => {
                fired = Some(self.fire_prelude(fire.kind));
                // `/loop` 的哨兵在这里展开（tools-more.md §17.3）。
                let prompt = self.resolve_loop_prompt(fire.prompt).await;
                (vec![(prompt, meta(crate::cron::CRON_SOURCE))], Vec::new())
            }
            TurnInput::Notices(notices) => {
                // 同伴消息开回合时用空闲的写法与自己的来源（cross-session.md §1.7）。
                let texts = notices
                    .iter()
                    .map(|notice| (notice.turn_text(), meta(notice.source())))
                    .collect();
                (texts, notices)
            }
        };
        let before = fired
            .as_ref()
            .map(|fired| fired.before.clone())
            .unwrap_or_default();
        let users: Vec<Message> = before
            .into_iter()
            .chain(texts.into_iter().map(|(text, origin)| {
                Message::User(UserMessage {
                    id: self.deps.ids.message(),
                    content: vec![ContentBlock::Text { text }],
                    origin,
                })
            }))
            .collect();
        let facts = users
            .iter()
            .map(|message| Fact::MessageAppended {
                message: message.clone(),
            })
            .collect();
        // 一个事务：几条通知一起成立；折叠说明与定时任务的 prompt 一起成立。
        self.commit(None, facts).await?;
        self.messages.extend(users.iter().cloned());
        if let (Some(fired), Some(prompt)) = (fired, users.last()) {
            self.fired(fired, prompt.id().clone());
        }
        // 通知已经在对话里：现在才 ack（至少一次）。
        for notice in &notices {
            self.ack_notice(notice).await;
        }
        let turn = self.deps.ids.turn();
        self.log.append(KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: None,
        });
        for message in &users {
            self.log.append(KernelEvent::MessageDone {
                message: message.clone(),
            });
        }
        let abort = Abort::new();
        let (sender, requests) = mpsc::channel(SINK_REQUESTS);
        let run = self.deps.ids.run();
        let request = QueryRequest {
            session: self.meta.id.clone(),
            run: run.clone(),
            workspace: self.meta.workspace.clone(),
            agent: None,
            messages: self.messages.clone(),
            selection: self.selection.clone(),
            mode: self.mode(),
            source: QuerySource::MainThread,
            max_turns: self.deps.max_turns,
            prompt_hook_messages: Vec::new(),
        };
        let runner = self.deps.runner.clone();
        let sink = Arc::new(self.sink(sender));
        let signal = abort.clone();
        let task = self.deps.spawner.spawn("query-run", async move {
            runner.run(request, sink, signal).await
        });
        self.running = Some(Running {
            turn,
            run: Some(run),
            abort,
            requests,
            task,
            failure: None,
            // checkpoint 挂在这次运行最后一条 user 消息上（drive 的同一规则）。
            prompt: users.last().map(|message| message.id().clone()),
            command: None,
        });
        Ok(())
    }

    pub(crate) async fn on_sink(&mut self, request: SinkRequest) {
        match request {
            SinkRequest::Message { message, reply } => {
                let _ = reply.send(self.record(message).await);
            }
            SinkRequest::Intent {
                operation,
                fingerprint,
                reply,
            } => {
                let _ = reply.send(self.intent(operation, fingerprint).await);
            }
            SinkRequest::Event(event) => self.forward(event),
            SinkRequest::Ask { request, reply } => {
                self.ask(request, reply, crate::actor::AskScope::Turn).await
            }
            SinkRequest::CancelAsk { tool_use } => self.cancel_ask(&tool_use).await,
            SinkRequest::ClaimSteering { reply } => {
                let _ = reply.send(self.claim_steering());
            }
            SinkRequest::SettleSteering { fates, reply } => {
                self.settle_steering(fates);
                let _ = reply.send(());
            }
            SinkRequest::Metered {
                message,
                metered,
                reply,
            } => {
                let usage = vec![metered_fact(*metered)];
                let result = match message {
                    Some(message) => self.record_with(message, usage).await,
                    None => self.record_facts(usage).await,
                };
                let _ = reply.send(result);
            }
            SinkRequest::Messages { messages, reply } => {
                let _ = reply.send(self.record_all(messages).await);
            }
            SinkRequest::CompactStarted(trigger) => crate::compact::compact_started(self, trigger),
            SinkRequest::SetMode { mode, reply } => {
                let changed = self.change_mode(None, mode).await;
                let _ =
                    reply.send(changed.map_err(|message| OrchestrationError::Failed { message }));
            }
            SinkRequest::SetPlan { enabled, reply } => {
                let changed = self.change_plan(enabled).await;
                let _ =
                    reply.send(changed.map_err(|message| OrchestrationError::Failed { message }));
            }
            SinkRequest::Mode { reply } => {
                let _ = reply.send(self.mode());
            }
            SinkRequest::ModeAndPlanFlags { reply } => {
                let _ = reply.send((self.mode(), self.plan_flags));
            }
            SinkRequest::Title(title) => self.retitle(title).await,
            SinkRequest::SetWorkspace { root, reply } => {
                let _ = reply.send(self.change_workspace(root).await);
            }
            SinkRequest::Workspace { reply } => {
                let _ = reply.send(self.meta.workspace.clone());
            }
            SinkRequest::Selection { reply } => {
                let _ = reply.send(self.selection.clone());
            }
            SinkRequest::Goal { reply } => {
                let _ = reply.send(self.goal.condition());
            }
            SinkRequest::GoalChecked {
                condition,
                verdict,
                reply,
            } => {
                let _ = reply.send(self.goal_checked(condition, verdict).await);
            }
        }
    }

    fn failure(&self) -> Option<StoreError> {
        self.running
            .as_ref()
            .and_then(|running| running.failure.clone())
            .map(store_error)
    }

    /// 提交失败：记下原因并中止本回合，之后的提交一律拒绝。
    fn fail(&mut self, reason: &str) -> StoreError {
        if let Some(running) = &mut self.running {
            running.abort.abort(AbortReason::User);
            running.failure.get_or_insert_with(|| reason.to_owned());
        }
        store_error(reason.to_owned())
    }

    async fn record(&mut self, message: Message) -> Result<(), StoreError> {
        self.record_with(message, Vec::new()).await
    }

    /// 同 [`Self::record`]，`usage` 是同一批里的计量事实（排在消息与 checkpoint 之后，usage.md §2.5）。
    async fn record_with(&mut self, message: Message, usage: Vec<Fact>) -> Result<(), StoreError> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        let fact = Fact::MessageAppended {
            message: message.clone(),
        };
        // 工具结果与它的 checkpoint 在一个事务里提交（checkpoints.rs）。
        let (checkpoints, extra) = self.settle_checkpoints(&message);
        let command = self.command_of(message.id());
        // 本回合开头的人类输入：之前没有 user 消息时是首个回合（会话标题，title.rs）。
        let prompt = self
            .running
            .as_ref()
            .is_some_and(|running| running.prompt.as_ref() == Some(message.id()));
        let first = !self.messages.iter().any(|m| matches!(m, Message::User(_)));
        if let Err(reason) = self
            .commit(command, [vec![fact], extra, usage].concat())
            .await
        {
            return Err(self.fail(&reason));
        }
        self.plan_attachment_committed(&message);
        self.goal_committed(&message);
        // 工具边界并入的通知：提交后才从通道去掉并 ack（lane.rs）。
        self.notice_absorbed(&message).await;
        self.messages.push(message.clone());
        if prompt {
            self.after_prompt(&message, first);
        }
        // 缺陷修复（R5-8）：原来内核从不发上下文用量，footer 的上下文提示永远不出现（单测手工注入所以通过）。
        // 在每条带用量的 assistant 之后重算；提示的级别与文字选择由压缩模块给出（compact.md §2.1，F14 起是
        // ContextNotice：以前只发 `(token, 有效窗口)`，TUI 只能显示 `N% context used` 一种）。F27 起同一处另发
        // `(token, 有效窗口)`，给 footer 的 ctx。
        let usage = matches!(&message, Message::Assistant(assistant) if assistant.usage.is_some());
        self.log.append(KernelEvent::MessageDone { message });
        if usage {
            self.context_changed();
        }
        self.checkpoints_settled(checkpoints);
        Ok(())
    }

    /// 没有消息的计量（没有内容的失败请求、丢弃的响应）：单独一批，失败时与其他提交一样中止本回合。
    async fn record_facts(&mut self, facts: Vec<Fact>) -> Result<(), StoreError> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        self.commit(None, facts)
            .await
            .map_err(|reason| self.fail(&reason))
    }

    async fn intent(
        &mut self,
        operation: OperationId,
        fingerprint: String,
    ) -> Result<(), StoreError> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        let fact = Fact::OperationIntent {
            operation,
            fingerprint,
        };
        self.commit(None, vec![fact])
            .await
            .map_err(|reason| self.fail(&reason))
    }

    fn forward(&mut self, event: QueryEvent) {
        let Some(turn) = self.running.as_ref().map(|running| running.turn.clone()) else {
            return;
        };
        let event = match event {
            QueryEvent::Delta {
                message,
                index,
                delta,
            } => KernelEvent::StreamDelta {
                turn,
                response: u32::try_from(message).unwrap_or(u32::MAX),
                index,
                delta,
            },
            QueryEvent::ToolStarted {
                tool_use,
                name,
                input,
            } => KernelEvent::ToolStarted {
                tool_use,
                name,
                input,
            },
            QueryEvent::ToolProgress { tool_use, progress } => {
                KernelEvent::ToolProgress { tool_use, progress }
            }
            QueryEvent::ToolFinished { tool_use, output } => {
                self.unsettle(&tool_use, &output);
                KernelEvent::ToolDone { tool_use, output }
            }
            // 模型通知只供显示，不落库（stall-visibility.md §1）。
            QueryEvent::ModelNotice { notice } => KernelEvent::ModelNotice { notice },
        };
        self.log.append(event);
    }

    pub(crate) async fn finish_turn(&mut self) {
        let Some(running) = self.running.take() else {
            return;
        };
        self.cancel_turn_asks().await;
        let result = running.task.await;
        let outcome = match (running.failure, result) {
            (Some(message), _) => TurnOutcome::Failed { message },
            (None, Ok(Ok(QueryStop::Completed))) => TurnOutcome::Success,
            (None, Ok(Ok(QueryStop::Aborted))) => TurnOutcome::Interrupted,
            (None, Ok(Ok(QueryStop::MaxTurns))) => TurnOutcome::Failed {
                message: "reached the maximum number of turns".to_owned(),
            },
            (None, Ok(Ok(QueryStop::Blocked { message } | QueryStop::ApiError { message }))) => {
                TurnOutcome::Failed { message }
            }
            (None, Ok(Err(error))) => TurnOutcome::Failed {
                message: error.to_string(),
            },
            (None, Err(error)) => TurnOutcome::Failed {
                message: error.to_string(),
            },
        };
        // Node `turn_usage`（usage.md §2.4）：只在这次运行的输入已经落库时写；压缩运行没有输入。用量行是观测，
        // 写不进去只记日志，不改回合的结果。
        let prompted = running
            .prompt
            .as_ref()
            .is_some_and(|prompt| self.messages.iter().rev().any(|m| m.id() == prompt));
        if prompted {
            let status = match &outcome {
                TurnOutcome::Success => CallStatus::Completed,
                TurnOutcome::Interrupted => CallStatus::Cancelled,
                TurnOutcome::Failed { .. } => CallStatus::Error,
            };
            if let Err(reason) = self.commit(None, vec![Fact::TurnEnded { status }]).await {
                tracing::warn!(%reason, "the turn usage could not be stored");
            }
        }
        let usage = self
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Assistant(assistant) => assistant.usage,
                _ => None,
            });
        self.log.append(KernelEvent::TurnFinished {
            turn: running.turn,
            outcome,
            usage,
        });
        self.start_next().await;
    }

    /// 空闲时开下一个回合：先 next 通道（插话与后台通知，到达顺序），再 `Queue` 输入（优先级 `later`），
    /// 最后是到期的定时任务（lane.rs）。
    pub(crate) async fn start_next(&mut self) {
        // 关闭中（R1-1）：排队的输入、通知与定时任务都不再开回合；通知没有 ack，会话再打开时送达。进程要退出
        // （`Shutdown`，修复线 F38）时同样。
        if self.closing.is_some() || self.shutting_down {
            return;
        }
        let input = if let Some(input) = self.next_turn_input() {
            input
        } else if let Some(fire) = self.next_cron().await {
            TurnInput::Cron(fire)
        } else {
            return;
        };
        // 提交失败时只记日志：排队输入已出队，前端从 QueueChanged 看到它消失；通知没有 ack，
        // 会再次送达。
        if let Err(reason) = self.start_turn(input).await {
            tracing::warn!(%reason, "queued input could not start a turn");
        }
    }
}

#[path = "turn_interact.rs"]
mod interact;
