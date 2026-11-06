//! `/rewind` 的点列表：点的规则（上面的回答）、最新的在上、⏎ 开始挑选、esc 关上、跟着草稿过滤。
use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock, UserMessage, UserOrigin};

use super::*;
use crate::ui::test_support::{ctrl, key};

fn user(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        origin: UserOrigin::Human,
    })
}

fn answer(id: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: "ok".into() }],
        model: "scripted".into(),
        stop_reason: None,
        usage: None,
    })
}

fn option(id: &str) -> RewindOption {
    RewindOption {
        message: MessageId::new(id),
        files: Vec::new(),
        code: false,
    }
}

fn app() -> App {
    let mut app = crate::ui::test_support::app();
    app.session = Some("s".into());
    app
}

/// 点上面最近的回答（Puffin host 的 `UserMark::answer`）：第一条没有；两条你的消息之间没有回答时取更早的那条。
#[test]
fn each_point_forks_on_the_latest_answer_above_it() {
    let messages = [
        user("u0", "first"),
        answer("a0"),
        user("u1", "second"),
        user("u2", "third"),
    ];
    let entries = entries_of(vec![option("u0"), option("u1"), option("u2")], &messages);
    let answers: Vec<Option<&str>> = entries
        .iter()
        .map(|e| e.answer.as_ref().map(MessageId::as_str))
        .collect();
    assert_eq!(answers, [None, Some("a0"), Some("a0")]);
    assert_eq!(entries[1].text, "second");
}

#[test]
fn the_list_follows_the_draft_and_enter_starts_picking() {
    let mut app = app();
    let messages = [user("u0", "first"), answer("a0"), user("u1", "second")];
    app.open_rewind(entries_of(vec![option("u0"), option("u1")], &messages));
    assert_eq!(app.composer.area.text(), DRAFT);
    assert!(matches!(app.overlay, Some(Overlay::Points(_))));
    // 最新的在上：选中的是 `second`。
    assert_eq!(app.point_matches(""), [1, 0]);
    // ↓ 到 `first`，⏎ 在它上面开始挑选，列表关上。
    assert_eq!(app.points_key(key(KeyCode::Down)), Some(None));
    assert_eq!(app.points_key(key(KeyCode::Enter)), Some(None));
    assert!(app.overlay.is_none());
    assert_eq!(app.picking.as_ref().map(|p| p.point), Some(0));
}

#[test]
fn words_filter_and_esc_keeps_the_draft() {
    let mut app = app();
    let messages = [user("u0", "first"), answer("a0"), user("u1", "second")];
    app.open_rewind(entries_of(vec![option("u0"), option("u1")], &messages));
    app.composer.set_text("/rewind fir");
    app.settle_points("/rewind fir");
    assert_eq!(app.point_matches("fir"), [0]);
    // 别的键去编辑草稿。
    assert_eq!(app.points_key(key(KeyCode::Char('s'))), None);
    assert_eq!(app.points_key(key(KeyCode::Esc)), Some(None));
    assert!(app.overlay.is_none());
    assert_eq!(app.composer.area.text(), "/rewind fir");
    // 草稿不再以 `/rewind ` 开头时关上。
    app.open_rewind(Vec::new());
    app.settle_points("/rew");
    assert!(app.overlay.is_none());
}

fn rows_text(app: &App) -> Vec<String> {
    let lines = rust_cli_tui_kit::app::layout::Layout::lay(
        &rust_cli_tui_kit::app::element::stack(app.points_rows(0)),
        80,
    );
    lines
        .iter()
        .map(rust_cli_tui_kit::render::StyledLine::plain)
        .collect()
}

/// ⌃r：选中的点上原地回退的一问（旧界面的能力，tui-kit.md §4.3）。有可以恢复的代码时三个回答，代码的回答写改动的行数；
/// 选代码时发内核的 `Rewind`（只恢复代码），列表关上、草稿清空。esc 回到列表。
#[test]
fn ctrl_r_asks_what_to_rewind_in_place() {
    let mut app = app();
    let messages = [user("u0", "first"), answer("a0"), user("u1", "second")];
    let mut code = option("u1");
    code.code = true;
    code.files = vec![rust_cli_kernel_api::session_api::FileChange {
        path: "a.rs".into(),
        additions: 3,
        deletions: 1,
        machine: Default::default(),
    }];
    app.open_rewind(entries_of(vec![option("u0"), code], &messages));
    assert_eq!(app.points_key(ctrl('r')), Some(None));
    let text = rows_text(&app);
    assert!(
        text[0].contains("Rewind in place to before your 2nd message?"),
        "{text:#?}"
    );
    assert!(
        text.iter().any(|l| l.contains("1  Conversation")),
        "{text:#?}"
    );
    assert!(
        text.iter()
            .any(|l| l.contains("2  Code") && l.contains("files are restored +3 −1")),
        "{text:#?}"
    );
    assert!(text.iter().any(|l| l.contains("3  Both")), "{text:#?}");
    // esc 回到列表；再问一次，选 2。
    assert_eq!(app.points_key(key(KeyCode::Esc)), Some(None));
    assert!(matches!(&app.overlay, Some(Overlay::Points(l)) if !l.asking()));
    app.points_key(ctrl('r'));
    let action = app.points_key(key(KeyCode::Char('2')));
    assert_eq!(
        action,
        Some(Some(Action::Rewind {
            target: MessageId::new("u1"),
            scope: RewindScope::Code,
            text: "second".into(),
        }))
    );
    assert!(app.overlay.is_none());
    assert!(app.composer.area.text().is_empty());
}

/// 这个点之后没有可以恢复的代码：只有「对话」一个回答。
#[test]
fn without_a_checkpoint_only_the_conversation_can_be_rewound() {
    let mut app = app();
    let messages = [user("u0", "first")];
    app.open_rewind(entries_of(vec![option("u0")], &messages));
    app.points_key(ctrl('r'));
    let text = rows_text(&app);
    assert!(text[0].contains("your 1st message"), "{text:#?}");
    assert!(!text.iter().any(|l| l.contains("Code")), "{text:#?}");
    assert_eq!(
        app.points_key(key(KeyCode::Enter)),
        Some(Some(Action::Rewind {
            target: MessageId::new("u0"),
            scope: RewindScope::Conversation,
            text: "first".into(),
        }))
    );
}
