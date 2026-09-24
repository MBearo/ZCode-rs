//! Plan mode inside a run: the two tools, the request reminders and the plan
//! file reference kept across compaction. State changes go to the engine.
use super::context::{RunContext, TransientKind};
use crate::domain::session_runtime::ReminderKind;
use crate::{
    contract::{Event, EventSink, ToolOutput, ToolPort},
    domain::{context::estimate, plan_mode},
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

async fn transition(
    sink: &EventSink,
    call_id: &str,
    enable: bool,
    cancel: &CancellationToken,
) -> Result<()> {
    let (reply, receipt) = oneshot::channel();
    sink.send(Event::PlanMode {
        call_id: call_id.into(),
        enable,
        reply,
    })
    .await?;
    let result = tokio::select! {biased;
        _=cancel.cancelled()=>bail!("Cancelled"),
        result=receipt=>result.context("Session owner stopped before the plan transition")?,
    };
    result.map_err(anyhow::Error::msg)
}

/// EnterPlanMode / ExitPlanMode after the permission gate (ExitPlanMode only
/// runs once the user approved the plan).
pub(super) async fn execute(
    tools: &dyn ToolPort,
    name: &str,
    args: &Value,
    call_id: &str,
    permissions: Option<&super::tool_permission::Permissions>,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    if name == plan_mode::ENTER {
        transition(sink, call_id, true, cancel).await?;
        return Ok(ToolOutput::text(plan_mode::enter_result().into()));
    }
    let plan = plan_mode::exit_plan(args).map_err(anyhow::Error::msg)?;
    if permissions.is_some_and(|p| !p.borrow().state.plan_enabled) {
        bail!(plan_mode::NOT_IN_PLAN);
    }
    // 与 Node 一致：计划文件写入失败静默忽略，退出照常进行。
    if let Err(error) = tools.write_plan_file(&sink.session_id, plan).await {
        tracing::debug!(target: "zcode::plan", error = %error, "Plan file was not written");
    }
    transition(sink, call_id, false, cancel).await?;
    Ok(ToolOutput::text(plan_mode::exit_result(plan)))
}

/// Node turn-loop reminders before a model request: the one-off exit reminder,
/// then the `runtime_mode` reminder while plan is on (skipped when continuing
/// an output-limited reply).
pub(super) async fn remind(history: &mut RunContext, sink: &EventSink) -> Result<()> {
    let Some(permissions) = history.permissions.clone() else {
        return Ok(());
    };
    let snapshot = permissions.borrow().clone();
    if !snapshot.plan_exit_pending {
        history.plan_exit_sent = false;
    } else if !history.plan_exit_sent {
        history.plan_exit_sent = true;
        let message = plan_mode::reminder_message(plan_mode::exit_reminder());
        add(history, sink, ReminderKind::PlanExit, message).await?;
    }
    let continuing = history
        .transient()
        .last()
        .is_some_and(|t| t.kind == TransientKind::Continue && t.position == history.messages.len());
    if !snapshot.state.plan_enabled || continuing {
        return Ok(());
    }
    let mut entries = vec![];
    let mut transient = history.transient().iter().peekable();
    for (index, message) in history.messages.iter().enumerate() {
        while let Some(t) = transient.next_if(|t| t.position <= index) {
            if t.kind == TransientKind::Reminder(ReminderKind::PlanRuntime) {
                entries.push(plan_mode::Entry::Reminder);
            }
        }
        entries.push(if plan_mode::is_real_user(message) {
            plan_mode::Entry::RealUser
        } else {
            plan_mode::Entry::Other
        });
    }
    for t in transient {
        if t.kind == TransientKind::Reminder(ReminderKind::PlanRuntime) {
            entries.push(plan_mode::Entry::Reminder);
        }
    }
    if let Some(body) = plan_mode::runtime_reminder(&entries) {
        let message = plan_mode::reminder_message(body);
        add(history, sink, ReminderKind::PlanRuntime, message).await?;
    }
    Ok(())
}

/// Adds a transient reminder at the end of the history and tells the engine
/// where it sits so later runs of this process keep it.
pub(super) async fn add(
    history: &mut RunContext,
    sink: &EventSink,
    kind: ReminderKind,
    message: Value,
) -> Result<()> {
    let tokens = estimate(std::slice::from_ref(&message));
    let position = history.add_transient(TransientKind::Reminder(kind), message.clone(), tokens);
    sink.send(Event::Reminder {
        anchor: history.state.offset + position,
        kind,
        message,
    })
    .await
}

/// Node `readApprovedPlanFileReferenceEntry`, read before compaction.
pub(super) async fn plan_reference(
    tools: &dyn ToolPort,
    sink: &EventSink,
) -> Result<Option<Value>> {
    Ok(tools
        .read_plan_file(&sink.session_id)
        .await?
        .map(|(path, content)| plan_mode::plan_file_reminder(&path, &content)))
}
