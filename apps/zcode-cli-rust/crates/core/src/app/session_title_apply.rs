//! The title sidecar's answer (Node `generateAndPersistSessionTitle`,
//! `persistGeneratedSessionTitle` and the goal summary title writes).
use super::Engine;
use crate::contract::ModelFailure;
use crate::domain::session_title as title;
use crate::domain::usage::{ErrorInfo, Fact, Tokens};
use anyhow::Result;
use serde_json::{Value, json};

/// The OpenAI-shaped usage of a completion as Node usage tokens.
fn tokens(usage: &Value) -> Tokens {
    let count = |v: &Value| v.as_u64().unwrap_or(0);
    Tokens {
        input: count(&usage["prompt_tokens"]),
        output: count(&usage["completion_tokens"]),
        reasoning: count(&usage["completion_tokens_details"]["reasoning_tokens"]),
        cache_write: count(&usage["prompt_tokens_details"]["cache_write_tokens"]),
        cache_read: count(&usage["prompt_tokens_details"]["cached_tokens"]),
    }
}

impl Engine {
    /// The sidecar's answer (Node `generateAndPersistSessionTitle` and
    /// `generateAndPersistGoalSummaryTitle`).
    pub(super) async fn title_done(
        &mut self,
        key: &str,
        result: std::result::Result<Value, ModelFailure>,
    ) -> Result<()> {
        self.cancel_auth(key);
        let Some(job) = self.auxiliary.remove(key).and_then(|a| a.title) else {
            return Ok(());
        };
        let mut fact = job.fact.clone();
        fact.completed_at = self.clock.now().max(job.started_at);
        let generated = match &result {
            Ok(out) => {
                fact.status = "completed";
                fact.tokens = tokens(&out["usage"]);
                fact.provider_total_tokens = out["usage"]["total_tokens"].as_u64();
                fact.tool_call_count = out["calls"].as_u64().unwrap_or(0);
                let finish = match (fact.tool_call_count, out["limit"] == true) {
                    (0, false) => "stop",
                    (0, true) => "length",
                    _ => "tool-calls",
                };
                fact.finish_reason = Some(finish.into());
                fact.raw_usage = Some(out["usage"].clone());
                (fact.tool_call_count == 0)
                    .then(|| title::clean(out["text"].as_str().unwrap_or("")))
                    .flatten()
            }
            Err(failure) => {
                fact.status = if failure.reason == "cancelled" {
                    "cancelled"
                } else {
                    "error"
                };
                fact.retryable = failure.retryable;
                fact.error = ErrorInfo {
                    kind: Some(failure.reason.into()),
                    code: Some(failure.code.into()),
                    message: Some(failure.message.into()),
                };
                None
            }
        };
        self.store.record_usage(Fact::Model(Box::new(fact))).await;
        let Some(generated) = generated else {
            // Node：没有可用标题时，目标摘要退回目标本身。
            if let Some(target) = &job.goal {
                let fallback = title::fallback_goal_title(&job.input);
                self.set_goal_summary(&job.session, target, fallback, true);
                self.persist_title(&job.session).await?;
            }
            return Ok(());
        };
        if job.session_title {
            self.set_generated_title(&job.session, &generated, job.message.as_deref());
        }
        if let Some(target) = &job.goal {
            self.set_goal_summary(&job.session, target, Some(generated), false);
        }
        self.persist_title(&job.session).await
    }

    async fn persist_title(&mut self, id: &str) -> Result<()> {
        if self.sessions.contains_key(id) {
            self.publish(id, vec![])?;
            self.persist(id, None).await?;
        }
        Ok(())
    }

    /// Node `persistGeneratedSessionTitle`: skipped after a custom title or an
    /// edit of the first prompt (`expectedTitleSources` guards the row).
    fn set_generated_title(&mut self, id: &str, generated: &str, message: Option<&str>) {
        let now = self.clock.now();
        let Some(s) = self.sessions.get_mut(id) else {
            return;
        };
        let edited = message.is_some_and(|m| {
            s.history
                .inputs
                .first()
                .and_then(|i| i.node_message.as_deref())
                .is_some_and(|first| first != m)
        });
        if edited
            || !matches!(
                s.title_source.as_str(),
                "default" | "first_input" | "generated"
            )
        {
            return;
        }
        let previous = std::mem::replace(&mut s.title, generated.into());
        s.title_source = "generated".into();
        s.revision += 1;
        s.node_generated_title(now, message);
        let mut payload =
            json!({"previousTitle": previous, "source": "generated", "title": generated});
        if let Some(message) = message {
            payload["messageID"] = message.into();
        }
        self.legacy_emit(id, None, vec![("session.titleUpdated", payload)]);
    }

    /// Node `persistGeneratedGoalSummaryTitle` / `persistFallbackGoalSummaryTitle`
    /// (`fallback` keeps an existing summary title).
    pub(super) fn set_goal_summary(
        &mut self,
        id: &str,
        target: &str,
        summary: Option<String>,
        fallback: bool,
    ) {
        let now = self.clock.now();
        let Some(summary) = summary else {
            return;
        };
        let Some(s) = self.sessions.get_mut(id) else {
            return;
        };
        let Some(goal) = s.goal.as_mut().filter(|g| g.target_id == target) else {
            return;
        };
        let existing = goal.summary_title.as_deref().map(str::trim).unwrap_or("");
        if (fallback && !existing.is_empty()) || goal.summary_title.as_deref() == Some(&summary) {
            return;
        }
        goal.summary_title = Some(summary);
        goal.updated_at = now;
        s.revision += 1;
        s.node_sync_goal(now);
    }
}
