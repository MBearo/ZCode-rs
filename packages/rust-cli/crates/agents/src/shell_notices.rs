//! 子代理里后台命令的通知（docs/specs/rust-cli/agents.md §5.2，L18）。
//!
//! 执行环境按子会话记它的后台命令与通知（唯一事实源仍是 env-local 的账本）。建子会话时为它起一个转发
//! 任务：收到一条通知，在 `live::Live` 的一把锁里按这个子代理的状态送出（在跑进邮箱、停放就续跑、已结束送顶层
//! 会话，与它派生的后台子代理的通知同一条路，`routing::deliver`），交出之后才 ack 执行环境的通知。
//!
//! 修正（L18）：以前子会话的命令通知没有接收方——会话 actor 只订阅顶层会话，子代理里 `run_in_background`
//! 的命令结束后通知一直挂在执行环境里，子代理与主线程都收不到，`-p` 也不等这些命令。

use std::sync::Arc;

use futures_util::StreamExt;
use rust_cli_kernel_api::env_shell::ShellNotice;
use rust_cli_kernel_api::ids::{AgentId, SessionId};

use crate::Core;
use crate::live::{LiveAgent, Notice};
use crate::routing;

/// 为这个子代理的子会话起转发任务（跟随进程；执行环境的流结束时它也结束）。
pub(crate) fn watch(core: &Arc<Core>, agent: &LiveAgent) {
    let core = core.clone();
    let id = agent.agent.clone();
    let child = agent.child.clone();
    let root = agent.root.clone();
    let _task = core
        .ports
        .spawner
        .clone()
        .spawn("subagent-shell-notices", async move {
            let mut notices = core.ports.machine.shell().shell_notices(&child, 0);
            while let Some(next) = notices.next().await {
                let Ok(notice) = next else {
                    // 执行环境不提供后台通知：以后也不会有。
                    return;
                };
                let seq = notice.seq;
                // 在看的 monitor 的终态交给它的任务：合成带剩下的行的终态、送出之后由它 ack（agents.md §5.3）。
                if notice.kind == rust_cli_kernel_api::env_shell::BackgroundKind::Monitor
                    && core.monitors.take(&notice)
                {
                    continue;
                }
                forward(&core, &id, &root, notice).await;
                if let Err(error) = core
                    .ports
                    .machine
                    .shell()
                    .ack_shell_notice(&child, seq)
                    .await
                {
                    tracing::warn!(%error, seq, "a subagent command notice was not acknowledged");
                }
            }
        });
}

/// 一条命令通知交给这个子代理（或它已结束时交给顶层会话）。
async fn forward(core: &Arc<Core>, agent: &AgentId, root: &SessionId, notice: ShellNotice) {
    let (status, envelope) = (core.ports.shell_notice)(&notice);
    let notice = Notice {
        // 通知的来源标识：顶层会话的 `BackgroundWork` 与后台列表用它（命令的任务 ID）。
        from: AgentId::new(notice.task.as_str()),
        status,
        envelope,
    };
    let wake = core
        .live
        .with(|agents| routing::deliver(agents, &core.hub, Some(agent), root, notice));
    routing::wake(core, wake.into_iter().collect()).await;
}

/// 顶层会话 `root` 里各子代理子会话还在跑、或通知还没 ack 的后台命令（`-p` 等它们，agents.md §5.2 第 4 条）。
pub(crate) async fn background_commands(core: &Core, root: &SessionId) -> Vec<AgentId> {
    let children: Vec<LiveAgent> = core.live.with(|agents| {
        agents
            .values()
            .filter(|agent| &agent.root == root)
            .cloned()
            .collect()
    });
    let mut commands = Vec::new();
    for agent in children {
        if let Ok(state) = core
            .ports
            .machine
            .shell()
            .shell_state(&agent.workspace, &agent.child)
            .await
        {
            commands.extend(
                state
                    .background
                    .into_iter()
                    .map(|task| AgentId::new(task.as_str())),
            );
        }
    }
    commands
}

#[cfg(test)]
#[path = "shell_notices_tests.rs"]
mod tests;
