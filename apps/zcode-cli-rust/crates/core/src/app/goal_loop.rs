use super::context::{RunContext, hidden_request};
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
    let (mut messages, _) = history.projection(prefix, 0);
    messages.push(json!({"role":"user","content":goal.prompt("goalVerify", None)}));
    // 修复：验证请求原先复用压缩的 hidden_summary，来源记为 compact；Node 的来源是
    // target_completion_verification，网络状态、session/debug 与用量都按它归属。
    let request = hidden_request(
        model,
        (messages, &[]),
        sink,
        "target_completion_verification",
        cancel,
    );
    // Node fail-open：工具调用、无效 JSON、请求失败都按通过记录（verifier 故障不能卡住目标）。
    let (verdict, usage) = match request.await {
        Ok(output) if output.calls.is_empty() => (
            Verdict::parse(output.message["content"].as_str().unwrap_or("")),
            output.usage,
        ),
        Ok(output) => (Verdict::tool_calls(), output.usage),
        Err(error) => (Verdict::request_failed(&error.to_string()), Value::Null),
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
