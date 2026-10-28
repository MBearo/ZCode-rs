//! 发送输入（docs/specs/rust-cli/tui.md §6）：skill 清单还没到时提交的命令。
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::Delivery;
use rust_cli_kernel_api::testing::selection;
use rust_cli_kernel_api::user_command::UserCommand;

use super::super::app::{Action, App, Local};
use crate::ui::i18n::Lang;
use crate::ui::palette::{ColorLevel, Palette};
use crate::view::Transcript;

fn app() -> App {
    App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    )
}

/// 本地提示行的文字（`App::info`）。
fn infos(app: &App) -> Vec<String> {
    app.local
        .iter()
        .filter_map(|cell| match &cell.local {
            Local::Info(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn greet() -> UserCommand {
    UserCommand {
        name: "greet".into(),
        description: "Greet someone".into(),
        argument_hint: None,
        hidden: false,
    }
}

/// 修复线 F13：输入框一就绪就提交 `/greet Ada` 时清单还在路上，原来当场判成未知命令。
/// 现在输入留在输入框里，清单到了再按同样的规则提交一次。
#[test]
fn a_command_submitted_before_the_skill_list_arrives_waits_for_it() {
    let mut app = app();
    app.composer.area.insert_str("/greet Ada");
    assert!(app.submit().is_none());
    assert_eq!(app.composer.area.text(), "/greet Ada");
    assert!(infos(&app).is_empty(), "no unrecognized-command line yet");
    match app.commands_arrived(vec![greet()]) {
        Some(Action::Submit { text, delivery }) => {
            assert_eq!(text, "/greet Ada");
            assert_eq!(delivery, Delivery::Queue);
        }
        other => panic!("expected the held command to be submitted: {other:?}"),
    }
    assert!(app.composer.is_empty());
    // 之后再到的清单不再重复提交。
    assert!(app.commands_arrived(vec![greet()]).is_none());
}

/// 清单到了、仍然不是 skill：按未知命令提示，输入保留。
#[test]
fn a_held_command_that_is_not_a_skill_is_unrecognized_once_the_list_arrives() {
    let mut app = app();
    app.composer.area.insert_str("/nope foo");
    assert!(app.submit().is_none());
    assert!(app.commands_arrived(Vec::new()).is_none());
    assert_eq!(app.composer.area.text(), "/nope foo");
    let lines = infos(&app);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("Unrecognized command '/nope'"),
        "{lines:?}"
    );
}

/// 立即发送的键（tui.md §5.1.1、tui-input.md §11.1）的测试用事件：开回合、排一条插话。
mod send_now {
    use std::time::Instant;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use rust_cli_kernel_api::ids::{CommandId, TurnId};
    use rust_cli_kernel_api::session_api::{Delivery, KernelEvent, QueuedInput, SequencedEvent};

    use super::{Action, App, app};

    fn feed(app: &mut App, events: Vec<KernelEvent>) {
        for (index, event) in events.into_iter().enumerate() {
            let seq = index as u64 + 1;
            let event = SequencedEvent {
                seq,
                revision: 0,
                event,
            };
            app.view.apply(event, seq * 1000);
        }
    }

    fn started() -> KernelEvent {
        KernelEvent::TurnStarted {
            turn: TurnId::new("t1"),
            command: None,
        }
    }

    fn queued() -> KernelEvent {
        KernelEvent::QueueChanged {
            items: vec![QueuedInput {
                command: CommandId::new("c2"),
                text: "also check the date".into(),
                delivery: Delivery::Steer,
            }],
        }
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    /// ctrl+x ctrl+s 与 ctrl+enter 两种按法的结果。
    fn both_keys(app: &mut App) -> [Option<Action>; 2] {
        let now = Instant::now();
        assert!(app.on_key(ctrl(KeyCode::Char('x')), now).is_none());
        let chord = app.on_key(ctrl(KeyCode::Char('s')), now);
        let enter = app.on_key(ctrl(KeyCode::Enter), now);
        [chord, enter]
    }

    /// 修复线 F105：输入框为空、回合在跑、队列里有插话时，立即发送的键只投递排着的插话（不入队新文字、不中断）。
    /// 旧代码什么都不做（`submit` 在输入框为空时返回 None）。
    #[test]
    fn an_empty_input_delivers_the_queued_input() {
        let mut app = app();
        feed(&mut app, vec![started(), queued()]);
        assert!(app.send_now_hint().is_some(), "the hint asks for this key");
        for action in both_keys(&mut app) {
            assert!(matches!(action, Some(Action::DeliverQueued)), "{action:?}");
        }
    }

    /// 队列为空、或回合没在跑（提示那一行也不显示）：什么都不做，也不中断。
    #[test]
    fn nothing_happens_without_a_queued_input_or_a_running_turn() {
        let mut running = app();
        feed(&mut running, vec![started()]);
        for action in both_keys(&mut running) {
            assert!(action.is_none(), "{action:?}");
        }
        let mut idle = app();
        feed(&mut idle, vec![queued()]);
        for action in both_keys(&mut idle) {
            assert!(action.is_none(), "{action:?}");
        }
    }

    /// 输入框里有文字时照旧：入队这段文字再做低延迟投递。
    #[test]
    fn text_in_the_input_is_still_sent_now() {
        let mut app = app();
        feed(&mut app, vec![started(), queued()]);
        app.composer.area.insert_str("stop here");
        let action = app.on_key(ctrl(KeyCode::Enter), Instant::now());
        match action {
            Some(Action::SendNow { text, delivery }) => {
                assert_eq!(text, "stop here");
                assert_eq!(delivery, Delivery::Steer);
            }
            other => panic!("expected send now: {other:?}"),
        }
    }
}
