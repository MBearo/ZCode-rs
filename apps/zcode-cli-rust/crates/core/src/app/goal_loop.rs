use super::context::{RunContext, hidden_summary};
use crate::{
    contract::{Event, EventSink, ModelPort},
    domain::goal::Verdict,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(super) async fn advance(
    model: &dyn ModelPort,
    history: &mut RunContext,
    prefix: &[Value],
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<bool> {
    if history.goal.is_none() {
        return Ok(false);
    }
    let (reply, receipt) = oneshot::channel();
    sink.send(Event::GoalStep { reply }).await?;
    let goal = tokio::select! {biased; _=cancel.cancelled()=>bail!("Cancelled"), result=receipt=>result.context("Goal verification start commit failed")?};
    let Some(goal) = goal else {
        return Ok(false);
    };
    let (mut messages, _) = history.projection(prefix, 0, usize::MAX);
    messages.push(json!({"role":"user","content":goal.prompt("goalVerify", None)}));
    let (verdict, usage) = match hidden_summary(model, messages, sink, cancel).await {
        Ok(output) if !output.output_limit && output.calls.is_empty() => (
            Verdict::parse(output.message["content"].as_str().unwrap_or("")),
            output.usage,
        ),
        Ok(output) => (
            Verdict::failed("The completion verifier returned tools or truncated output."),
            output.usage,
        ),
        Err(error) => (Verdict::failed(error.to_string()), Value::Null),
    };
    if cancel.is_cancelled() {
        bail!("Cancelled");
    }
    let (reply, receipt) = oneshot::channel();
    sink.send(Event::GoalVerdict {
        target_id: goal.target_id,
        verdict,
        usage,
        reply,
    })
    .await?;
    let next = tokio::select! {biased; _=cancel.cancelled()=>bail!("Cancelled"), result=receipt=>result.context("Goal verdict commit failed")?};
    if let Some((goal, message)) = next {
        history.goal = Some(goal);
        history.push(message);
        return Ok(true);
    }
    Ok(false)
}
