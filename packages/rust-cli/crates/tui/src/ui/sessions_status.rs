//! `/resume` ⇥ 的看板（docs/specs/rust-cli/tui-panels.md §2；tui-kit.md §4.3「`/resume` ⇥ 的看板」）；驾驶舱也用这里的刷新：`/resume` 的列表列所有文件夹时，以前项目页的
//! 每行的状态、各状态的数量与选中的会话的分支、首条输入、最后的回复。状态的唯一来源是内核的 `project_sessions`（本进程打开着
//! 的会话的 `SessionActivity`）；现在的会话的事件改了它时 runner 按同一个查询重读（[`App::sessions_refreshing`]），收下时
//! 选中的留着（[`App::sessions_refreshed`]）。只列这个文件夹时都没有，同 Puffin。
use rust_cli_kernel_api::panels::ProjectSessions;
use rust_cli_kernel_api::session_api::KernelEvent;
use rust_cli_kernel_api::store::SessionOrder;
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::resume_list::{self, Dash, State};
use rust_cli_tui_kit::theme::theme::Theme;

use super::app::{App, Overlay};
use super::sessions_list::{Found, SessionsList, found_of};
use super::sessions_steps::Status;

/// 一个会话在看板上的状态与细节。
pub fn dash(x: &Found) -> Dash<'_> {
    Dash {
        state: Status::of(x.activity).state(),
        branch: x.branch.as_deref(),
        prompt: &x.prompt,
        reply: &x.reply,
    }
}

/// 各状态的数量（[`State::ALL`] 的次序）：数 ⌃f 筛之前的会话。
pub fn counts(shown: &[&Found]) -> Vec<(State, usize)> {
    State::ALL
        .iter()
        .map(|&state| {
            let n = shown
                .iter()
                .filter(|x| Status::of(x.activity).state() == state)
                .count();
            (state, n)
        })
        .collect()
}

/// 会改内核给的状态的事件（runtime `workspace_sessions::activity`：待处理的交互、在跑的回合、后台的工作）。
pub fn touches_activity(event: &KernelEvent) -> bool {
    matches!(
        event,
        KernelEvent::TurnStarted { .. }
            | KernelEvent::TurnFinished { .. }
            | KernelEvent::InteractionAsked { .. }
            | KernelEvent::InteractionResolved { .. }
            | KernelEvent::BackgroundWork { .. }
    )
}

impl SessionsList {
    /// 看板的第一行（kit `resume_list::counts_row`）：⌃f 选中的状态总是写。
    pub(super) fn counts_row(&self, theme: &Theme) -> Option<Element> {
        resume_list::counts_row(
            theme,
            &counts(&self.searched()),
            self.status.map(Status::state),
        )
    }
}

impl App {
    /// 现在的会话的状态变了（`touches_activity` 的事件）：列表开着、列所有文件夹时要重读的顺序；否则为 None（不读）。
    pub fn sessions_refreshing(&self) -> Option<SessionOrder> {
        match &self.overlay {
            Some(Overlay::Sessions(list)) if list.folders && list.all.is_some() => Some(list.order),
            _ => None,
        }
    }

    /// 重读的回答：换上新的会话与状态，选中的会话还在就留着它；词的回答里的同一个会话也换上新的状态与回复。
    pub fn sessions_refreshed(&mut self, sessions: &ProjectSessions) {
        let fresh: Vec<Found> = sessions
            .sessions
            .iter()
            .filter_map(|s| found_of(s, ""))
            .collect();
        let fresh = self.with_folders(fresh);
        let Some(Overlay::Sessions(list)) = self.overlay.as_mut() else {
            return;
        };
        if !list.folders {
            return;
        }
        let was = list.chosen_found().map(|x| x.id.clone());
        if let Some((_, found)) = list.found.as_mut() {
            for x in found.iter_mut() {
                if let Some(new) = fresh.iter().find(|n| n.id == x.id) {
                    x.activity = new.activity;
                    x.waiting = new.waiting;
                    x.reply.clone_from(&new.reply);
                    x.branch.clone_from(&new.branch);
                    x.presence.clone_from(&new.presence);
                }
            }
        }
        // 最后的回复只有打开着的会话才有：驾驶舱读回的（`cockpit_replies`）不因为重读丢掉。
        let mut fresh = fresh;
        for x in &mut fresh {
            if x.reply.is_empty()
                && let Some(old) = list.all.iter().flatten().find(|old| old.id == x.id)
            {
                x.reply.clone_from(&old.reply);
            }
        }
        list.all = Some(fresh);
        if let Some(at) = was.and_then(|id| list.matches().iter().position(|x| x.id == id)) {
            list.choice = at;
        }
    }
}

#[cfg(test)]
#[path = "sessions_status_tests.rs"]
mod tests;
