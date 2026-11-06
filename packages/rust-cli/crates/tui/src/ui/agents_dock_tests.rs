//! 结束了、结果还没到主会话的后台子代理（tui-panels.md §1.8）：只记见过在跑的顶层后台子代理；快照里没有了的立即离开。
//! 结果到达后离开的情形在 `task_line_tests.rs`（任务行按它数）。
use super::*;
use crate::ui::i18n::Lang;
use crate::ui::task_line::tests::{app, entry, set};

/// 前台子代理、后台子代理派生的、没见过在跑的都不记；停放与等你回答的算在跑。
#[test]
fn only_top_level_background_subagents_seen_running_are_kept() {
    let mut app = app(Lang::En);
    let mut child = entry("c", true, SubagentStatus::Running);
    child.owner = Some(AgentId::new("a"));
    let mut parked = entry("p", true, SubagentStatus::Completed);
    parked.parked = true;
    set(
        &mut app,
        vec![
            entry("a", true, SubagentStatus::Running),
            entry("f", false, SubagentStatus::Running),
            child,
            entry("d", true, SubagentStatus::Completed),
            parked,
        ],
    );
    app.note_dock();
    assert!(app.ended_unreported().is_empty());
    for entry in &mut app.snapshots.agents.entries {
        entry.status = SubagentStatus::Failed;
        entry.parked = false;
    }
    app.note_dock();
    assert_eq!(
        app.ended_unreported(),
        [SubagentStatus::Failed, SubagentStatus::Failed]
    );
}

/// 快照里没有了的立即离开（不等结果）。
#[test]
fn rows_leave_with_the_snapshot() {
    let mut app = app(Lang::En);
    set(&mut app, vec![entry("a", true, SubagentStatus::Running)]);
    app.note_dock();
    app.snapshots.agents.entries[0].status = SubagentStatus::Killed;
    app.note_dock();
    assert_eq!(app.ended_unreported(), [SubagentStatus::Killed]);
    app.snapshots.agents.entries.clear();
    app.note_dock();
    app.snapshots.agents.entries = vec![entry("a", true, SubagentStatus::Killed)];
    assert!(app.ended_unreported().is_empty());
}
