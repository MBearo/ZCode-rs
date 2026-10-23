//! 计划模式 attachment 的节奏：`due` 按模式、会话标志与历史决定发什么（context.md §13.2、§13.4）。

use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::plan::PlanModeFlags;

use super::tests::{human, humans, marker};
use super::{Due, EXIT, PLAN_MODE, REENTRY, due};

const NONE: PlanModeFlags = PlanModeFlags {
    has_exited: false,
    needs_exit_attachment: false,
};
const EXITED: PlanModeFlags = PlanModeFlags {
    has_exited: true,
    needs_exit_attachment: false,
};
const LEFT: PlanModeFlags = PlanModeFlags {
    has_exited: true,
    needs_exit_attachment: true,
};

fn enter(full: bool, reentry: bool) -> Due {
    Due::Enter { full, reentry }
}

#[test]
fn the_first_step_in_plan_mode_gets_a_full_reminder() {
    assert_eq!(due(&[], PermissionMode::Plan, NONE), enter(true, false));
    assert_eq!(
        due(&[human()], PermissionMode::Plan, NONE),
        enter(true, false)
    );
}

#[test]
fn reminders_wait_five_human_turns_and_every_fifth_is_full() {
    let mut history = vec![human(), marker(PLAN_MODE)];
    history.extend(humans(4));
    assert_eq!(due(&history, PermissionMode::Plan, NONE), Due::Nothing);
    history.push(human());
    assert_eq!(
        due(&history, PermissionMode::Plan, NONE),
        enter(false, false)
    );
    // 第 6 条 plan_mode 又是完整提醒。
    let mut history = Vec::new();
    for _ in 0..5 {
        history.push(marker(PLAN_MODE));
        history.extend(humans(5));
    }
    assert_eq!(
        due(&history, PermissionMode::Plan, NONE),
        enter(true, false)
    );
}

#[test]
fn tool_rounds_do_not_count_as_turns() {
    let history = vec![human(), marker(PLAN_MODE), marker("total_tokens_reminder")];
    assert_eq!(due(&history, PermissionMode::Plan, NONE), Due::Nothing);
}

#[test]
fn leaving_plan_mode_announces_the_exit_once() {
    let entered = vec![human(), marker(PLAN_MODE)];
    assert_eq!(due(&entered, PermissionMode::AcceptEdits, NONE), Due::Exit);
    let reentered = vec![marker(REENTRY), marker(PLAN_MODE)];
    assert_eq!(due(&reentered, PermissionMode::Default, NONE), Due::Exit);
    let exited = vec![marker(PLAN_MODE), marker(EXIT), human()];
    assert_eq!(due(&exited, PermissionMode::Default, NONE), Due::Nothing);
    assert_eq!(due(&[human()], PermissionMode::Default, NONE), Due::Nothing);
}

/// 待发退出提醒的标志为真时，模型还没收到 `plan_mode` 也发退出提醒（context.md §13.4）。
#[test]
fn the_exit_flag_announces_an_exit_the_history_does_not_show() {
    assert_eq!(due(&[human()], PermissionMode::Default, LEFT), Due::Exit);
    let exited = vec![marker(PLAN_MODE), marker(EXIT), human()];
    assert_eq!(due(&exited, PermissionMode::Default, LEFT), Due::Exit);
    // 回到 plan 模式时不发退出提醒（进入 plan 时 actor 已清掉这个标志）。
    assert_eq!(
        due(&[human()], PermissionMode::Plan, LEFT),
        enter(true, true)
    );
}

#[test]
fn entering_again_after_an_exit_restarts_the_cycle() {
    let mut history = Vec::new();
    for _ in 0..3 {
        history.push(marker(PLAN_MODE));
        history.extend(humans(5));
    }
    history.push(marker(EXIT));
    history.push(human());
    assert_eq!(
        due(&history, PermissionMode::Plan, EXITED),
        enter(true, true)
    );
}

/// reentry 只看“离开过 plan 模式”的标志，不看历史（resume 后标志为假，压缩边界之前的
/// 退出也照样算，context.md §13.4）。
#[test]
fn the_reentry_follows_the_session_flag_not_the_history() {
    let exited = vec![marker(PLAN_MODE), marker(EXIT), human()];
    assert_eq!(due(&exited, PermissionMode::Plan, NONE), enter(true, false));
    assert_eq!(
        due(&[human()], PermissionMode::Plan, EXITED),
        enter(true, true)
    );
}
