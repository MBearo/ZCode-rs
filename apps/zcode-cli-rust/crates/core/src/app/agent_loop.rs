use crate::contract::{ContextPort, Event, EventSink, ModelPort, ToolPort};
use anyhow::{Context, Result, bail};
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
        let reference = super::plan_tools::plan_reference(tools, sink).await?;
        return history
            .compact(model, sink, cancel, Some(&instructions), reference)
            .await;
    }
    let mut reactive_compacted = false;
    let mut continuations = 0;
    super::skills::initialize(tools, context, history, sink, cancel).await?;
    let skills = history.skills.clone().unwrap_or_default();
    let profile = history.agent_profile.clone();
    let mut definitions = tools.scoped_definitions(&sink.session_id, cancel).await?;
    if let Some(profile) = &profile {
        // 子代理不注册 plan 工具（Node subagent tool-policy）。
        definitions.retain(|d| {
            let name = d["function"]["name"].as_str().unwrap_or("");
            profile.allows(name) && !matches!(name, "EnterPlanMode" | "ExitPlanMode")
        });
    }
    if !skills.enabled {
        definitions.retain(|d| d["function"]["name"] != "Skill");
    }
    let tool_filter = history.tool_filter.clone();
    definitions.retain(|d| tool_filter.allows(d["function"]["name"].as_str().unwrap_or("")));
    // 与 Node 一致：禁用集合只从提供给模型的定义中移除；执行边界不据此拦截（D1）。
    hide(&mut definitions, &history.tool_disallowlist);
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
    let mut tool_tokens = definition_tokens(&definitions);
    let mut turns = 0;
    let permissions = history.permissions.clone();
    // 本 run 的请求归属副本；Engine 在引导输入提交时下发新 origin。
    let mut current = sink.clone();
    loop {
        let sink = &current;
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
        super::plan_tools::remind(history, sink).await?;
        let (mut messages, mut tokens) = history.projection(&prefix, tool_tokens, micro_threshold);
        if policy.automatic && tokens >= policy.threshold() {
            let reference = super::plan_tools::plan_reference(tools, sink).await?;
            history
                .compact(model, sink, cancel, None, reference)
                .await?;
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
                let reference = super::plan_tools::plan_reference(tools, sink).await?;
                history
                    .compact(model, sink, cancel, None, reference)
                    .await?;
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
        let scope = super::tool_execution::Scope {
            skills: &skills,
            profile: profile.as_ref(),
            profiles: &profiles,
            selection: identity.clone(),
            permissions: permissions.as_ref(),
            tool_filter: &tool_filter,
        };
        // 与 Node turnControl 一致：结果要求停轮时，其后的工具取消且本轮不再请求模型。
        if super::tool_execution::run_calls(tools, &scope, output.calls, history, sink, cancel)
            .await?
        {
            return Ok(());
        }
        let (committed, receipt) = oneshot::channel();
        sink.send(Event::StepBoundary { committed }).await?;
        let guide = tokio::select! {biased;
            _=cancel.cancelled()=>bail!("Cancelled"),
            result=receipt=>result.context("Session owner stopped before guide commit")?,
        };
        if let Some(guide) = guide {
            if let Some(origin) = guide.origin {
                current.origin = origin;
            }
            if !guide.tool_disallowlist.is_empty() {
                // automation 引导不会重新开轮，限制必须并入当前 loop（Node turn-guide-drain）。
                for name in guide.tool_disallowlist {
                    if !history.tool_disallowlist.contains(&name) {
                        history.tool_disallowlist.push(name);
                    }
                }
                hide(&mut definitions, &history.tool_disallowlist);
                tool_tokens = definition_tokens(&definitions);
            }
            for message in guide.messages {
                history.push(message);
            }
        } else if !has_tools
            && !super::goal_loop::advance(model, history, &prefix, sink, cancel).await?
        {
            return Ok(());
        }
    }
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
fn hide(definitions: &mut Vec<Value>, disallowed: &[String]) {
    if !disallowed.is_empty() {
        definitions.retain(|d| {
            !disallowed
                .iter()
                .any(|name| d["function"]["name"] == name.as_str())
        });
    }
}
fn definition_tokens(definitions: &[Value]) -> usize {
    definitions
        .iter()
        .map(|d| d.to_string().encode_utf16().count().div_ceil(3))
        .sum()
}
