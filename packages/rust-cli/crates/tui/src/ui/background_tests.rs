//! 后台命令（tui-panels.md §3；tui-tasks.md §3.2）：在跑的命令的行、展开的输出的读取（序号与再读）、输出的清洗。画法在
//! `task_card_tests.rs`。
use rust_cli_kernel_api::panels::ShellOutput;

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::task_card::TaskKind;
use crate::ui::task_line::tests::{app, shell};

fn detail(status: BackgroundStatus, lines: &[&str], total: u64) -> ShellDetail {
    ShellDetail {
        id: "b1".into(),
        command: "npm test".into(),
        status,
        elapsed_ms: (status == BackgroundStatus::Running).then_some(83_000),
        output: ShellOutput::Tail {
            lines: lines.iter().map(|l| (*l).to_owned()).collect(),
            total,
        },
    }
}

/// 只有在跑的命令有行（结束了的与后台子代理不是），按快照的次序；monitor 与 shell 同列（修复线 F117）。
#[test]
fn only_running_commands_have_rows() {
    let mut app = app(Lang::En);
    app.snapshots.background = vec![
        shell("b1", "npm test"),
        BackgroundEntry {
            kind: BackgroundKind::Monitor,
            ..shell("m1", "build log")
        },
        BackgroundEntry {
            status: BackgroundStatus::Completed,
            ..shell("b2", "ls")
        },
        BackgroundEntry {
            kind: BackgroundKind::Agent,
            ..shell("agent_a", "Review code")
        },
        shell("b3", "cargo watch"),
    ];
    let ids: Vec<&str> = app.shell_rows().iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["b1", "m1", "b3"]);
}

/// 展开的输出只收下序号对得上的回复；在跑时请再读，结束了或内核不认识它时不再读。没展开时回复丢掉。
#[test]
fn the_peek_keeps_the_latest_read_only() {
    let mut app = app(Lang::En);
    app.snapshots.background = vec![shell("b1", "npm test")];
    assert!(!app.on_shell_detail("b1", 0, Some(detail(BackgroundStatus::Running, &["x"], 1))));
    app.open_tasks();
    assert_eq!(app.task_card().unwrap().kind, TaskKind::Commands);
    app.task_card_mut().unwrap().peek = Some(ShellView::new("b1".into()));
    app.shell_reading("b1", 7);
    assert!(!app.on_shell_detail(
        "b1",
        6,
        Some(detail(BackgroundStatus::Running, &["old"], 1))
    ));
    assert_eq!(app.task_card().unwrap().peek.as_ref().unwrap().last, None);
    assert!(app.on_shell_detail(
        "b1",
        7,
        Some(detail(BackgroundStatus::Running, &["new"], 1))
    ));
    let peek = app.task_card().unwrap().peek.clone().unwrap();
    assert_eq!(peek.seq, 7);
    assert_eq!(
        peek.last.unwrap().output,
        ShellOutput::Tail {
            lines: vec!["new".into()],
            total: 1
        }
    );
    assert!(!app.on_shell_detail(
        "b1",
        7,
        Some(detail(BackgroundStatus::Completed, &["done"], 1))
    ));
    assert!(!app.on_shell_detail("b1", 7, None));
    assert!(app.task_card().unwrap().peek.as_ref().unwrap().gone);
}

#[test]
fn ansi_and_control_characters_are_stripped() {
    assert_eq!(plain_output("\u{1b}[31mred\u{1b}[0m\tx\u{7}"), "red    x");
}
