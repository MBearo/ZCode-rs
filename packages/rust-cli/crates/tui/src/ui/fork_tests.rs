//! `/fork` 的选择器（session-lifecycle.md §6.5.3）：每条一行、带序号、焦点在最新的一条；选中即分叉。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::keyboard::Typing;
use crate::ui::list::Outcome;
use crate::ui::palette::{ColorLevel, Palette};
use crate::view::Transcript;

fn app(lang: Lang) -> App {
    App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        lang,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    )
}

fn entries() -> Vec<Entry> {
    ["first", "second\n  line", "third"]
        .iter()
        .enumerate()
        .map(|(index, text)| Entry {
            message: MessageId::new(format!("msg_{}", index + 1)),
            text: (*text).to_owned(),
        })
        .collect()
}

fn rows(app: &App) -> Vec<String> {
    let Some(Overlay::List(list)) = &app.overlay else {
        panic!("the picker is not open")
    };
    let (lines, _) = list.lines(80, 24, app.lang, &app.palette);
    lines
        .iter()
        .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

#[test]
fn the_inputs_are_numbered_with_the_newest_focused() {
    let mut app = app(Lang::En);
    app.open_fork(entries());
    let rows = rows(&app);
    assert_eq!(rows[1], "  Fork");
    assert!(rows.contains(&"    1. first".to_owned()), "{rows:?}");
    // 多行输入压成一行。
    assert!(rows.contains(&"    2. second line".to_owned()), "{rows:?}");
    assert!(
        rows.iter().any(|row| row.starts_with("  › 3. third")),
        "{rows:?}"
    );
    let mut zh = self::app(Lang::Zh);
    zh.open_fork(Vec::new());
    let rows = self::rows(&zh);
    assert_eq!(rows[1], "  分叉");
    assert!(
        rows.iter()
            .any(|row| row.contains("还没有可以分叉的输入。")),
        "{rows:?}"
    );
}

#[test]
fn a_digit_forks_after_that_input() {
    let mut app = app(Lang::En);
    app.open_fork(entries());
    let Some(Overlay::List(list)) = &mut app.overlay else {
        panic!("the picker is not open")
    };
    let key = KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE);
    let Outcome::Chosen(value) = list.on_key(key, Typing::default()) else {
        panic!("a digit chooses directly")
    };
    let action = app.fork_chosen(&value);
    assert!(
        matches!(&action, Some(Action::Fork { input }) if input.as_str() == "msg_2"),
        "{action:?}"
    );
}

/// 带参数时忽略参数（工作线 F78）：与不带参数相同，打开选择器，不显示错误。
#[test]
fn arguments_are_ignored() {
    for text in ["/fork", "/fork latest", "/fork checkpoint_1 more"] {
        let mut app = app(Lang::En);
        assert_eq!(app.slash(text), Some(Action::OpenFork), "{text}");
        assert!(app.local.is_empty(), "{text}");
    }
}

/// 分叉后的提示（工作线 F78）：英文是内核的回执（Node 原文）加 Node 的切换提示；中文另配，消息 ID 与条数取回执。
#[test]
fn the_notice_after_a_fork_follows_the_language() {
    let result = serde_json::json!({
        "session": "sess_2",
        "response": "Forked session sess_2 from message msg_4: copied 4 messages.",
        "copied": 4,
        "message": "msg_4",
    });
    assert_eq!(
        forked_notice(Lang::En, "sess_2", &result),
        "Forked session sess_2 from message msg_4: copied 4 messages.\nSwitched to forked session sess_2."
    );
    assert_eq!(
        forked_notice(Lang::Zh, "sess_2", &result),
        "已从消息 msg_4 分出会话 sess_2：复制了 4 条消息。\n已切换到分出的会话 sess_2。"
    );
}
