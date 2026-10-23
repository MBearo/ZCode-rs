// 复制自 apps/zcode-host/crates/tui/src/ui/list_tests.rs（feat/rs 79b6691c55）。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::ui::text::plain;

fn item(label: &str, current: bool) -> Item {
    Item {
        label: label.into(),
        description: format!("{label} description"),
        current,
        value: label.into(),
        ..Item::default()
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn text(list: &ListView, width: usize) -> Vec<String> {
    list.lines(
        width,
        20,
        Lang::En,
        &crate::ui::palette::Palette::new(
            crate::ui::palette::ColorLevel::TrueColor,
            Some((0, 0, 0)),
        ),
    )
    .0
    .iter()
    .map(|line| plain(line).trim_end().to_owned())
    .collect()
}

#[test]
fn starts_on_the_current_item_wraps_around_and_chooses_with_enter_or_digits() {
    let mut list = ListView::new(
        Purpose::Permissions,
        "Pick".into(),
        vec![item("a", false), item("b", true)],
        "none".into(),
    );
    assert_eq!(list.selected, 1);
    list.on_key(key(KeyCode::Down), Typing::default());
    assert_eq!(list.selected, 0, "wraps to the top");
    list.on_key(key(KeyCode::Up), Typing::default());
    assert_eq!(list.selected, 1, "wraps to the bottom");
    assert!(
        matches!(list.on_key(key(KeyCode::Enter), Typing::default()), Outcome::Chosen(v) if v == "b")
    );
    assert!(
        matches!(list.on_key(key(KeyCode::Char('1')), Typing::default()), Outcome::Chosen(v) if v == "a")
    );
    assert!(matches!(
        list.on_key(key(KeyCode::Esc), Typing::default()),
        Outcome::Closed
    ));
}

#[test]
fn the_frame_has_a_rule_a_bold_title_a_focus_pointer_and_a_hint() {
    let list = ListView::new(
        Purpose::Permissions,
        "Pick".into(),
        vec![item("a", false), item("b", true)],
        "none".into(),
    )
    .with_subtitle("Choose one".into());
    let lines = text(&list, 40);
    assert_eq!(lines[0], "─".repeat(40));
    assert_eq!(lines[1], "  Pick");
    assert_eq!(lines[2], "  Choose one");
    assert!(
        lines[4].starts_with("    1. a     a description"),
        "{lines:?}"
    );
    assert!(
        lines[5].starts_with("  › 2. b ✔   b description"),
        "{lines:?}"
    );
    assert_eq!(lines.last().unwrap(), "  Enter to select · Esc to cancel");
}

#[test]
fn models_have_one_row_each_and_arrows_change_the_effort() {
    let model = |label: &str, current: bool, level: usize| Item {
        label: label.into(),
        description: "Z.ai".into(),
        current,
        value: format!("zai{SEP}{label}"),
        levels: vec!["low".into(), "medium".into(), "high".into()],
        level,
    };
    let mut list = ListView::new(
        Purpose::Model,
        "Select model".into(),
        vec![model("GLM-5", true, 2), model("GLM-4", false, 0)],
        "none".into(),
    );
    let lines = text(&list, 60);
    assert!(
        lines.iter().any(|l| l == "  ◉ high effort  ←/→ to adjust"),
        "{lines:?}"
    );
    list.on_key(key(KeyCode::Left), Typing::default());
    let lines = text(&list, 60);
    assert!(
        lines
            .iter()
            .any(|l| l == "  ● medium effort  ←/→ to adjust"),
        "{lines:?}"
    );
    assert!(
        matches!(list.on_key(key(KeyCode::Enter), Typing::default()), Outcome::Chosen(v) if v == format!("zai{SEP}GLM-5{SEP}medium"))
    );
    // 数字选中别的模型，用它自己的档位。
    assert!(
        matches!(list.on_key(key(KeyCode::Char('2')), Typing::default()), Outcome::Chosen(v) if v == format!("zai{SEP}GLM-4{SEP}low"))
    );
}

#[test]
fn two_line_lists_search_and_two_lines_per_item() {
    let mut list = ListView::new(
        Purpose::Fork,
        "Resume session".into(),
        vec![
            Item {
                label: "alpha".into(),
                description: "5m ago".into(),
                current: true,
                value: "a".into(),
                ..Item::default()
            },
            Item {
                label: "beta".into(),
                description: "2h ago".into(),
                value: "b".into(),
                ..Item::default()
            },
        ],
        "none".into(),
    )
    .filterable();
    let lines = text(&list, 60);
    assert_eq!(lines[2], "  ⌕ Search…");
    assert!(lines.contains(&"  › alpha".to_owned()), "{lines:?}");
    assert!(
        lines.contains(&"    5m ago · current".to_owned()),
        "{lines:?}"
    );
    list.on_key(key(KeyCode::Char('b')), Typing::default());
    assert_eq!(list.visible().len(), 1);
    assert!(
        matches!(list.on_key(key(KeyCode::Enter), Typing::default()), Outcome::Chosen(v) if v == "b")
    );
}

#[test]
fn the_wheel_moves_the_focus_and_stops_at_the_ends() {
    let mut list = ListView::new(
        Purpose::Permissions,
        "Pick".into(),
        vec![item("a", true), item("b", false), item("c", false)],
        "none".into(),
    );
    list.wheel(-1);
    assert_eq!(list.selected, 0, "stops at the top instead of wrapping");
    list.wheel(1);
    list.wheel(1);
    assert_eq!(list.selected, 2);
    list.wheel(1);
    assert_eq!(list.selected, 2, "stops at the bottom instead of wrapping");
}

#[test]
fn models_are_searchable_and_digits_go_into_the_search() {
    let model = |label: &str, provider: &str, current: bool| Item {
        label: label.into(),
        description: provider.into(),
        current,
        value: format!("p{SEP}{label}"),
        levels: vec!["low".into(), "high".into()],
        level: 1,
    };
    let mut list = ListView::new(
        Purpose::Model,
        "Select model".into(),
        vec![
            model("GLM-5", "Z.ai", false),
            model("GLM-4.6", "Z.ai", true),
            model("Kimi K2", "Moonshot", false),
        ],
        "none".into(),
    )
    .with_subtitle("Applies to this session".into())
    .filterable();
    // 打开时焦点仍在当前模型上；搜索行在副标题下面，条目不写编号。
    assert_eq!(list.selected, 1);
    let lines = text(&list, 60);
    assert_eq!(lines[3], "  ⌕ Search…");
    assert!(
        lines.iter().any(|l| l.starts_with("  › GLM-4.6 ✔")),
        "{lines:?}"
    );
    // 数字输入到搜索里，不直接选中。
    assert!(matches!(
        list.on_key(key(KeyCode::Char('4')), Typing::default()),
        Outcome::Pending
    ));
    assert_eq!(list.visible().len(), 1);
    assert!(
        matches!(list.on_key(key(KeyCode::Enter), Typing::default()), Outcome::Chosen(v) if v == format!("p{SEP}GLM-4.6{SEP}high"))
    );
    // 也按 provider 过滤，不区分大小写。
    list.on_key(key(KeyCode::Backspace), Typing::default());
    for c in "MOON".chars() {
        list.on_key(key(KeyCode::Char(c)), Typing::default());
    }
    let visible: Vec<_> = list.visible().iter().map(|i| i.label.clone()).collect();
    assert_eq!(visible, ["Kimi K2"]);
    // 有条目但没有匹配：写 No matches，不用列表为空时的说法。
    list.on_key(key(KeyCode::Char('x')), Typing::default());
    let lines = text(&list, 60);
    assert!(lines.contains(&"  No matches".to_owned()), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("none")), "{lines:?}");
}
