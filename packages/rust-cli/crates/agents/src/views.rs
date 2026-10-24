//! 给前端看的子代理快照（docs/specs/rust-cli/tui-panels.md §1）：输入框下面的列表与只读查看。
//!
//! 事实都在 `live::Live`（身份、状态、时刻、等待、提交次数、当前活动）与子会话的存储里；这里只按查询时刻整理，
//! 不另存状态。

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use rust_cli_kernel_api::manage::{BackgroundEntry, BackgroundKind};
use rust_cli_kernel_api::panels::{SubagentEntry, SubagentStatus, TasksSnapshot};
use rust_cli_kernel_api::session_api::{KernelError, SessionView};
use rust_cli_kernel_api::store::StoreError;

use crate::Core;
use crate::live::{AgentState, Live, LiveAgent};
use crate::report::Ending;

fn status(agent: &LiveAgent) -> SubagentStatus {
    match &agent.state {
        AgentState::Running { .. } => SubagentStatus::Running,
        // 停放的子代理状态是 completed。
        AgentState::Parked(_) => SubagentStatus::Completed,
        AgentState::Finished(Ending::Completed) => SubagentStatus::Completed,
        AgentState::Finished(Ending::Failed) => SubagentStatus::Failed,
        AgentState::Finished(Ending::Killed | Ending::KilledParked) => SubagentStatus::Killed,
    }
}

fn entry(agent: &LiveAgent, now: u64) -> SubagentEntry {
    let shown = &agent.shown;
    let end = shown.ended_ms.unwrap_or(now);
    let running = matches!(agent.state, AgentState::Running { .. });
    // 只有在跑的子代理带当前活动；重试与停顿按查询时刻算还要等多久（stall-visibility.md §4.3）。
    let activity = shown
        .doing
        .as_ref()
        .filter(|_| running)
        .map(|(doing, at)| doing.shown(now.saturating_sub(*at)));
    SubagentEntry {
        agent: agent.agent.clone(),
        child: agent.child.clone(),
        agent_type: agent.definition.agent_type.clone(),
        description: agent.description.clone(),
        status: status(agent),
        background: agent.background,
        waiting: shown.waiting > 0,
        elapsed_ms: end.saturating_sub(shown.started_ms),
        ended_ago_ms: shown.ended_ms.map(|ended| now.saturating_sub(ended)),
        version: shown.version,
        owner: agent.owner.clone(),
        activity,
        parked: matches!(agent.state, AgentState::Parked(_)),
        // 子代理的缺省机器（远端执行 R4，remote-exec.md §21.6）。
        machine: agent.machine.clone(),
    }
}

/// 顶层会话 `root` 里的子代理（含嵌套的），按启动顺序。
pub(crate) fn entries(live: &Live, root: &SessionId) -> Vec<SubagentEntry> {
    let (mut agents, now) = live.snapshot_in(root);
    agents.sort_by_key(|agent| agent.shown.order);
    agents.iter().map(|agent| entry(agent, now)).collect()
}

/// footer 的一个后台子代理：在跑或停放的后台子代理都算在跑（与 TaskStop 的“能停”同一个判断，tui-display.md §3.1）。
pub(crate) fn background_entry(agent: &LiveAgent) -> BackgroundEntry {
    BackgroundEntry {
        id: agent.agent.to_string(),
        kind: BackgroundKind::Agent,
        description: agent.description.clone(),
        status: BackgroundStatus::Running,
        output: None,
        machine: agent.machine.clone(),
    }
}

/// 子代理条目与在跑的后台子代理出自 `live::Live` 的同一次读取（修复线 F34，tui-panels.md §0）：列表的行与 pill 里的子代理
/// 不会一新一旧。后台子代理与 `Live::background_in` 同一个筛选、同一个顺序（按 ID）。
pub(crate) fn tasks(live: &Live, root: &SessionId) -> TasksSnapshot {
    let (mut agents, now) = live.snapshot_in(root);
    let background = agents
        .iter()
        .filter(|agent| agent.background && agent.is_live())
        .map(background_entry)
        .collect();
    agents.sort_by_key(|agent| agent.shown.order);
    TasksSnapshot {
        subagents: agents.iter().map(|agent| entry(agent, now)).collect(),
        background,
    }
}

/// 变化通知：`live::Live` 每次改动后产生一项（watch 合并多次改动）。
pub(crate) fn changes(live: &Live) -> BoxStream<'static, ()> {
    futures_util::stream::unfold(live.subscribe(), |mut changes| async move {
        changes.changed().await.ok().map(|()| ((), changes))
    })
    .boxed()
}

/// 只读快照：子会话里已提交的消息；`running` 是它是否在跑。子代理不属于 `root` 时按找不到处理。
pub(crate) async fn view(
    core: &Core,
    root: &SessionId,
    agent: &AgentId,
) -> Result<SessionView, KernelError> {
    let found = core.live.get(agent).filter(|found| &found.root == root);
    let Some(found) = found else {
        return Err(KernelError::Failed {
            message: format!("No agent named '{agent}' in this session"),
        });
    };
    let loaded = core
        .ports
        .store
        .load(&found.child)
        .await
        .map_err(|error| match error {
            StoreError::NotFound { session } => KernelError::NotFound { session },
            other => KernelError::Failed {
                message: other.to_string(),
            },
        })?;
    // 执行状态（修复线 F20）：库里有就用库里的权限模式与 plan 开关；没有时用子代理运行的单值模式。
    let execution = match loaded.mode {
        Some(mode) => {
            rust_cli_kernel_api::permission::ExecutionState::from_parts(mode, loaded.plan)
        }
        None => found.mode.into(),
    };
    Ok(SessionView {
        meta: loaded.meta,
        messages: loaded.messages,
        selection: loaded.model.unwrap_or(found.selection),
        mode: execution.mode,
        plan: execution.plan,
        pending: Vec::new(),
        queue: Vec::new(),
        last_seq: 0,
        running: matches!(found.state, AgentState::Running { .. }),
        background: Vec::new(),
        peer_held: Vec::new(),
        // 子代理的只读快照不显示 footer 的上下文（F27）。
        context_usage: None,
        context_notice: Default::default(),
    })
}

#[cfg(test)]
#[path = "views_tests.rs"]
mod tests;
