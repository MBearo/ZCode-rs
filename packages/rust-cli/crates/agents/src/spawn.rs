//! 跑一个子代理：建子会话、组装子循环、等它结束、整理结果（docs/specs/rust-cli/agents.md §4、§5）。

use std::sync::Arc;

use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::env_shell;
use rust_cli_kernel_api::ids::{AgentId, SessionId, TraceId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::metering::CallStatus;
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{QueryError, QueryRequest, QueryStop, ToolRegistry};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::run_ledger::RunLedger;
use rust_cli_kernel_api::store::entry_kinds;
use rust_cli_kernel_api::store::{Fact, SessionKind, SessionMeta, StoreError};
use tokio_util::sync::CancellationToken;

use crate::Core;
use crate::child::{ChildSink, Parent, tool_use_blocks};
use crate::context::SubagentContext;
use crate::live::{AgentState, LiveAgent};
use crate::report::{self, Ending, Report};
use crate::routing;
use crate::runs::{RunInfo, Tracked};
use crate::toolset::{FilteredTools, ToolRule};

/// 子代理的模型：定义里的模型 ID（`inherit` 与缺省继承父运行），`effort` 参数优先于定义。provider 与账号连接
/// （Team Plan 的团队项目，工作线 F49）都跟父运行：同一个团队项目、换一个模型。
pub(crate) fn selection(
    parent: &ModelSelection,
    model: Option<&str>,
    effort: Option<String>,
) -> ModelSelection {
    let mut selection = match model.filter(|model| *model != "inherit") {
        Some(model) => ModelSelection {
            connection: parent.connection.clone(),
            ..ModelSelection::new(parent.provider.clone(), model.to_owned())
        },
        None => parent.clone(),
    };
    if effort.is_some() {
        selection.reasoning = effort;
    }
    selection
}

/// 子代理的模式：定义里的 `permissionMode` 覆盖父运行；bypassPermissions 只在父也是 bypass 时生效
/// （授权检查）。
pub(crate) fn mode(parent: PermissionMode, declared: Option<PermissionMode>) -> PermissionMode {
    match declared {
        Some(PermissionMode::BypassPermissions) if parent != PermissionMode::BypassPermissions => {
            parent
        }
        Some(mode) => mode,
        None => parent,
    }
}

fn user(core: &Core, text: &str, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id: core.ports.ids.message(),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin,
    })
}

/// 提交到子会话（运行开头的输入）：同 `ChildSink::commit`，提交核心是这次运行的那一份 `RunLedger`（node-store.md §4，
/// 修复线 F6），之后交给 `ChildSink`。
async fn commit(
    core: &Core,
    session: &SessionId,
    ledger: &mut RunLedger,
    facts: Vec<Fact>,
) -> Result<(), StoreError> {
    let batch = ledger.batch(None, facts, core.ports.ids.as_ref());
    let turn = batch.turn.clone();
    core.ports.store.commit(session, batch).await?;
    ledger.committed(turn);
    Ok(())
}

/// 一次运行的开头：历史（最后一条是这次运行的输入）与这次运行的提交核心（带着子会话的当前轮）。
pub(crate) struct Opening {
    pub(crate) messages: Vec<Message>,
    pub(crate) ledger: RunLedger,
}

/// 建子会话并提交模型、模式与第一条 user 消息；返回登记好的子代理（状态为运行中）。
pub(crate) async fn create(
    core: &Core,
    mut agent: LiveAgent,
    parent: &RunInfo,
    prompt: &str,
) -> Result<(LiveAgent, Opening), StoreError> {
    let meta = SessionMeta {
        id: agent.child.clone(),
        kind: SessionKind::Subagent,
        parent: Some(parent.session.clone()),
        workspace: agent.workspace.clone(),
        title: agent.description.clone(),
        trace: TraceId::new(agent.root.as_str()),
        created_at_ms: core.ports.clock.now_ms(),
    };
    core.ports.store.create(meta).await?;
    inherit_directory(core, &agent).await;
    let first = user(core, prompt, UserOrigin::Human);
    let mut facts = vec![
        Fact::ModelSelected {
            selection: agent.selection.clone(),
        },
        Fact::ModeChanged { mode: agent.mode },
    ];
    // fork：子会话记下父会话与切点（溯源用，node-store.md §4.8.1）；继承的前缀不落库。
    if let Some(prefix) = &agent.fork_prefix {
        facts.push(Fact::Entry {
            kind: entry_kinds::FORK_PARENT.to_owned(),
            id: agent.agent.as_str().to_owned(),
            data: serde_json::json!({
                "parentSession": parent.session.as_str(),
                "throughMessageId": prefix
                    .iter()
                    .rev()
                    .nth(2)
                    .map(|message| message.id().as_str().to_owned()),
                "toolUseId": agent.tool_use.as_ref().map(|id| id.as_str().to_owned()),
            }),
        });
    }
    facts.push(Fact::MessageAppended {
        message: first.clone(),
    });
    let mut ledger = RunLedger::new(None);
    commit(core, &agent.child, &mut ledger, facts).await?;
    agent.state = AgentState::Running {
        abort: Abort::new(),
        mailbox: Vec::new(),
    };
    core.live.insert(agent.clone());
    let messages = with_prefix(&agent, vec![first]);
    Ok((agent, Opening { messages, ledger }))
}

/// 子会话的当前目录取父会话此刻的当前目录（派生时复制父会话的 cwd，之后主线程的
/// `cd` 传不过去，tools-files.md §3.3）。缺陷修复（F19）：以前子会话没有自己的 shell cwd，文件工具、判决链与 Bash 都
/// 落回工作区根——父会话 `cd sub` 之后，子代理的 `x.txt` 是根下的 x.txt。执行环境不支持时照旧（工作区根）。
async fn inherit_directory(core: &Core, agent: &LiveAgent) {
    let machine = core.ports.machines.primary_machine();
    let shell = machine.shell().as_ref();
    let cwd = env_shell::current_dir(shell, &agent.workspace, &agent.parent).await;
    if let Err(error) = shell.set_shell_cwd(&agent.child, cwd).await {
        tracing::debug!(%error, "the subagent starts in the workspace root");
    }
}

/// 续跑已结束或停放的子代理：在子会话里追加 user 消息（SendMessage 是用户来源；通知是 meta，
/// 来源 `task-notification`，agents.md §5.1），返回完整历史。
pub(crate) async fn resume(
    core: &Core,
    agent: &LiveAgent,
    message: &str,
    origin: UserOrigin,
) -> Result<Opening, StoreError> {
    let loaded = core.ports.store.load(&agent.child).await?;
    let mut messages = with_prefix(agent, loaded.messages);
    let next = user(core, message, origin);
    let mut ledger = RunLedger::new(loaded.turn);
    commit(
        core,
        &agent.child,
        &mut ledger,
        vec![Fact::MessageAppended {
            message: next.clone(),
        }],
    )
    .await?;
    messages.push(next);
    Ok(Opening { messages, ledger })
}

/// fork 的子会话只存自己的消息：前面拼上内存里继承的前缀（修复线 L-A，agents.md §8.1.3）。
fn with_prefix(agent: &LiveAgent, own: Vec<Message>) -> Vec<Message> {
    match &agent.fork_prefix {
        Some(prefix) => prefix.iter().cloned().chain(own).collect(),
        None => own,
    }
}

/// 一次子循环的结果。
pub(crate) struct Finished {
    pub(crate) ending: Ending,
    pub(crate) report: Report,
    /// 子循环本身出错（不是模型的 API 错误）。
    pub(crate) error: Option<QueryError>,
}

fn abort_of(agent: &LiveAgent) -> Abort {
    match &agent.state {
        AgentState::Running { abort, .. } => abort.clone(),
        AgentState::Parked { .. } | AgentState::Finished(_) => Abort::new(),
    }
}

/// 跑到结束。前台时 `cancel` 是父工具调用的取消信号，收到后中止子循环并等它收尾。
pub(crate) async fn drive(
    core: &Arc<Core>,
    agent: &LiveAgent,
    opening: Opening,
    parent: Option<Parent>,
    cancel: Option<CancellationToken>,
) -> Finished {
    let Opening { messages, ledger } = opening;
    let started = core.ports.clock.now_ms();
    // 修正（2026-11-05）：`tool_uses` 是子会话里的 tool_use 块数（agents.md §2），续跑时也算之前几次运行的。以前只数
    // 这一次运行的，命令保活续跑后的第二次通知少算了第一次运行的工具。之前的从这次运行开头的历史数
    // （唯一来源是子会话），fork 继承的前缀是父会话的，不算。
    let inherited = agent.fork_prefix.as_ref().map_or(0, |prefix| prefix.len());
    let earlier: usize = messages
        .iter()
        .skip(inherited)
        .filter_map(|message| match message {
            Message::Assistant(assistant) => Some(tool_use_blocks(&assistant.content)),
            _ => None,
        })
        .sum();
    tracing::info!(
        event = "subagent.start",
        session = %agent.parent,
        agent = %agent.agent,
        child = %agent.child,
        agent_type = %agent.definition.agent_type,
        background = agent.background,
        depth = agent.depth,
        "subagent started"
    );
    let abort = abort_of(agent);
    // 这次运行开头的 user 消息是列表的最后一条（新建时是 prompt，续跑时是追加的消息）。
    let prompt = messages
        .last()
        .map_or_else(|| core.ports.ids.message(), |message| message.id().clone());
    let sink = Arc::new(
        ChildSink::new(
            core.ports.store.clone(),
            core.ports.ids.clone(),
            core.live.clone(),
            agent.agent.clone(),
            agent.child.clone(),
            prompt,
            parent,
        )
        .with_ledger(ledger)
        .asking(&agent.definition.agent_type, agent.asks.clone())
        .with_core(core.clone()),
    );
    let result = match core.registry() {
        Some(registry) => {
            run_loop(core, agent, registry, messages, sink.clone(), abort, cancel).await
        }
        // 主线程的注册表已随内核释放（R1-13）：不带工具空跑没有意义，按失败结束。
        None => Err(QueryError::Unsupported {
            what: "running a subagent after the kernel was released".to_owned(),
        }),
    };
    // 子会话这一轮的 `turn_usage`（usage.md §2.4）：用量行是观测，写不进去只记日志。
    let status = match &result {
        Ok(QueryStop::Aborted) => CallStatus::Cancelled,
        Ok(QueryStop::ApiError { .. }) | Err(_) => CallStatus::Error,
        Ok(
            QueryStop::Completed
            | QueryStop::MaxTurns
            | QueryStop::HookStopped
            | QueryStop::StopHookPrevented
            | QueryStop::Blocked { .. },
        ) => CallStatus::Completed,
    };
    if let Err(error) = sink.commit(vec![Fact::TurnEnded { status }]).await {
        tracing::warn!(%error, "the subagent turn usage could not be stored");
    }
    let (last, run_tool_uses) = sink.outcome();
    let tool_uses = earlier + run_tool_uses;
    let mut text = report::final_text(last.as_ref());
    let (ending, error) = match result {
        Ok(QueryStop::Aborted) => (Ending::Killed, None),
        Ok(QueryStop::ApiError { message } | QueryStop::Blocked { message }) => {
            // 子循环以一条 API 错误消息结束，它就是最后的文本。
            if text.is_empty() {
                text = message;
            }
            (Ending::Completed, None)
        }
        // 工具的 hook 要求停止、SubagentStop 阻止继续：子代理按正常结束，结果是已有的文本（query.md §3）。
        Ok(
            QueryStop::Completed
            | QueryStop::MaxTurns
            | QueryStop::HookStopped
            | QueryStop::StopHookPrevented,
        ) => (Ending::Completed, None),
        // 失败时 `<result>` 仍是已有的文本，错误进 `<summary>`（`failed: <error>`，agents.md §5）。
        Err(error) => (Ending::Failed, Some(error)),
    };
    let duration_ms = core.ports.clock.now_ms().saturating_sub(started);
    // `Option` 的字段在 None 时不写（logging.md §2）。
    let failure = error.as_ref().map(ToString::to_string);
    tracing::info!(
        event = "subagent.end",
        session = %agent.parent,
        agent = %agent.agent,
        child = %agent.child,
        background = agent.background,
        ending = ?ending,
        duration_ms,
        tool_uses,
        error = failure.as_deref(),
        "subagent finished"
    );
    Finished {
        ending,
        report: Report {
            text,
            tokens: report::tokens(last.and_then(|last| last.usage)),
            tool_uses,
            duration_ms,
        },
        error,
    }
}

/// 组装子循环并跑到结束。前台时 `cancel` 是父工具调用的取消信号，收到后中止子循环并等它收尾。
async fn run_loop(
    core: &Arc<Core>,
    agent: &LiveAgent,
    registry: Arc<dyn ToolRegistry>,
    messages: Vec<Message>,
    sink: Arc<ChildSink>,
    abort: Abort,
    cancel: Option<CancellationToken>,
) -> Result<QueryStop, QueryError> {
    // fork：主线程的整份注册表与主线程的 system（同对照实现，修复线 L-A，agents.md §8.1.2）。
    let (tools, context): (
        Arc<dyn ToolRegistry>,
        Arc<dyn rust_cli_kernel_api::orchestration::ContextBuilder>,
    ) = if agent.fork_prefix.is_some() {
        (
            registry,
            Arc::new(crate::fork::ForkContext {
                inner: core.ports.context.clone(),
            }),
        )
    } else {
        (
            Arc::new(FilteredTools {
                inner: registry,
                rule: ToolRule::new(&agent.definition, agent.background, agent.depth),
            }),
            Arc::new(SubagentContext {
                inner: core.ports.context.clone(),
                system: report::system(&agent.definition.prompt),
            }),
        )
    };
    let runner = Tracked {
        inner: core.ports.queries.runner(tools, context),
        book: core.runs.clone(),
    };
    let request = QueryRequest {
        agent: Some(agent.agent.clone()),
        max_turns: agent.definition.max_turns,
        // 子代理的缺省机器（R3，remote-exec.md §20.6）：派生时 Agent 的 `machine`。
        default_machine: agent.machine.clone(),
        ..QueryRequest::new(
            agent.child.clone(),
            core.ports.ids.run(),
            agent.workspace.clone(),
            messages,
            agent.selection.clone(),
            agent.mode,
            QuerySource::Subagent(agent.definition.agent_type.clone()),
        )
    };
    let run = rust_cli_kernel_api::orchestration::QueryRunner::run(
        &runner,
        request,
        sink.clone(),
        abort.clone(),
    );
    tokio::pin!(run);
    match cancel {
        Some(cancel) => tokio::select! {
            result = &mut run => result,
            () = cancel.cancelled() => {
                abort.abort(AbortReason::User);
                run.await
            }
        },
        None => run.await,
    }
}

/// 后台运行：在 agents 的作用域里跑；结束后按 agents.md §5.1 停放、续跑或送出通知（`routing`）。
pub(crate) fn background(core: &Arc<Core>, agent: LiveAgent, opening: Opening) {
    let task_core = core.clone();
    let _task = core.ports.spawner.spawn("subagent", async move {
        let finished = drive(&task_core, &agent, opening, None, None).await;
        let settled = routing::settle(&task_core, &agent.agent, &finished).await;
        routing::wake(&task_core, settled.wakes).await;
    });
}

/// 新子代理的 ID（Node 的 `agent_<uuid>` 与 `sess_subagent_agent_<uuid>`）。
pub(crate) fn ids(core: &Core) -> (AgentId, SessionId) {
    core.ports.ids.subagent()
}

#[cfg(test)]
#[path = "spawn_tests.rs"]
mod tests;
