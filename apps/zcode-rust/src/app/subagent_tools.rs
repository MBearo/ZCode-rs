use crate::contract::{ChildHandle, Event, EventSink, ModelIdentity, ToolOutput};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(super) async fn execute(
    preparation: (
        &dyn crate::contract::ToolPort,
        &[crate::domain::subagent::Profile],
        &crate::domain::skills::SkillCatalog,
    ),
    name: &str,
    args: &Value,
    call: &str,
    selection: Option<ModelIdentity>,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    let (tools, profiles, skills) = preparation;
    let launch = matches!(name, "Agent" | "Task");
    let mut profile = if launch {
        for key in ["description", "prompt"] {
            ensure!(
                args[key]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty() && s.len() <= 64000),
                "Invalid Agent {key}"
            );
        }
        ensure!(
            args.get("run_in_background").is_none_or(Value::is_boolean),
            "Invalid Agent background flag"
        );
        let agent = args["subagent_type"].as_str().unwrap_or("general-purpose");
        Some(Box::new(
            profiles
                .iter()
                .find(|p| p.name == agent)
                .context("agent_unknown_type")?
                .clone(),
        ))
    } else {
        None
    };
    if let Some(profile) = &mut profile {
        for name in &profile.skills {
            let output =
                super::skills::execute(tools, skills, &json!({"skill":name}), cancel).await?;
            profile.system_prompt.push_str("\n\n");
            profile.system_prompt.push_str(&output.content);
        }
        if let Some(memory) = tools.agent_memory(profile, cancel).await? {
            profile.system_prompt.push_str("\n\n");
            profile.system_prompt.push_str(&memory);
            if let Some(tools) = &mut profile.tools {
                for name in ["Write", "Edit"] {
                    if !tools.iter().any(|t| t == name) {
                        tools.push(name.into());
                    }
                }
            }
        }
    }
    if name == "SendMessage" {
        ensure!(
            args.as_object().is_some_and(|o| o.len() == 3),
            "Invalid SendMessage arguments"
        );
        for (key, max) in [("to", 200), ("summary", 200), ("message", 20000)] {
            ensure!(
                args[key]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty() && s.encode_utf16().count() <= max),
                "Invalid SendMessage {key}"
            );
        }
    }
    let (reply, receipt) = oneshot::channel();
    sink.send(Event::Subagent {
        name: name.into(),
        args: args.clone(),
        call_id: call.into(),
        profile,
        selection,
        reply,
    })
    .await?;
    // owner 持有子进程清理；前台 Agent 取消后仍等真实 child 终态，不能丢弃清理任务。
    let mut handle = receipt
        .await
        .context("Subagent owner stopped")?
        .map_err(anyhow::Error::msg)?;
    if launch && !handle.task.background || name == "TaskStop" {
        wait(&mut handle).await?;
    } else if name == "TaskOutput" && args["block"] != false && args["block"] != "false" {
        let timeout = args["timeout"].as_f64().unwrap_or(30000.0);
        ensure!(
            timeout.is_finite() && (0.0..=600000.0).contains(&timeout),
            "Invalid TaskOutput timeout"
        );
        tokio::select! {biased;_=cancel.cancelled()=>anyhow::bail!("Cancelled"),_=tokio::time::sleep(std::time::Duration::from_secs_f64(timeout/1000.0))=>(),result=wait(&mut handle)=>result?};
    }
    if name == "SendMessage" {
        return Ok(ToolOutput::text(json!({"status":"success","messageId":handle.message_id,"agentId":handle.task.id,"taskId":handle.task.id,"delivery":handle.delivery,"outputFile":handle.task.output_file}).to_string()));
    }
    if name == "TaskOutput" {
        return Ok(ToolOutput::text(
            handle
                .task
                .task_output(args["block"] != false && args["block"] != "false")
                .to_string(),
        ));
    }
    if name == "TaskStop" {
        return Ok(ToolOutput::text(json!({"message":format!("Successfully stopped task: {}",handle.task.id),"task_id":handle.task.id,"task_type":"local_agent"}).to_string()));
    }
    let mut result = ToolOutput::text(handle.task.content());
    result.failed = !handle.task.running() && handle.task.status != "completed";
    Ok(result)
}
async fn wait(handle: &mut ChildHandle) -> Result<()> {
    while handle.task.running() {
        handle
            .updates
            .changed()
            .await
            .context("Subagent owner stopped before terminal commit")?;
        handle.task = handle.updates.borrow_and_update().clone();
    }
    Ok(())
}
