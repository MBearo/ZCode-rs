//! Usage facts of runs and the usage queries (spec rust-m9-usage-logs §2).
use super::{Engine, auxiliary::Auxiliary, engine::Call};
use crate::contract::{Event, EventSink, Method, RuntimeError, ServerMsg};
use crate::domain::{
    legacy_params,
    usage::{self, Outcome},
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

impl Engine {
    /// Observes one event of the session's active run before the projection
    /// (which skips events after cancellation) sees it.
    pub(super) async fn observe_usage(&mut self, id: &str, event: &Event) {
        let now = self.clock.now();
        if let Some(s) = self.sessions.get_mut(id) {
            super::goal_events::account_nested(s, event, now);
        }
        // Node span id：`randomUUID().slice(0, 16)`，每个逻辑请求一个。
        let span = self.clock.id().chars().take(16).collect::<String>();
        let journal = self.sessions.get(id).and_then(|s| s.node.turn.as_ref());
        let ids = usage::StepIds {
            user: journal.map(|t| t.user.clone()),
            assistant: journal
                .and_then(|t| t.step.as_ref())
                .map(|s| s.assistant.clone()),
        };
        let meta = match event {
            Event::ToolStart { call } => {
                let name = call["function"]["name"].as_str().unwrap_or("");
                let hints = self.tools.mcp_annotations(id, name);
                usage::tool_meta(name, hints.as_ref())
            }
            _ => None,
        };
        let Some(active) = self.active.get_mut(id) else {
            return;
        };
        let cancelled = active.cancel.is_cancelled();
        let mut run_tokens = None;
        let run = &mut active.usage;
        let facts = match event {
            Event::ModelStatus(status) => run.on_status(status, &ids, || span, now),
            Event::Text { .. } => {
                run.on_text(now);
                vec![]
            }
            Event::ModelDone { message, .. } => run.on_model_done(message.as_ref(), &ids),
            Event::ToolStart { call } => run.on_tool_start(call, meta, now),
            Event::Permission { call, .. } => {
                run.on_permission(call["id"].as_str().unwrap_or(""));
                vec![]
            }
            Event::ToolExecuting { id } => {
                run.on_tool_executing(id, now);
                vec![]
            }
            Event::ToolDone {
                id,
                result,
                failed,
                denied,
                facts,
                ..
            } => {
                // Node 在工具结果之后追加 tool_internal 的 ModelComplete：用量计入本轮。
                if let Some(usage) = &facts.model_usage {
                    run.on_nested_usage(usage);
                }
                let end = usage::ToolEnd {
                    failed: *failed,
                    denied: *denied,
                    cancelled,
                    result,
                    exit_code: facts.exit_code,
                    truncated: facts.truncated,
                };
                run.on_tool_done(id, end, now)
            }
            Event::Finished {
                error,
                model_failure,
                cancelled: stopped,
            } => {
                let outcome = if *stopped || cancelled {
                    Outcome::Cancelled
                } else if error.is_some() {
                    Outcome::Failed
                } else {
                    Outcome::Completed
                };
                let failure = model_failure.as_ref().map(|f| (f.reason, f.code));
                run_tokens = Some(run.total_tokens());
                run.finish(outcome, failure, ids.user, now)
            }
            _ => vec![],
        };
        if let (Some(tokens), Some(s)) = (run_tokens, self.sessions.get_mut(id)) {
            s.runtime.run_tokens = tokens;
        }
        for fact in facts {
            self.store.record_usage(fact).await;
        }
    }

    /// A usage query: params failures are answered at once, the rest in the background.
    pub(super) async fn usage_query(
        &mut self,
        call: &Call,
        output: &super::engine::RuntimeOutput,
    ) -> anyhow::Result<()> {
        if let Err(error) = self.start_usage_query(call) {
            let reply = ServerMsg::Reply {
                token: call.token,
                result: Err(error),
            };
            output.send(vec![reply]).await?;
        }
        Ok(())
    }

    fn start_usage_query(&mut self, call: &Call) -> Result<(), RuntimeError> {
        let stats = matches!(call.method, Method::UsageStats | Method::LegacyUsageStats);
        let parsed = if stats {
            legacy_params::usage_stats(&call.params)
        } else {
            legacy_params::task_usage(&call.params)
        };
        let p = parsed.map_err(|error| RuntimeError::Params {
            message: error.message,
            data: error.data,
        })?;
        let id = format!("usage-query:{}", self.clock.id());
        self.auxiliary.insert(
            id.clone(),
            Auxiliary {
                token: call.token,
                cancel: CancellationToken::new(),
                operation: None,
                plugin_operation: None,
                title: None,
            },
        );
        let sink = EventSink {
            session_id: id.clone(),
            run_id: id,
            tx: self.events.clone(),
            origin: crate::contract::RequestOrigin::detached(self.clock.id()),
            request_auth: None,
        };
        let store = self.store.clone();
        let now = self.clock.now() as i64;
        tokio::spawn(async move {
            let result = if stats {
                app_snapshot(store.as_ref(), &p, now).await
            } else {
                let session = p["sessionId"].as_str().unwrap_or_default();
                store
                    .task_usage(session)
                    .await
                    .map(|rows| usage::task_usage(session, &rows))
            };
            let result = result.map_err(|error| {
                tracing::warn!(
                    target: "zcode::runtime",
                    event = "usage.query.failed",
                    error = %error,
                    "Usage query failed"
                );
                "Usage query failed".to_owned()
            });
            let _ = sink.send(Event::UsageDone { result }).await;
        });
        Ok(())
    }

    /// Replies to a usage query with its result.
    pub(super) fn usage_done(&mut self, id: &str, result: Result<Value, String>) {
        let job = self.auxiliary.remove(id).unwrap();
        self.outbox.push(ServerMsg::Reply {
            token: job.token,
            result: result.map_err(|message| RuntimeError::Fault {
                message,
                code: None,
            }),
        });
    }
}

/// Node `getUsageStats`.
async fn app_snapshot(
    store: &dyn crate::contract::SessionStore,
    p: &Value,
    until: i64,
) -> anyhow::Result<Value> {
    let range = p["range"].as_str().unwrap_or("30d");
    let time_zone = p["timeZone"].as_str().unwrap_or("UTC");
    let offset = usage::offset_ms(time_zone, until);
    let days = match range {
        "7d" => 7,
        _ => 30,
    };
    let since = if range == "all" {
        0
    } else {
        until - days * usage::DAY_MS
    };
    let rows = store.app_usage(since, until, offset).await?;
    let options = usage::SnapshotOptions {
        range,
        time_zone,
        tz_offset_ms: offset,
        generated_at: until as u64,
        since,
        until,
    };
    Ok(usage::snapshot(&rows, &options))
}
