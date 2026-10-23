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
