//! `/fork` 的点列表（fork.rs）：同 `/rewind` 那一层，最新的在上、跟着草稿过滤、⏎ 在那条输入之后分叉、esc 关上。
use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::MessageId;

use super::*;
use crate::ui::test_support::{app_in, key};

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
    super::super::pick::lay(&app.forks_rows(0), 80)
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

/// 照 Puffin 的 `/rewind` 列表（tui-kit.md §4.3「`/fork`」）：没有标题，最新的在上，消息的第一行，选中的下面一行说分叉做什么。
#[test]
fn the_inputs_are_listed_newest_first_like_the_rewind_list() {
    let mut app = app_in(Lang::En);
    app.open_fork(entries());
    assert_eq!(app.composer.area.text(), DRAFT);
    let rows = rows(&app);
    let names: Vec<&str> = rows.iter().map(|r| r.trim_start()).collect();
    assert!(names[0].ends_with("third"), "{rows:?}");
    assert!(names[1].ends_with("second"), "{rows:?}");
    assert!(names[2].ends_with("first"), "{rows:?}");
    assert!(
        rows[3].contains("a new session after this turn  ·  files are not changed"),
        "{rows:?}"
    );
}

#[test]
fn nothing_to_fork_says_so_and_opens_nothing() {
    let mut app = app_in(Lang::Zh);
    app.open_fork(Vec::new());
    assert!(app.overlay.is_none());
    assert!(app.composer.is_empty());
}

#[test]
fn enter_forks_after_the_chosen_input_and_words_filter() {
    let mut app = app_in(Lang::En);
    app.open_fork(entries());
    // ↓ 到 `second`，⏎ 在它之后分叉，列表关上、草稿清空。
    assert_eq!(app.forks_key(key(KeyCode::Down)), Some(None));
    let action = app.forks_key(key(KeyCode::Enter));
    assert!(
        matches!(&action, Some(Some(Action::Fork { input })) if input.as_str() == "msg_2"),
        "{action:?}"
    );
    assert!(app.overlay.is_none());
    assert!(app.composer.is_empty());
    // 词过滤；别的键去编辑草稿；esc 关上、草稿留着；草稿不再以 `/fork ` 开头时关上。
    app.open_fork(entries());
    app.composer.set_text("/fork fir");
    app.settle_forks("/fork fir");
    let Some(Overlay::Forks(list)) = &app.overlay else {
        panic!("the list is not open")
    };
    assert_eq!(app.fork_matches(list), [0]);
    assert_eq!(app.forks_key(key(KeyCode::Char('s'))), None);
    assert_eq!(app.forks_key(key(KeyCode::Esc)), Some(None));
    assert!(app.overlay.is_none());
    assert_eq!(app.composer.area.text(), "/fork fir");
    app.open_fork(entries());
    app.settle_forks("/for");
    assert!(app.overlay.is_none());
}

/// 带参数时忽略参数（工作线 F78）：与不带参数相同，打开选择器，不显示错误。
#[test]
fn arguments_are_ignored() {
    for text in ["/fork", "/fork latest", "/fork checkpoint_1 more"] {
        let mut app = app_in(Lang::En);
        assert_eq!(app.slash(text), Some(Action::OpenFork), "{text}");
        assert!(app.local.is_empty(), "{text}");
    }
}

/// 分叉后的提示（工作线 F78）：英文是内核的回执（Node 原文）加 Node 的切换提示；中文另配，消息 ID 与条数取回执。
#[test]
fn the_notice_after_a_fork_follows_the_language() {
    let forked = rust_cli_kernel_api::session_api::Forked {
        session: rust_cli_kernel_api::ids::SessionId::new("sess_2"),
        through: Some(MessageId::new("msg_4")),
        copied: 4,
    };
    assert_eq!(
        forked_notice(Lang::En, &forked),
        "Forked session sess_2 from message msg_4: copied 4 messages.\nSwitched to forked session sess_2."
    );
    assert_eq!(
        forked_notice(Lang::Zh, &forked),
        "已从消息 msg_4 分出会话 sess_2：复制了 4 条消息。\n已切换到分出的会话 sess_2。"
    );
}
