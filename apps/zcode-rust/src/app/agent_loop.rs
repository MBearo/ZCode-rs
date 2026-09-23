use crate::contract::{ContextPort, Event, EventSink, ModelPort, ToolPort};
use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(super) async fn run(
    model: &dyn ModelPort,
    tools: &dyn ToolPort,
    context: &dyn ContextPort,
    history: &mut super::context::RunContext,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<()> {
    if let Some(instructions) = history.manual.take() {
        return history
            .compact(model, sink, cancel, Some(&instructions))
            .await;
    }
    let mut reactive_compacted = false;
    let mut continuations = 0;
    super::skills::initialize(tools, context, history, sink, cancel).await?;
    let skills = history.skills.clone().unwrap_or_default();
    let profile = history.agent_profile.clone();
    let mut definitions = tools.scoped_definitions(&sink.session_id, cancel).await?;
    if let Some(profile) = &profile {
        definitions.retain(|d| profile.allows(d["function"]["name"].as_str().unwrap_or("")));
    }
    if !skills.enabled {
        definitions.retain(|d| d["function"]["name"] != "Skill");
    }
    let profiles = if definitions.iter().any(|d| d["function"]["name"] == "Agent") {
        tools.agent_profiles(cancel).await?
    } else {
        vec![]
    };
    if let Some(agent) = definitions
        .iter_mut()
        .find(|d| d["function"]["name"] == "Agent")
    {
        let descriptions = profiles
            .iter()
            .map(|p| format!("- {}: {}", p.name, p.description))
            .collect::<Vec<_>>()
            .join("\n");
        let base = agent["function"]["description"].as_str().unwrap_or("");
        agent["function"]["description"] =
            format!("{base}\n\nCurrent profile catalog (authoritative):\n{descriptions}").into();
    }
    let tool_tokens = definitions
        .iter()
        .map(|d| d.to_string().encode_utf16().count().div_ceil(3))
        .sum();
    let mut turns = 0;
    loop {
        if profile
            .as_ref()
            .and_then(|p| p.max_turns)
            .is_some_and(|max| turns >= max)
        {
            bail!("Subagent maxTurns reached");
        }
        turns += 1;
        // 每步冻结同一 Model，同时用于预算和请求；运行中切换不能混用旧预算和新端点。
        let bound = model.bind();
        let model = bound.as_deref().unwrap_or(model);
        let policy = model.context_policy();
        if cancel.is_cancelled() {
            bail!("Cancelled");
        }
        if continuations == 0
            && definitions
                .iter()
                .any(|d| d["function"]["name"] == "TodoWrite")
            && crate::domain::todo::should_remind(&history.messages)
        {
            let (reply, receipt) = oneshot::channel();
            sink.send(Event::TodoReminder { reply }).await?;
            let message = tokio::select! {biased;
                _=cancel.cancelled()=>bail!("Cancelled"),
                message=receipt=>message.context("Todo reminder commit failed")?,
            };
            history.push(message);
        }
        let instructions = if profile
            .as_ref()
            .is_some_and(|p| p.inject_agents_md == Some(false))
        {
            vec![]
        } else {
            context.instructions(cancel).await?
        };
        let identity = model.identity();
        let mut prefix = crate::domain::prompt::prefix(
            history.prompt_snapshot.as_ref().unwrap(),
            &instructions,
            identity
                .as_ref()
                .map(|id| (id.provider_id.as_str(), id.model_id.as_str())),
            context.desktop(),
        );
        if let Some(reminder) = skills.reminder() {
            prefix.push(reminder);
        }
        if let Some(profile) = &profile {
            prefix.push(json!({"role":"system","content":profile.system_prompt}));
        }
        if let Some(goal) = history.goal.as_ref().filter(|g| g.active()) {
            prefix.push(json!({"role":"user","content":format!("<system-reminder>\n{}\n</system-reminder>",goal.prompt("goalState", None))}));
        }
        let micro_threshold = if policy.automatic {
            policy.micro_threshold()
        } else {
            usize::MAX
        };
        let (mut messages, mut tokens) = history.projection(&prefix, tool_tokens, micro_threshold);
        if policy.automatic && tokens >= policy.threshold() {
            history.compact(model, sink, cancel, None).await?;
            (messages, tokens) = history.projection(&prefix, tool_tokens, micro_threshold);
            if tokens >= policy.threshold() {
                bail!("Context remains above budget after compaction; narrow the input");
            }
        }
        sink.send(Event::ContextUsage(json!({"usedTokens":tokens,"maxTokens":policy.window,"autoCompactThresholdTokens":if policy.automatic {Some(policy.threshold())} else {None}}))).await?;
        let output = match model.complete(messages, &definitions, sink, cancel).await {
            Err(failure)
                if policy.automatic
                    && failure.reason == "context_exceeded"
                    && !failure.output_committed
                    && !reactive_compacted
                    && crate::domain::context::split_for_summary(&history.messages, false)
                        .is_some() =>
            {
                reactive_compacted = true;
                history.compact(model, sink, cancel, None).await?;
                continue;
            }
            result => result?,
        };
        history.anchor_usage(&output.usage);
        let persist = !output.output_limit
            || output.message.as_object().is_some_and(|m| {
                ["content", "reasoning_content"].iter().any(|k| {
                    m.get(*k)
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty())
                }) || ["_zcode_responses_reasoning", "_zcode_anthropic_thinking"]
                    .iter()
                    .any(|k| {
                        m.get(*k)
                            .and_then(Value::as_array)
                            .is_some_and(|a| !a.is_empty())
                    })
            });
        if persist {
            history.push(output.message.clone());
        }
        let (committed, receipt) = oneshot::channel();
        sink.send(Event::ModelDone {
            stable: !output.output_limit && output.calls.is_empty(),
            message: persist.then_some(output.message),
            usage: output.usage,
            committed,
        })
        .await?;
        durable(receipt, cancel).await?;
        if output.output_limit {
            if continuations == 3 {
                return Err(crate::contract::ModelFailure::new(
                    "model_output_limit_exceeded",
                    true,
                )
                .into());
            }
            continuations += 1;
            history.continue_output();
            reactive_compacted = false;
            continue;
        }
        continuations = 0;
        let has_tools = !output.calls.is_empty();
        let mut calls = output.calls.into_iter().peekable();
        while let Some(first) = calls.next() {
            let mut group = vec![first];
            if safe(tools, &sink.session_id, &group[0]) {
                while calls
                    .peek()
                    .is_some_and(|call| safe(tools, &sink.session_id, call))
                {
                    group.push(calls.next().unwrap());
                }
            }
            // 只读工具并发执行，但按原始 call 顺序持久化结果；写/Shell 不跨越该屏障。
            let mut results = stream::iter(group)
                .map(|call| {
                    execute(
                        tools,
                        ExecutionContext {
                            skills: &skills,
                            profile: profile.as_ref(),
                            profiles: &profiles,
                            selection: identity.clone(),
                        },
                        call,
                        sink,
                        cancel,
                    )
                })
                .buffered(4);
            while let Some(result) = results.next().await {
                let (id, output, failed) = result?;
                let content = output.content;
                history.push(json!({"role":"tool","tool_call_id":id,"content":content,"_zcode_tool_failed":failed}));
                let (committed, receipt) = oneshot::channel();
                sink.send(Event::ToolDone {
                    id,
                    result: content,
                    display: output.display,
                    failed,
                    committed,
                })
                .await?;
                durable(receipt, cancel).await?;
            }
        }
        let (committed, receipt) = oneshot::channel();
        sink.send(Event::StepBoundary { committed }).await?;
        let guide = tokio::select! {biased;
            _=cancel.cancelled()=>bail!("Cancelled"),
            result=receipt=>result.context("Session owner stopped before guide commit")?,
        };
        if let Some(messages) = guide {
            for message in messages {
                history.push(message);
            }
        } else if !has_tools
            && !super::goal_loop::advance(model, history, &prefix, sink, cancel).await?
        {
            return Ok(());
        }
    }
}
fn safe(tools: &dyn ToolPort, session: &str, call: &Value) -> bool {
    let name = call["function"]["name"].as_str().unwrap_or("");
    tools.concurrent_safe_scoped(session, name) && !tools.requires_permission(name)
}
pub(super) async fn durable(
    receipt: oneshot::Receiver<()>,
    cancel: &CancellationToken,
) -> Result<()> {
    tokio::select! {biased;
        _=cancel.cancelled()=>bail!("Cancelled"),
        result=receipt=>result.context("Session owner stopped before durable commit"),
    }
}
struct ExecutionContext<'a> {
    skills: &'a crate::domain::skills::SkillCatalog,
    profile: Option<&'a crate::domain::subagent::Profile>,
    profiles: &'a [crate::domain::subagent::Profile],
    selection: Option<crate::contract::ModelIdentity>,
}
async fn execute(
    tools: &dyn ToolPort,
    context: ExecutionContext<'_>,
    call: Value,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<(String, crate::contract::ToolOutput, bool)> {
    let ExecutionContext {
        skills,
        profile,
        profiles,
        selection,
    } = context;
    if cancel.is_cancelled() {
        bail!("Cancelled");
    }
    sink.send(Event::ToolStart { call: call.clone() }).await?;
    let name = call["function"]["name"]
        .as_str()
        .context("Tool name missing")?;
    let allowed = if tools.requires_permission(name) {
        let (reply, receipt) = oneshot::channel();
        sink.send(Event::Permission {
            call: call.clone(),
            reply,
        })
        .await?;
        tokio::select! {biased; _=cancel.cancelled()=>bail!("Cancelled"), result=receipt=>result.unwrap_or(false)}
    } else {
        true
    };
    let result = if profile.is_some_and(|p| !p.allows(name)) {
        Err(anyhow::anyhow!(
            "Tool is not allowed by this subagent profile"
        ))
    } else if !allowed {
        Err(anyhow::anyhow!("Permission denied"))
    } else {
        match serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap_or("")) {
            Ok(args)
                if matches!(name, "Agent" | "Task" | "SendMessage")
                    || matches!(name, "TaskOutput" | "TaskStop")
                        && args["task_id"]
                            .as_str()
                            .is_some_and(|id| id.starts_with("agent_")) =>
            {
                super::subagent_tools::execute(
                    (tools, profiles, skills),
                    name,
                    &args,
                    call["id"].as_str().unwrap(),
                    selection,
                    sink,
                    cancel,
                )
                .await
            }
            Ok(args) if name == "AskUserQuestion" => {
                super::question_tool::execute(call["id"].as_str().unwrap(), args, sink, cancel)
                    .await
            }
            Ok(args) if matches!(name, "TodoRead" | "TodoWrite") => {
                super::todos::execute(name, call["id"].as_str().unwrap(), args, sink, cancel).await
            }
            Ok(args) if name == "Skill" => {
                super::skills::execute(tools, skills, &args, cancel).await
            }
            Ok(args) => tools.execute_scoped(name, &args, sink, cancel).await,
            Err(_) => Err(anyhow::anyhow!("Invalid tool JSON arguments")),
        }
    };
    if let Err(error) = &result
        && error.is::<crate::contract::ProcessCleanupFailure>()
    {
        // 进程未确认回收时不能包装成普通工具失败再发请求；由 owner 终止本 runtime。
        sink.send(Event::ToolCleanupFailed(format!("{error:#}")))
            .await?;
        return Err(result.err().unwrap());
    }
    let failed = result.as_ref().map_or(true, |output| output.failed);
    let content = result
        .unwrap_or_else(|error| crate::contract::ToolOutput::text(format!("Tool failed: {error}")));
    Ok((
        call["id"].as_str().context("Tool id missing")?.into(),
        content,
        failed,
    ))
}
