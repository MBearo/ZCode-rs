//! 结束了、结果还没到主会话的后台子代理（docs/specs/rust-cli/tui-panels.md §1.8）：任务行按它数「完成、出错、停了」
//! （tui-tasks.md §2.1）。以前这里画后台子代理的坞（每个顶层的后台子代理一行，Puffin 工作流运行的坞）；2026-10-31（用户
//! 决定）起坞的行不画，只留它离开的时刻的规则：界面见过它在跑的，结束后留到主会话收到它的 `<task-notification>`
//! （`Transcript::reported`）；快照里没有了的立即离开。
use rust_cli_kernel_api::ids::AgentId;
use rust_cli_kernel_api::panels::{SubagentEntry, SubagentStatus};

use super::app::App;

/// 记下的顶层后台子代理与它最近的状态：在跑（含停放与等你回答）或结束的终态。
#[derive(Debug, Clone)]
struct Seen {
    agent: AgentId,
    ended: Option<SubagentStatus>,
}

/// 见过在跑的顶层后台子代理（只有见过它在跑的，结束后才留到结果通知到达，同 Puffin 的运行）。
#[derive(Debug, Clone, Default)]
pub struct DockSeen {
    rows: Vec<Seen>,
}

/// 顶层的后台子代理：一次后台运行。它派生的子代理不另记。
fn top_level(entry: &SubagentEntry) -> bool {
    entry.background && entry.owner.is_none()
}

/// 条目此刻结束了没有：停放（自己跑完了、派生的后台子代理还在跑）与等你回答的都算在跑。
fn ended(entry: &SubagentEntry) -> Option<SubagentStatus> {
    match entry.status {
        _ if entry.waiting || entry.parked => None,
        SubagentStatus::Running => None,
        status => Some(status),
    }
}

impl App {
    /// 这一帧记下（`draw::draw` 在画之前调）：第一次见到在跑的记下；结束了而结果已经到了主会话的离开，快照里没有了的
    /// 也离开。
    pub(super) fn note_dock(&mut self) {
        let entries: Vec<(AgentId, Option<SubagentStatus>)> = self
            .snapshots
            .agents
            .entries
            .iter()
            .filter(|e| top_level(e))
            .map(|e| (e.agent.clone(), ended(e)))
            .collect();
        let view = &self.view;
        let rows = &mut self.dock.rows;
        rows.retain(|seen| {
            entries.iter().any(|(agent, end)| {
                *agent == seen.agent && !(end.is_some() && view.reported(agent.as_str()))
            })
        });
        for (agent, end) in entries {
            match rows.iter_mut().find(|seen| seen.agent == agent) {
                Some(seen) => seen.ended = end,
                None if end.is_none() => rows.push(Seen { agent, ended: None }),
                None => {}
            }
        }
    }

    /// 结束了、结果还没到主会话的顶层后台子代理的终态，按快照的次序。
    pub(super) fn ended_unreported(&self) -> Vec<SubagentStatus> {
        self.snapshots
            .agents
            .entries
            .iter()
            .filter_map(|e| self.dock.rows.iter().find(|s| s.agent == e.agent)?.ended)
            .collect()
    }
}

#[cfg(test)]
#[path = "agents_dock_tests.rs"]
mod tests;
