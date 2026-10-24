//! 父会话已关闭之后的询问（docs/specs/rust-cli/agents.md §4.1.2，工作线 F72）：进程级的入口。`/clear`、`/resume` 之后
//! 旧会话的后台子代理经它询问，前端订阅、回答。询问只在内存里（对话框不落库），“总是允许”的规则提交到旧会话。
//!
//! 修复（用户决定 2026-10-23）：以前父会话 actor 退出之后，会话级询问入口发不出询问，后台子代理按拒绝处理。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::ids::{InteractionId, OwnerEpoch, SessionId, ToolUseId};
use rust_cli_kernel_api::orchestration::{
    DetachedAsk, DetachedEvent, InteractionAnswer, InteractionRequest, OrchestrationError,
};
use rust_cli_kernel_api::session_api::KernelError;
use rust_cli_kernel_api::store::{FactBatch, SessionStore};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

type Reply = oneshot::Sender<Result<InteractionAnswer, OrchestrationError>>;

struct Entry {
    ask: DetachedAsk,
    reply: Reply,
}

#[derive(Default)]
struct State {
    /// 关闭过的会话的标题（标签用）。
    titles: HashMap<SessionId, String>,
    /// 还没回答的询问，按到达顺序。
    pending: Vec<Entry>,
    /// 订阅者：每个一个无界通道（不丢事件）。
    watchers: Vec<mpsc::UnboundedSender<DetachedEvent>>,
}

impl State {
    fn emit(&mut self, event: &DetachedEvent) {
        self.watchers
            .retain(|watcher| watcher.send(event.clone()).is_ok());
    }

    fn watched(&mut self) -> bool {
        self.watchers.retain(|watcher| !watcher.is_closed());
        !self.watchers.is_empty()
    }
}

/// 旧会话询问的入口（runtime 建一个，经 `SessionDeps::detached` 给每个会话）。
pub struct DetachedAsks {
    store: Arc<dyn SessionStore>,
    ids: Arc<dyn Ids>,
    state: Mutex<State>,
}

fn gone() -> OrchestrationError {
    OrchestrationError::Failed {
        message: "the session is closed".to_owned(),
    }
}

impl DetachedAsks {
    pub fn new(store: Arc<dyn SessionStore>, ids: Arc<dyn Ids>) -> Self {
        Self {
            store,
            ids,
            state: Mutex::default(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 会话因 `/clear`、`/resume` 关闭（取消待回答的交互之前）：登记它与它的标题，之后它的询问用标题做标签。
    pub(crate) fn closed(&self, session: &SessionId, title: &str) {
        self.state()
            .titles
            .insert(session.clone(), title.to_owned());
    }

    /// 旧会话的询问：会话没有登记过（不是因 `/clear`、`/resume` 关闭的）或没有订阅者（没人能回答）时不收，否则登记、
    /// 通知订阅者，等回答。
    pub(crate) async fn ask(
        &self,
        session: &SessionId,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        let (reply, answer) = oneshot::channel();
        {
            let mut state = self.state();
            let Some(title) = state.titles.get(session).cloned() else {
                return Err(gone());
            };
            if !state.watched() {
                return Err(gone());
            }
            let ask = DetachedAsk {
                interaction: self.ids.interaction(),
                session: session.clone(),
                title,
                request,
            };
            tracing::info!(event = "interaction.detached", session = %session, interaction = %ask.interaction, "a closed session asks");
            state.emit(&DetachedEvent::Asked {
                ask: Box::new(ask.clone()),
            });
            state.pending.push(Entry { ask, reply });
        }
        answer.await.map_err(|_| gone())?
    }

    /// 工具被取消，撤回它的询问（`Interactions::cancel_ask`）。
    pub(crate) fn cancel(&self, session: &SessionId, tool_use: &ToolUseId) {
        let mut state = self.state();
        let Some(position) = state.pending.iter().position(|entry| {
            entry.ask.session == *session && entry.ask.request.tool_use() == tool_use
        }) else {
            return;
        };
        let entry = state.pending.remove(position);
        tracing::info!(event = "interaction.cancelled", session = %session, interaction = %entry.ask.interaction, "a closed session's question was withdrawn");
        state.emit(&DetachedEvent::Resolved {
            interaction: entry.ask.interaction,
        });
    }

    /// 订阅：先给出还没回答的询问，再给出之后的变化（同一把锁下取快照与登记，不漏不重）。
    pub fn subscribe(&self) -> BoxStream<'static, DetachedEvent> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut state = self.state();
        for entry in &state.pending {
            let _ = sender.send(DetachedEvent::Asked {
                ask: Box::new(entry.ask.clone()),
            });
        }
        state.watchers.push(sender);
        futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|event| (event, receiver))
        })
        .boxed()
    }

    /// 回答：“总是允许”的规则先提交到旧会话，成功后才唤醒询问方；提交失败时询问留着，可以重答。
    pub async fn resolve(
        &self,
        interaction: &InteractionId,
        answer: Value,
    ) -> Result<(), KernelError> {
        let (position, entry) = {
            let mut state = self.state();
            let Some(position) = state
                .pending
                .iter()
                .position(|entry| entry.ask.interaction == *interaction)
            else {
                return Err(KernelError::Failed {
                    message: format!("no pending interaction {interaction}"),
                });
            };
            (position, state.pending.remove(position))
        };
        if let Some(fact) = crate::rules::remembered(&entry.ask.request, &answer) {
            let batch = FactBatch {
                owner_epoch: OwnerEpoch::LOCAL,
                command: None,
                facts: vec![fact],
                turn: None,
            };
            if let Err(error) = self.store.commit(&entry.ask.session, batch).await {
                tracing::warn!(%error, session = %entry.ask.session, interaction = %interaction, "could not remember the rules of a closed session's question");
                let mut state = self.state();
                let position = position.min(state.pending.len());
                state.pending.insert(position, entry);
                return Err(KernelError::Failed {
                    message: error.to_string(),
                });
            }
        }
        tracing::info!(
            event = "interaction.resolved",
            session = %entry.ask.session,
            interaction = %interaction,
            decision = answer["decision"].as_str().unwrap_or_default(),
            remembered = answer["remember"].as_array().map_or(0, Vec::len),
            "a closed session's question was answered"
        );
        let _ = entry.reply.send(Ok(InteractionAnswer {
            interaction: interaction.clone(),
            answer,
        }));
        self.state().emit(&DetachedEvent::Resolved {
            interaction: interaction.clone(),
        });
        Ok(())
    }
}

#[cfg(test)]
#[path = "detached_tests.rs"]
mod tests;
