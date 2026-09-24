//! Turn-level hooks of a run (Node `runtime/methods/hooks.ts`, `turn.ts`,
//! `turn-stop.ts`): SessionStart and UserPromptSubmit as the turn starts, Stop
//! at a text-only step end. Contexts become reminders in the process history.
use super::context::{RunContext, TransientKind};
use super::hook_runner::Hooks;
use crate::contract::{Event, EventSink};
use crate::domain::hooks::{HookEvent, decision, display, input};
use crate::domain::{plan_mode, session_runtime::ReminderKind};
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// The input that started the turn (Node `runUserPromptSubmitHooks` args).
pub(super) struct Prompt {
    pub text: String,
    pub attachments: Option<String>,
    /// Session message index where the input's messages start.
    pub at: usize,
}

pub(super) struct TurnHooks {
    pub hooks: Arc<Hooks>,
    /// `startup` / `resume` when SessionStart has not run in this process.
    pub session_start: Option<&'static str>,
    pub prompt: Option<Prompt>,
    /// `provider/model` of the turn (SessionStart `model`).
    pub model: String,
    pub stop_continuations: usize,
    pub tool_calls: usize,
}

fn mode(history: &RunContext) -> &'static str {
    history
        .permissions
        .as_ref()
        .map(|p| p.borrow().state.mode.as_str())
        .unwrap_or("build")
}

impl TurnHooks {
    /// Node turn start. `false`: UserPromptSubmit blocked the input; the
    /// engine already took it out of the history and the run ends.
    pub async fn start(
        &self,
        history: &mut RunContext,
        sink: &EventSink,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let mode = mode(history);
        let position = self
            .prompt
            .as_ref()
            .and_then(|p| p.at.checked_sub(history.state.offset))
            .unwrap_or(history.messages.len())
            .min(history.messages.len());
        if let Some(source) = self.session_start {
            let now = self.hooks.timestamp();
            let base = self.hooks.base(sink, mode, &now);
            let hook_input = input::session_start(&base, Some(&self.model), source);
            let result = self
                .hooks
                .run(hook_input, &[source.into()], sink, cancel)
                .await;
            let contexts = &result.additional_contexts;
            remind(history, sink, HookEvent::SessionStart, contexts, position).await?;
        }
        let Some(prompt) = &self.prompt else {
            return Ok(true);
        };
        let now = self.hooks.timestamp();
        let base = self.hooks.base(sink, mode, &now);
        let hook_input = input::user_prompt_submit(&base, &prompt.text, prompt.attachments.clone());
        let result = self.hooks.run(hook_input, &[], sink, cancel).await;
        if result.prevent_continuation {
            let (committed, receipt) = oneshot::channel();
            sink.send(Event::PromptBlocked { committed }).await?;
            super::agent_loop::durable(receipt, cancel).await?;
            return Ok(false);
        }
        let contexts = &result.additional_contexts;
        remind(
            history,
            sink,
            HookEvent::UserPromptSubmit,
            contexts,
            position,
        )
        .await?;
        Ok(true)
    }

    /// Node Stop hooks after a text-only step: `true` continues the turn with
    /// the hooks' context (at most three times a turn).
    pub async fn stop(
        &mut self,
        history: &mut RunContext,
        response: &str,
        sink: &EventSink,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let now = self.hooks.timestamp();
        let base = self.hooks.base(sink, mode(history), &now);
        let active = self.stop_continuations > 0;
        let hook_input = input::stop(&base, response, self.tool_calls, active);
        let result = self.hooks.run(hook_input, &[], sink, cancel).await;
        if !decision::should_continue(&result, self.stop_continuations) {
            return Ok(false);
        }
        self.stop_continuations += 1;
        let position = history.messages.len();
        remind(
            history,
            sink,
            HookEvent::Stop,
            &result.additional_contexts,
            position,
        )
        .await?;
        Ok(true)
    }
}

/// Node `injectHookAdditionalContextIntoMessageHistory` (`hook_context`).
async fn remind(
    history: &mut RunContext,
    sink: &EventSink,
    event: HookEvent,
    contexts: &[String],
    position: usize,
) -> Result<()> {
    let Some(body) = display::lifecycle_body(event, contexts) else {
        return Ok(());
    };
    let message = plan_mode::reminder_message(&body);
    let kind = ReminderKind::HookContext;
    history.insert_transient(position, TransientKind::Reminder(kind), message.clone());
    sink.send(Event::Reminder {
        anchor: history.state.offset + position,
        kind,
        message,
    })
    .await
}

/// Node `summarizeTurnAttachments` over V4 attachment refs as
/// `mapAttachmentRefsToTurnAttachments` turns them into turn attachments.
pub(super) fn attachments_summary(refs: &Value) -> Option<String> {
    let refs = refs.as_array()?;
    let items: Vec<(String, Option<String>)> = refs
        .iter()
        .map(|r| {
            let mime = r["mime"].as_str().unwrap_or("");
            let mime = mime.split(';').next().unwrap_or("").trim().to_lowercase();
            let kind = if mime.starts_with("video/") {
                "video"
            } else if mime.starts_with("image/") {
                "image"
            } else if mime == "application/pdf" {
                "pdf"
            } else {
                "file"
            };
            let reference = r["ref"].as_str().unwrap_or("");
            let uri = reference.split_once("://").is_some_and(|(scheme, _)| {
                scheme
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
                    && scheme
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
            });
            let path = if !uri {
                Some(reference.to_owned())
            } else if kind != "file" || r["bytes"].as_u64().is_some_and(|b| b <= 64 * 1024) {
                r["fileName"].as_str().map(str::to_owned)
            } else {
                None
            };
            (kind.to_owned(), path)
        })
        .collect();
    let attachments: Vec<input::Attachment> = items
        .iter()
        .map(|(kind, path)| input::Attachment {
            kind,
            path: path.as_deref(),
            inline: None,
        })
        .collect();
    input::attachments_summary(&attachments)
}
