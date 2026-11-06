//! 管理命令的文字（tui-display.md §2.3、§4.9）。在跑的命令的计数文字（以前 footer 的 pill）2026-10-31 起是任务行的
//! 命令一段与回合结束那一行的 `still running`（tui-tasks.md §2.2，`task_line_tests.rs`）。
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::manage::{BackgroundEntry, BackgroundKind};

use super::*;

fn entry(id: &str, kind: BackgroundKind, status: BackgroundStatus) -> BackgroundEntry {
    BackgroundEntry::new(id, kind, id, status)
}

#[test]
fn the_count_covers_only_the_running_shells() {
    use BackgroundKind::{Agent, Shell};
    use BackgroundStatus::{Completed, Running};
    assert_eq!(pill(&[], Lang::En), None);
    assert_eq!(pill(&[entry("b1", Shell, Completed)], Lang::En), None);
    assert_eq!(
        pill(&[entry("b1", Shell, Running)], Lang::En).as_deref(),
        Some("1 shell")
    );
    let shells = [entry("b1", Shell, Running), entry("b2", Shell, Running)];
    assert_eq!(pill(&shells, Lang::En).as_deref(), Some("2 shells"));
    // 混合时只数命令（用户决定 2026-10-27）。
    let mixed = [entry("b1", Shell, Running), entry("a1", Agent, Running)];
    assert_eq!(pill(&mixed, Lang::En).as_deref(), Some("1 shell"));
    assert_eq!(pill(&mixed, Lang::Zh).as_deref(), Some("1 shell"));
}

/// 修复线 F117（tui-display.md §2.3）：Monitor 起的后台命令单独数，`N monitor(s)`；与 shell 都有时 shell 在前、用 `, `
/// 连接（中文用 `、`）。修前 monitor 也数成 shell（`1 shell`），看不出有几个 monitor。修复线 F118（用户决定
/// 2026-10-30）：中文不带“个”（`1 shell、2 monitor`；以前写 `1 个 shell、2 个 monitor`）。
#[test]
fn the_count_keeps_monitors_apart_from_shells() {
    use BackgroundKind::{Agent, Monitor, Shell};
    use BackgroundStatus::{Completed, Killed, Running};
    let one = [entry("b1", Monitor, Running)];
    assert_eq!(pill(&one, Lang::En).as_deref(), Some("1 monitor"));
    assert_eq!(pill(&one, Lang::Zh).as_deref(), Some("1 monitor"));
    let two = [entry("b1", Monitor, Running), entry("s2", Monitor, Running)];
    assert_eq!(pill(&two, Lang::En).as_deref(), Some("2 monitors"));
    assert_eq!(pill(&two, Lang::Zh).as_deref(), Some("2 monitor"));
    // shell 在前；子代理与不在跑的都不数。
    let both = [
        entry("b1", Monitor, Running),
        entry("a1", Agent, Running),
        entry("b2", Shell, Running),
        entry("s3", Monitor, Running),
        entry("b4", Monitor, Completed),
        entry("b5", Shell, Killed),
    ];
    assert_eq!(
        pill(&both, Lang::En).as_deref(),
        Some("1 shell, 2 monitors")
    );
    assert_eq!(pill(&both, Lang::Zh).as_deref(), Some("1 shell、2 monitor"));
    assert_eq!(
        command_text(2, 1, Lang::En).as_deref(),
        Some("2 shells, 1 monitor")
    );
    // 只有已结束的 monitor：没有。
    assert_eq!(pill(&[entry("b1", Monitor, Completed)], Lang::En), None);
}

/// 用户决定（tui-display.md §2.3、D25）：子代理不在这里数（任务行另有子代理的一段）。
#[test]
fn subagents_are_not_counted_as_commands() {
    use BackgroundKind::{Agent, Shell};
    use BackgroundStatus::{Completed, Running};
    let local = [entry("a1", Agent, Running), entry("a2", Agent, Running)];
    let remote = BackgroundEntry {
        machine: rust_cli_kernel_api::ids::MachineName::new("dev-a"),
        ..entry("a3", Agent, Running)
    };
    for lang in [Lang::En, Lang::Zh] {
        assert_eq!(pill(&local, lang), None);
        assert_eq!(pill(std::slice::from_ref(&remote), lang), None);
    }
    assert_eq!(
        pill(
            &[entry("b1", Shell, Completed), entry("a1", Agent, Running)],
            Lang::En
        ),
        None
    );
    let mixed = [
        entry("b1", Shell, Running),
        remote,
        entry("a1", Agent, Running),
        entry("b2", Shell, Running),
    ];
    assert_eq!(pill(&mixed, Lang::En).as_deref(), Some("2 shells"));
    assert_eq!(pill(&mixed, Lang::Zh).as_deref(), Some("2 shell"));
}
