use super::compaction::{self, Outcome, Request, Trigger, compaction_failed};
use super::context::step_prefix;
use super::stream_recovery::retry_request;
use crate::contract::{ContextPort, Event, EventSink, ModelPort, ToolPort};
use crate::domain::compact;
use crate::domain::stream_recovery as recovering;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(super) async fn run(
    model: &dyn ModelPort,
    tools: &dyn ToolPort,
    context: &dyn ContextPort,
    history: &mut super::context::RunContext,
    mut turn: Option<super::turn_hooks::TurnHooks>,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<()> {
    // Node：SessionStart 在手动压缩前运行；UserPromptSubmit 阻止输入时本轮直接结束。
    if let Some(turn) = &mut turn
        && !turn.start(history, sink, cancel).await?
    {
        return Ok(());
    }
    let manual = history.manual.take();
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
    tools.model_definitions(&mut definitions, &model.format_properties()["inputFormat"]);
    // Node getTools(model)：只有声明 supportsNativeWebSearch 的模型提供 WebSearch。
    if !model.supports_native_web_search() {
        definitions.retain(|d| d["function"]["name"] != "WebSearch");
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
    if let Some(instructions) = manual {
        // 手动压缩的摘要请求带与 agent step 相同的前缀与工具（Node compactActiveConversation）。
        let bound = model.bind();
        let model = bound.as_deref().unwrap_or(model);
        let prefix =
            step_prefix(model, context, history, (&skills, profile.as_ref()), cancel).await?;
        let reminder = super::plan_tools::plan_reference(tools, sink).await?;
        let request = Request {
            prefix: &prefix,
            tools: &definitions,
            reminder,
            port: tools,
        };
        let trigger = Trigger::Manual(&instructions);
        history
            .compact(model, sink, cancel, trigger, request)
            .await?;
        return Ok(());
    }
    let mut tool_tokens = definition_tokens(&definitions);
    let mut turns = 0;
    let permissions = history.permissions.clone();
    // 本 run 的请求归属副本；Engine 在引导输入提交时下发新 origin。
    let mut current = sink.clone();
    // 断流恢复：本 run 已恢复次数，以及下一次请求要带的 streamRecovery。
    let mut recoveries = 0;
    let mut recovery: Option<Arc<Value>> = None;
    // 本轮的重复调用与调用预算提醒（Node model_anomaly，按轮计数）。
    let mut anomalies = crate::domain::model_anomaly::TurnAnomalies::default();
    // 本轮的快速回填计数（Node compactTracking）。
    let mut refill = compact::RapidRefill::default();
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
        let prefix =
            step_prefix(model, context, history, (&skills, profile.as_ref()), cancel).await?;
        let identity = model.identity();
        super::plan_tools::remind(history, sink).await?;
        let (mut messages, mut tokens) = history.projection(&prefix, tool_tokens);
        if policy.automatic
            && tokens >= policy.threshold()
            && history.compact_failures < compact::MAX_CONSECUTIVE_FAILURES
            && history.can_compact()
        {
            let (count, blocked) = refill.evaluate();
            if blocked {
                bail!(compact::rapid_refill_error());
            }
            let reminder = super::plan_tools::plan_reference(tools, sink).await?;
            let request = Request {
                prefix: &prefix,
                tools: &definitions,
                reminder,
                port: tools,
            };
            match history
                .compact(model, sink, cancel, Trigger::Auto, request)
                .await
            {
                Ok(Outcome::Compacted) => {
                    refill.compacted(count);
                    history.compact_failures = 0;
                    (messages, tokens) = history.projection(&prefix, tool_tokens);
                }
                Ok(Outcome::Skipped) => {}
                // Node：自动压缩失败只计数，本次请求照常发送。
                Err(error) if compaction_failed(&error) => history.compact_failures += 1,
                Err(error) => return Err(error),
            }
        }
        sink.send(Event::ContextUsage(json!({"usedTokens":tokens,"maxTokens":policy.window,"autoCompactThresholdTokens":if policy.automatic {Some(policy.threshold())} else {None}}))).await?;
        let recovering;
        let request = match recovery.take() {
            Some(status) => {
                let mut origin = (*sink.origin).clone();
                origin.stream_recovery = Some(status);
                recovering = EventSink {
                    origin: Arc::new(origin),
                    ..sink.clone()
                };
                &recovering
            }
            None => sink,
        };
        let output = match model
            .complete(messages, &definitions, request, cancel)
            .await
        {
            Err(failure)
                if policy.automatic
                    && failure.reason == "context_exceeded"
                    && !failure.output_committed
                    && !reactive_compacted =>
            {
                // Node：每个模型步骤最多一次反应式压缩；不可压缩或失败时上报原失败。
                reactive_compacted = true;
                let (count, blocked) = refill.evaluate();
                if blocked {
                    bail!(compact::rapid_refill_error());
                }
                if !history.can_compact() {
                    return Err(failure.into());
                }
                let reminder = super::plan_tools::plan_reference(tools, sink).await?;
                let request = Request {
                    prefix: &prefix,
                    tools: &definitions,
                    reminder,
                    port: tools,
                };
                match history
                    .compact(
                        model,
                        sink,
                        cancel,
                        Trigger::Reactive(compaction::overflow(&failure)),
                        request,
                    )
                    .await
                {
                    Ok(Outcome::Compacted) => {
                        refill.compacted(count);
                        history.compact_failures = 0;
                        continue;
                    }
                    Ok(Outcome::Skipped) => return Err(failure.into()),
                    Err(error) if compaction_failed(&error) => {
                        history.compact_failures += 1;
                        return Err(failure.into());
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(failure)
                if !cancel.is_cancelled() && recovering::recoverable(&failure, recoveries) =>
            {
                // 已流出可见输出后断流：丢弃这段输出，用同一份历史重发（Node core recovery）。
                recoveries += 1;
                let max = recovering::MAX_RETRIES;
                recovery = Some(retry_request(sink, cancel, (recoveries, max), 0).await?);
                continue;
            }
            Err(failure) if !cancel.is_cancelled() => {
                let provider = identity.as_ref().map_or("", |id| id.provider_id.as_str());
                let busy =
                    recovering::busy_delay(&failure, provider, history.returning, recoveries);
                if let Some(delay) = busy {
                    // Node：非首轮的 Start Plan busy 在无输出时等待 1s / 2s 后重发。
                    recoveries += 1;
                    let max = recovering::BUSY_MAX_RETRIES;
                    recovery = Some(retry_request(sink, cancel, (recoveries, max), delay).await?);
                    continue;
                }
                if recoveries > 0 && recovering::start_plan_busy(&failure) {
                    return Err(recovering::busy_exhausted(&failure).into());
                }
                return Err(failure.into());
            }
            result => result?,
        };
        history.anchor_usage(&output.usage);
        let response = output.message["content"].as_str().unwrap_or("").to_owned();
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
        if let Some(turn) = &mut turn {
            turn.tool_calls += output.calls.len();
        }
        let scope = super::tool_execution::Scope {
            skills: &skills,
            profile: profile.as_ref(),
            profiles: &profiles,
            selection: identity.clone(),
            permissions: permissions.as_ref(),
            tool_filter: &tool_filter,
            hooks: turn.as_ref().map(|t| &t.hooks),
            model,
        };
        let called = output.calls.clone();
        // 与 Node turnControl 一致：结果要求停轮时，其后的工具取消且本轮不再请求模型。
        if super::tool_execution::run_calls(tools, &scope, output.calls, history, sink, cancel)
            .await?
        {
            return Ok(());
        }
        // Node recordCompletedToolBatch：工具批次完成后重新允许反应式压缩，并计一个工具轮。
        reactive_compacted = false;
        refill.tool_batch();
        for body in anomalies.observe(&called, &history.anomaly_guard) {
            let message = crate::domain::plan_mode::reminder_message(&body);
            let kind = crate::domain::session_runtime::ReminderKind::ModelAnomaly;
            super::plan_tools::add(history, sink, kind, message).await?;
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
        } else if !has_tools {
            // Node：纯文本步骤收口时先跑 Stop hooks，要求续跑则带上下文继续同一轮。
            if let Some(turn) = &mut turn
                && turn.stop(history, &response, sink, cancel).await?
            {
                continue;
            }
            if !super::goal_loop::advance(model, history, &prefix, sink, cancel).await? {
                return Ok(());
            }
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
