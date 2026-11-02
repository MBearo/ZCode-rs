//! 会话侧 hooks 的接线（docs/specs/rust-cli/session-lifecycle.md §3、hooks.md §7）。
//!
//! hook 都在子任务里跑：UserPromptSubmit 是回合任务的第一段（输入管线在 input.rs），SessionStart 是启动后台工作，
//! Notification 是可取消的计时任务，SessionEnd 在调用方的任务里跑。actor 只按 `HookOutcome`
//! 提交消息、改标题、决定流程，所以 hook 跑多久都不会挡住中断与 `view`。

use rust_cli_kernel_api::hooks::{HookContext, HookEvent, HookEventName, HookOutcome};
use rust_cli_kernel_api::ids::CommandId;
// 权限询问多久没回答才发 Notification（6000 ms）：只定义在 kernel-api，TUI 的终端通知用同一个值（F58：
// 以前这里与 tui 各写一份）。
use rust_cli_kernel_api::interaction::NOTIFY_AFTER as PERMISSION_NOTIFY_DELAY;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::session_api::{
    Delivery, EndReason, KernelEvent, QueuedInput, StartSource,
};
use rust_cli_kernel_api::store::{Fact, SessionMeta};
use serde_json::{Map, Value, json};
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::SessionDeps;
use crate::actor::{Actor, Job, JobDone};

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
        context,
        ..HookEvent::new(name, meta.id.clone(), meta.workspace.clone(), payload)
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

/// SessionStart 给的首条输入的命令 ID 前缀。
pub(crate) const SESSION_START_INPUT: &str = "session-start-";

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

    /// SessionStart 的结果：一个事务提交 hook 消息，改标题，`initial_user_message` 排到队首（命令 ID 以
    /// [`SESSION_START_INPUT`] 开头：回合结束后单独一个回合，不与排队输入成批，lane.rs）。
    pub(crate) async fn finish_startup(&mut self, outcome: HookOutcome) {
        if !outcome.messages.is_empty() {
            match self.append(None, outcome.messages, Vec::new()).await {
                Ok(appended) => self.after_commit(appended).await,
                Err(reason) => {
                    tracing::warn!(%reason, "SessionStart hook messages were not stored")
                }
            }
        }
        if let Some(title) = outcome.session_title {
            self.retitle(title).await;
        }
        if let Some(text) = outcome.initial_user_message {
            let command = CommandId::new(format!(
                "{SESSION_START_INPUT}{}",
                self.deps.ids.operation()
            ));
            // 排在最前（通道与队列的头部），回合结束后先开它。
            self.inbox.push_first(QueuedInput {
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
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "early_interrupt_tests.rs"]
mod early_interrupt_tests;
