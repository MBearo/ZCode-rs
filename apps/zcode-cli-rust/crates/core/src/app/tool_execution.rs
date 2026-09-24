//! One step's tool calls (Node batch runner): permission gate, dispatch, result
//! commits in call order, and the turn stop a result may request.
use super::tool_hooks::{self, ToolCall};
use super::tool_permission::Gate;
use crate::{
    contract::{Event, EventSink, ToolOutput, ToolPort},
    domain::{hooks::output::RunResult, plan_mode},
};
use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(super) struct Scope<'a> {
    pub skills: &'a crate::domain::skills::SkillCatalog,
    pub profile: Option<&'a crate::domain::subagent::Profile>,
    pub profiles: &'a [crate::domain::subagent::Profile],
    pub selection: Option<crate::contract::ModelIdentity>,
    pub permissions: Option<&'a super::tool_permission::Permissions>,
    pub tool_filter: &'a crate::domain::session_runtime::ToolFilter,
    /// The run's hooks; `None` in subagents.
    pub hooks: Option<&'a std::sync::Arc<super::hook_runner::Hooks>>,
}
async fn execute(
    tools: &dyn ToolPort,
    context: &Scope<'_>,
    call: Value,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<(String, crate::contract::ToolOutput, bool)> {
    let Scope {
        skills,
        profile,
        profiles,
        selection,
        permissions,
        tool_filter,
        hooks,
    } = context;
    let (skills, profile, profiles, permissions, hooks) =
        (*skills, *profile, *profiles, *permissions, *hooks);
    let selection = selection.clone();
    if cancel.is_cancelled() {
        bail!("Cancelled");
    }
    sink.send(Event::ToolStart { call: call.clone() }).await?;
    let name = call["function"]["name"]
        .as_str()
        .context("Tool name missing")?;
    let id: String = call["id"].as_str().context("Tool id missing")?.into();
    let mut parsed =
        serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap_or("")).ok();
    let plan_tool = matches!(name, plan_mode::ENTER | plan_mode::EXIT);
    // 与 Node 一致：会话未注册的工具（allow/deny 过滤、子代理中的 plan 工具）按不存在处理，
    // ExitPlanMode 的输入校验在权限询问之前；两者都不进入 hook 与权限流程。
    let rejected = if !tool_filter.allows(name) || (plan_tool && profile.is_some()) {
        Some(format!("Tool not found: {name}"))
    } else if name == plan_mode::EXIT {
        parsed
            .as_ref()
            .and_then(|args| plan_mode::exit_plan(args).err())
            .map(str::to_owned)
    } else {
        None
    };
    if let Some(message) = rejected {
        // Node：未注册的工具与输入校验失败都不带前缀。
        let mut output = ToolOutput::text(message);
        output.failed = true;
        return Ok((id, output, true));
    }
    let mode = permissions
        .map(|p| p.borrow().state.mode.as_str())
        .unwrap_or("build");
    let mut pre = RunResult::default();
    if let (Some(h), Some(args)) = (hooks, &parsed) {
        let call = ToolCall {
            id: &id,
            name,
            args,
            mode,
        };
        pre = tool_hooks::pre_tool_use(h, tools, &call, sink, cancel).await;
        if let Some(reason) = tool_hooks::pre_tool_refusal(&pre) {
            let plan_on = permissions.is_some_and(|p| p.borrow().state.plan_enabled);
            let mut output =
                super::tool_permission::refusal(super::permissions::summarize(&reason));
            output.stop_turn = name == plan_mode::EXIT && plan_on;
            tool_hooks::append_contexts(&mut output, &pre.additional_contexts);
            return Ok((id, output, true));
        }
        if let Some(updated) = pre.updated_input.take() {
            parsed = Some(updated);
        }
    }
    if let (Some(permissions), Some(args)) = (permissions, &parsed) {
        let gate = hooks.map(|h| (h, &pre));
        match super::tool_permission::authorize(tools, permissions, &call, args, gate, sink, cancel)
            .await?
        {
            Gate::Stop(mut output) => {
                tool_hooks::append_contexts(&mut output, &pre.additional_contexts);
                return Ok((id, output, true));
            }
            Gate::Run(Some(modified)) => parsed = Some(modified),
            Gate::Run(None) => {}
        }
    }
    sink.send(Event::ToolExecuting { id: id.clone() }).await?;
    let result = if profile.is_some_and(|p| !p.allows(name)) {
        Err(anyhow::anyhow!(
            "Tool is not allowed by this subagent profile"
        ))
    } else {
        match parsed.clone() {
            Some(args)
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
                    &id,
                    selection,
                    sink,
                    cancel,
                )
                .await
            }
            Some(args) if plan_tool => {
                super::plan_tools::execute(tools, name, &args, &id, permissions, sink, cancel).await
            }
            Some(args) if name == "AskUserQuestion" => {
                super::question_tool::execute(&id, args, sink, cancel).await
            }
            Some(args) if matches!(name, "TodoRead" | "TodoWrite") => {
                super::todos::execute(name, &id, args, sink, cancel).await
            }
            Some(args) if name == "Skill" => {
                super::skills::execute(tools, skills, &args, cancel).await
            }
            Some(args) => tools.execute_scoped(name, &args, sink, cancel).await,
            None => Err(anyhow::anyhow!("Invalid tool JSON arguments")),
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
    let message = match &result {
        Err(error) => {
            crate::domain::js_string::sanitize_message(&error.to_string()).unwrap_or_default()
        }
        Ok(output) => output.content.clone(),
    };
    // Node createErrorResult：处理器失败包 <tool_use_error>，其余错误为规整后的消息。
    let mut content =
        result.unwrap_or_else(|error| ToolOutput::text(crate::contract::render_failure(&error)));
    if !failed && crate::domain::js_string::trim(&content.content).is_empty() {
        // Node serializeOutput：空结果给出占位，避免模型误读为缺失结果。
        content.content = format!("({name} completed with no output)");
    }
    let mut contexts = pre.additional_contexts;
    if let (Some(h), Some(args)) = (hooks, &parsed) {
        let call = ToolCall {
            id: &id,
            name,
            args,
            mode,
        };
        if failed {
            // Node：失败 hook 有上下文时连同 PreToolUse 上下文一起追加，否则只追加 PreToolUse 的。
            let cancelled = cancel.is_cancelled();
            let failure =
                tool_hooks::post_tool_use_failure(h, &call, &message, cancelled, sink, cancel)
                    .await;
            if !failure.additional_contexts.is_empty() {
                contexts.extend(failure.additional_contexts);
            }
        } else {
            let post = tool_hooks::post_tool_use(h, &call, &content, sink, cancel).await;
            contexts.extend(post.additional_contexts);
        }
    }
    tool_hooks::append_contexts(&mut content, &contexts);
    Ok((id, content, failed))
}

/// Runs one model step's calls. Consecutive concurrency-safe calls run together
/// and commit in call order; a result with `stop_turn` cancels every later call.
/// Returns whether the turn must end.
pub(super) async fn run_calls(
    tools: &dyn ToolPort,
    scope: &Scope<'_>,
    calls: Vec<Value>,
    history: &mut super::context::RunContext,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<bool> {
    let safe = |call: &Value| {
        tools.concurrent_safe_scoped(
            &sink.session_id,
            call["function"]["name"].as_str().unwrap_or(""),
        )
    };
    let mut stop = false;
    let mut calls = calls.into_iter().peekable();
    while let Some(first) = calls.next() {
        let mut group = vec![first];
        if safe(&group[0]) {
            while let Some(call) = calls.next_if(|call| safe(call)) {
                group.push(call);
            }
        }
        if stop {
            for call in group {
                sink.send(Event::ToolStart { call: call.clone() }).await?;
                let mut output = ToolOutput::text(plan_mode::TURN_STOP_CANCELLED.into());
                (output.failed, output.denied) = (true, true);
                commit(
                    history,
                    call["id"].as_str().unwrap_or(""),
                    output,
                    true,
                    sink,
                    cancel,
                )
                .await?;
            }
            continue;
        }
        let ids: Vec<String> = group
            .iter()
            .map(|call| call["id"].as_str().unwrap_or("").to_owned())
            .collect();
        // 只读工具并发执行，但按原始 call 顺序持久化结果；写/Shell 不跨越该屏障。
        let mut results = stream::iter(group)
            .map(|call| execute(tools, scope, call, sink, cancel))
            .buffered(4);
        while let Some(result) = results.next().await {
            let (id, output, failed) = result?;
            stop |= output.stop_turn;
            commit(history, &id, output, failed, sink, cancel).await?;
        }
        sink.send(Event::ToolBatch { ids }).await?;
    }
    Ok(stop)
}

async fn commit(
    history: &mut super::context::RunContext,
    id: &str,
    output: ToolOutput,
    failed: bool,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<()> {
    let content = output.content;
    history.push(
        json!({"role":"tool","tool_call_id":id,"content":content,"_zcode_tool_failed":failed}),
    );
    let (committed, receipt) = oneshot::channel();
    sink.send(Event::ToolDone {
        id: id.into(),
        result: content,
        display: output.display,
        failed,
        denied: output.denied,
        committed,
    })
    .await?;
    super::agent_loop::durable(receipt, cancel).await
}
