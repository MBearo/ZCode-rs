//! 会话侧 hooks 的接线（docs/specs/rust-cli/session-lifecycle.md §3、hooks.md §7）。
//!
//! hook 都在子任务里跑：UserPromptSubmit 是回合任务的第一段，SessionStart 是启动后台工作，
//! Notification 是可取消的计时任务，SessionEnd 在调用方的任务里跑。actor 只按 `HookOutcome`
//! 提交消息、改标题、决定流程，所以 hook 跑多久都不会挡住中断与 `view`。

use std::sync::Arc;

use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::hooks::{HookContext, HookEvent, HookEventName, HookOutcome};
use rust_cli_kernel_api::ids::{CommandId, MessageId};
// 权限询问多久没回答才发 Notification（6000 ms）：只定义在 kernel-api，TUI 的终端通知用同一个值（F58：
// 以前这里与 tui 各写一份）。
use rust_cli_kernel_api::interaction::NOTIFY_AFTER as PERMISSION_NOTIFY_DELAY;
use rust_cli_kernel_api::message::{
    AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{InteractionRequest, QueryRequest, QueryStop, RunSink};
use rust_cli_kernel_api::session_api::{
    Delivery, EndReason, KernelEvent, QueuedInput, StartSource,
};
use rust_cli_kernel_api::store::{Fact, SessionMeta};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::SessionDeps;
use crate::actor::{Actor, Job, JobDone, Running};

// 回合任务提交请求的缓冲（与 turn.rs 相同，F55）。
use crate::turn::SINK_REQUESTS;

fn source_name(source: StartSource) -> &'static str {
    match source {
        StartSource::Startup => "startup",
        StartSource::Resume => "resume",
        StartSource::Clear => "clear",
        StartSource::Compact => "compact",
        StartSource::Fork => "fork",
    }
}

fn reason_name(reason: EndReason) -> &'static str {
    match reason {
        EndReason::Clear => "clear",
        EndReason::Resume => "resume",
        EndReason::Logout => "logout",
        EndReason::PromptInputExit => "prompt_input_exit",
        EndReason::Other => "other",
    }
}

fn event(
    meta: &SessionMeta,
    name: HookEventName,
    payload: Value,
    context: HookContext,
) -> HookEvent {
    HookEvent {
        name,
        session: meta.id.clone(),
        workspace: meta.workspace.clone(),
        payload,
        context,
    }
}

/// SessionStart 的载荷：`{source, model, session_title?}`（hooks.md §7）。
pub(crate) fn session_start(
    meta: &SessionMeta,
    source: StartSource,
    selection: &ModelSelection,
) -> HookEvent {
    let mut payload = Map::new();
    payload.insert("source".into(), source_name(source).into());
    payload.insert("model".into(), selection.model.clone().into());
    if !meta.title.is_empty() {
        payload.insert("session_title".into(), meta.title.clone().into());
    }
    // prompt hook 的缺省模型经小模型槽位从当前主模型得出（model.md §6.1）。
    let context = HookContext {
        selection: Some(selection.clone()),
        ..HookContext::default()
    };
    event(
        meta,
        HookEventName::SessionStart,
        Value::Object(payload),
        context,
    )
}

/// SessionEnd：不影响流程，返回失败的 hook 的提示（给 stderr）。
pub(crate) async fn session_end(
    deps: &SessionDeps,
    meta: &SessionMeta,
    reason: EndReason,
) -> Vec<String> {
    if !deps.hooks.has_hooks(HookEventName::SessionEnd) {
        return Vec::new();
    }
    let payload = json!({"reason": reason_name(reason)});
    let event = event(
        meta,
        HookEventName::SessionEnd,
        payload,
        HookContext::default(),
    );
    deps.hooks
        .run(event, CancellationToken::new())
        .await
        .notices
}

impl Actor {
    /// actor 启动时：有 SessionStart hook 才进入"启动"后台工作（没有时不改变行为，无竞态）。
    pub(crate) fn start_session(&mut self, source: StartSource) {
        if !self.deps.hooks.has_hooks(HookEventName::SessionStart) {
            return;
        }
        let event = session_start(&self.meta, source, &self.selection);
        let hooks = self.deps.hooks.clone();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let task = self.deps.spawner.spawn("session-start", async move {
            JobDone::Startup(Box::new(hooks.run(event, token).await))
        });
        self.job = Some(Job {
            task,
            cancel: Some(cancel),
        });
    }

    /// SessionStart 的结果：一个事务提交 hook 消息，改标题，`initial_user_message` 排到队首。
    pub(crate) async fn finish_startup(&mut self, outcome: HookOutcome) {
        let facts: Vec<Fact> = outcome
            .messages
            .iter()
            .map(|message| Fact::MessageAppended {
                message: message.clone(),
            })
            .collect();
        if !facts.is_empty() {
            match self.commit(None, facts).await {
                Ok(()) => {
                    for message in outcome.messages {
                        self.messages.push(message.clone());
                        self.log.append(KernelEvent::MessageDone { message });
                    }
                }
                Err(reason) => {
                    tracing::warn!(%reason, "SessionStart hook messages were not stored")
                }
            }
        }
        if let Some(title) = outcome.session_title {
            self.retitle(title).await;
        }
        if let Some(text) = outcome.initial_user_message {
            let command = CommandId::new(format!("session-start-{}", self.deps.ids.operation()));
            // 排在最前（通道与队列都放到头部），回合结束后先开它。
            self.lane
                .push_front(crate::lane::Lane::Input(command.clone()));
            self.queue.push_front(QueuedInput {
                command,
                text,
                delivery: Delivery::Queue,
            });
            self.emit_queue();
        }
        self.start_next().await;
    }

    /// 改标题：先提交，再改内存、发事件。
    pub(crate) async fn retitle(&mut self, title: String) {
        let fact = Fact::TitleChanged {
            title: title.clone(),
        };
        if let Err(reason) = self.commit(None, vec![fact]).await {
            tracing::warn!(%reason, "the hook title was not stored");
            return;
        }
        // hook 给的标题按 Node 记为 custom：之后不再自动生成（titles.md §4）。
        self.titles.mark_custom();
        self.meta.title.clone_from(&title);
        self.log.append(KernelEvent::TitleChanged { title });
    }

    /// UserPromptSubmit 的事件：`{prompt, session_title?}`，带当前模式（hooks.md §7）。
    pub(crate) fn user_prompt_submit(&self, text: &str) -> HookEvent {
        let mut payload = Map::new();
        payload.insert("prompt".into(), text.into());
        if !self.meta.title.is_empty() {
            payload.insert("session_title".into(), self.meta.title.clone().into());
        }
        let context = HookContext {
            mode: Some(self.mode()),
            selection: Some(self.selection.clone()),
            ..HookContext::default()
        };
        event(
            &self.meta,
            HookEventName::UserPromptSubmit,
            Value::Object(payload),
            context,
        )
    }

    /// 人类输入开的回合：UserPromptSubmit → 提交输入 → 主循环（当步 attachment 之后提交 hook 消息），都在回合
    /// 任务里（spec §3.1 的时序图）。`screened` 是插话在并入点已跑过的结果，有值时不再跑 hook。`content` 是输入的
    /// 内容块（文字，加粘贴的图片，tui-input.md §3.2）；UPS 与 `@` 提及只看文字块。
    pub(crate) fn start_human_turn(
        &mut self,
        command: CommandId,
        content: Vec<ContentBlock>,
        screened: Option<Box<HookOutcome>>,
    ) {
        let prompt = self.deps.ids.message();
        let text = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let user = Message::User(UserMessage {
            id: prompt.clone(),
            content,
            origin: UserOrigin::Human,
        });
        let turn = self.deps.ids.turn();
        self.log.append(KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: Some(command.clone()),
        });
        let hook = self.user_prompt_submit(&text);
        // `/goal` 是内置的本地命令：消息按 actor 现在的目标算好，不经 skill 展开（goal.md §2.1）。
        let goal = crate::goal::command(&text).map(|args| self.goal_inputs(args, prompt.clone()));
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
        let hooks = self.deps.hooks.clone();
        let runner = self.deps.runner.clone();
        let commands = self.deps.commands.clone();
        let ids = self.deps.ids.clone();
        let sink = Arc::new(self.sink(sender));
        let context = self.command_context(&request.run, &abort, sink.clone());
        let mention_context = self.command_context(&request.run, &abort, sink.clone());
        let user_inputs = self.deps.inputs.clone();
        let signal = abort.clone();
        let task = self.deps.spawner.spawn("query-run", async move {
            // 斜杠命令先展开（UPS 在展开之后，ext.md §2.7）；不是命令时就是这条输入本身。
            let mut inputs = match goal {
                Some(inputs) => inputs,
                None => crate::slash::expand(commands.as_ref(), ids.as_ref(), user, context).await,
            };
            // 普通输入的 `@` 提及（在 UPS 之前）：attachment 排在输入之后提交
            // （docs/specs/rust-cli/tui-input.md §1.1）。
            if inputs.plain && inputs.query {
                let mentioned = user_inputs.mentions(&text, mention_context).await;
                inputs
                    .messages
                    .extend(mentioned.into_iter().map(|attachment| {
                        Message::Attachment(AttachmentMessage {
                            id: ids.message(),
                            source: attachment.source,
                            text: attachment.text,
                            data: attachment.data,
                        })
                    }));
            }
            // 修 bug（F37）：以前展开之后、hook 之后各看一次中止，已中止就返回 `Aborted`、什么都不提交，空档里按 esc、
            // 收到退出信号或关闭时用户刚发的输入没了。处理输入之后不看中止（session-lifecycle.md §3.1）：输入照常
            // 提交，主循环在发请求之前看到中止、写中断标记（query.md §1.2）。阻塞与停止仍按 hook 的结果。
            // 退出（`Shutdown`，修复线 F38）不同：收到退出信号不中止、直接退出，这条输入还没进对话，所以什么都不写。
            let shutdown = || signal.reason() == Some(AbortReason::Shutdown);
            if shutdown() {
                return Ok(QueryStop::Aborted);
            }
            // 不调模型的本地结果（`user-invocable: false`）：只提交，不跑 UPS。
            if !inputs.query {
                for message in inputs.messages {
                    sink.commit_message(message).await?;
                }
                return Ok(QueryStop::Completed);
            }
            // 已中止时 hook 收到已取消的令牌，按取消收尾（结果里只有已完成的 hook 给出的）。
            let outcome = match screened {
                Some(outcome) => *outcome,
                None => hooks.run(hook, signal.token().clone()).await,
            };
            if shutdown() {
                return Ok(QueryStop::Aborted);
            }
            if outcome.blocking_error.is_some() {
                let message = outcome.notices.first().cloned().unwrap_or_default();
                return Ok(QueryStop::Blocked { message });
            }
            for message in &inputs.messages {
                sink.commit_message(message.clone()).await?;
            }
            if let Some(title) = outcome.session_title {
                sink.title(title).await;
            }
            if outcome.prevent_continuation {
                if !outcome.messages.is_empty() {
                    sink.commit_messages(outcome.messages.clone()).await?;
                }
                return Ok(QueryStop::Completed);
            }
            // 修 bug（F10）：以前 hook 消息在这里提交，排在当步 attachment（环境段等）之前；应先给输入
            // 产生当步 attachment，再追加 UPS 的结果。交给主循环在当步 attachment
            // 之后提交（query.md §1.1）。
            let mut request = request;
            request.messages.extend(inputs.messages);
            request.prompt_hook_messages = outcome.messages;
            runner.run(request, sink, signal).await
        });
        self.running = Some(Running {
            turn,
            run: Some(run),
            abort,
            requests,
            task,
            failure: None,
            prompt: Some(prompt),
            command: Some(command),
        });
    }

    /// 权限询问：6 s 内没有回答就发 Notification；回答或取消时（Pending 被丢弃）计时随之取消。
    pub(crate) fn notify_permission(&self, request: &InteractionRequest) -> Option<DropGuard> {
        let InteractionRequest::Permission { tool, .. } = request else {
            return None;
        };
        if !self.deps.hooks.has_hooks(HookEventName::Notification) {
            return None;
        }
        let payload = json!({
            "message": format!("ZCode needs your permission to use {tool}"),
            "notification_type": "permission_prompt",
        });
        let hook = event(
            &self.meta,
            HookEventName::Notification,
            payload,
            HookContext::default(),
        );
        let hooks = self.deps.hooks.clone();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        drop(
            self.deps
                .spawner
                .spawn("permission-notification", async move {
                    tokio::select! {
                        () = token.cancelled() => {}
                        () = tokio::time::sleep(PERMISSION_NOTIFY_DELAY) => {
                            let _ = hooks.run(hook, token).await;
                        }
                    }
                }),
        );
        Some(cancel.drop_guard())
    }

    /// 本回合开头那条 user 消息的提交带上命令 ID（Node `anchor.sourceCommandId`）。
    pub(crate) fn command_of(&self, message: &MessageId) -> Option<CommandId> {
        self.running
            .as_ref()
            .filter(|running| running.prompt.as_ref() == Some(message))
            .and_then(|running| running.command.clone())
    }
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "early_interrupt_tests.rs"]
mod early_interrupt_tests;
