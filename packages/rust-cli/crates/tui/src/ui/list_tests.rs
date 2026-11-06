// 复制自 apps/zcode-host/crates/tui/src/ui/list_tests.rs（feat/rs 79b6691c55）。
use crossterm::event::KeyCode;
use rust_cli_base::models::{GLM_4_6_ACCOUNT, GLM_4_ACCOUNT, GLM_5_ACCOUNT};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_tui_kit::render::{StyledLine, attr};

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::ink::Ink;
use crate::ui::lines::plain;
use crate::ui::probe::ColorLevel;
use crate::ui::test_support::key;

fn ink() -> Ink {
    Ink::new(
        ColorLevel::TrueColor,
        Some((255, 255, 255)),
        Some((0, 0, 0)),
    )
}

fn item(label: &str, current: bool) -> Item {
    Item {
        label: label.into(),
        description: format!("{label} description"),
        current,
        value: Choice::Theme(label.into()),
        ..Item::default()
    }
}

/// 选中的值是 `Choice::Theme(label)`（测试用它当带名字的值）。
fn chose(outcome: Outcome, label: &str) -> bool {
    matches!(outcome, Outcome::Chosen { choice: Choice::Theme(v), level: None } if v == label)
}

/// 选中的是这个模型、这一档。
fn chose_model(outcome: Outcome, model: &str, effort: &str) -> bool {
    matches!(
        outcome,
        Outcome::Chosen { choice: Choice::Model(selection), level: Some(level) }
            if selection.model == model && level == effort
    )
}

fn model_choice(provider: &str, model: &str) -> Choice {
    Choice::Model(ModelSelection::new(provider, model))
}

fn text(list: &ListView, width: usize) -> Vec<String> {
    list.lines(width, Lang::En, &ink())
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
    list.on_key(key(KeyCode::Down));
    assert_eq!(list.selected, 0, "wraps to the top");
    list.on_key(key(KeyCode::Up));
    assert_eq!(list.selected, 1, "wraps to the bottom");
    assert!(chose(list.on_key(key(KeyCode::Enter)), "b"));
    assert!(chose(list.on_key(key(KeyCode::Char('1'))), "a"));
    assert!(matches!(list.on_key(key(KeyCode::Esc)), Outcome::Closed));
}

/// 一层（tui-kit.md §4.0「板」，Puffin 输入框的板上的那一层）：板的上下沿由底部栈画，按键提示在槽里；这一层是头一行（标题
/// 与放得下的副标题）与从第 3 列起的条目（没有指针与序号；当前值后面 `current`）。模型、权限、导出与回退的列表没有头一行。
#[test]
fn the_layer_has_a_head_and_rows_from_column_three() {
    let list = |purpose: Purpose| {
        ListView::new(
            purpose,
            "Pick".into(),
            vec![item("a", false), item("b", true)],
            "none".into(),
        )
        .with_subtitle("Choose one".into())
    };
    let lines = text(&list(Purpose::Info), 40);
    assert_eq!(
        lines,
        [
            "   Pick   Choose one",
            "   a   a description",
            "   b   b description   current"
        ]
    );
    let lines = text(&list(Purpose::Export), 60);
    assert_eq!(
        lines,
        [
            format!("   {:<21}a description", "a"),
            format!("   {:<21}b description   current", "b"),
        ]
    );
}

/// 条目的标签（Puffin 模式列表的 `on` / `off`）代替 `current`；权限与导出的名字补到 21 格（Puffin `mode_list_rows`、
/// `export_rows`）。
#[test]
fn a_tag_replaces_current_and_mode_names_pad_to_twenty_one() {
    let tagged = Item {
        tag: "off".into(),
        ..item("Plan", true)
    };
    let list = ListView::new(
        Purpose::Permissions,
        String::new(),
        vec![item("Ask", true), tagged],
        "none".into(),
    );
    assert_eq!(
        text(&list, 60),
        [
            format!("   {:<21}Ask description   current", "Ask"),
            format!("   {:<21}Plan description   off", "Plan"),
        ]
    );
}

/// 墨色（tui-kit.md §4.2）：头 fg2 粗体、副标题 fg3；选中的名字是 `chosen`（rich 是 brand）、说明 fg2；其余名字 fg2、
/// 说明 fg3；`current` fg3。panel 底由板铺（底部栈）。
#[test]
fn the_layer_takes_puffins_inks() {
    let ink = ink();
    let theme = ink.theme;
    let list = ListView::new(
        Purpose::Info,
        "Pick".into(),
        vec![item("a", true), item("b", false)],
        "none".into(),
    )
    .with_subtitle("Choose one".into());
    let (lines, rows) = list.lines(40, Lang::En, &ink);
    let head = &lines[0];
    let style_of = |line: &StyledLine, text: &str| {
        line.spans
            .iter()
            .find(|span| span.text.contains(text))
            .map(|span| span.style)
    };
    assert_eq!(
        style_of(head, "Pick"),
        Some(theme.fg2.with_attr(attr::BOLD))
    );
    assert_eq!(style_of(head, "Choose"), Some(theme.fg3));
    let focused = &lines[rows[0].0];
    assert_eq!(style_of(focused, "a "), Some(ink.selection()));
    assert_eq!(style_of(focused, "a description"), Some(theme.fg2));
    assert_eq!(style_of(focused, "current"), Some(theme.fg3));
    let other = &lines[rows[1].0];
    assert_eq!(style_of(other, "b "), Some(theme.fg2));
    assert_eq!(style_of(other, "b description"), Some(theme.fg3));
}

/// plain（16 色）：选中的名字 brand 加粗（板的上下沿是 line 色的 `─`，由底部栈画）。
#[test]
fn plain_terminals_choose_in_bold() {
    let ink = Ink::new(ColorLevel::Ansi16, Some((255, 255, 255)), Some((0, 0, 0)));
    let list = ListView::new(
        Purpose::Export,
        "Pick".into(),
        vec![item("a", true)],
        "none".into(),
    );
    let (lines, rows) = list.lines(30, Lang::En, &ink);
    let focused = &lines[rows[0].0];
    assert_eq!(focused.spans[1].style, ink.selection());
    assert!(ink.selection().has_attr(attr::BOLD));
}

/// `/mode` 的选择器（Puffin `/mode`）：每个模式一行，名字补到最长的名字之后再空三格，然后是说明；当前的后面 `current`。
#[test]
fn the_mode_picker_has_one_row_per_mode() {
    let mode = |label: &str, mode: PermissionMode, current: bool| Item {
        label: label.into(),
        description: format!("{label} description"),
        current,
        value: Choice::Mode(mode),
        ..Item::default()
    };
    let list = ListView::new(
        Purpose::Permissions,
        "Mode".into(),
        vec![
            mode("Ask before changes", PermissionMode::Default, true),
            mode("Full access", PermissionMode::BypassPermissions, false),
        ],
        "none".into(),
    );
    let lines = text(&list, 80);
    assert_eq!(
        lines[0],
        "   Ask before changes   Ask before changes description   current"
    );
    assert_eq!(lines[1], "   Full access          Full access description");
}

#[test]
fn models_have_one_row_each_and_arrows_change_the_effort() {
    let model = |label: &str, current: bool, level: usize| Item {
        label: label.into(),
        description: "Z.ai".into(),
        current,
        value: model_choice("zai", label),
        levels: vec!["low".into(), "medium".into(), "high".into()],
        level,
        tag: String::new(),
    };
    let mut list = ListView::new(
        Purpose::Model,
        "Select model".into(),
        vec![
            model(GLM_5_ACCOUNT, true, 2),
            model(GLM_4_ACCOUNT, false, 0),
        ],
        "none".into(),
    );
    // 档位行（Puffin 的 Steps）：窄于 64 列时没有 provider 列，标签 `Effort ` 后面是条与档位名。
    let lines = text(&list, 60);
    assert!(
        lines.iter().any(|l| l == "   Effort  ▄▄ ▅▅ ▆▆  high"),
        "{lines:?}"
    );
    list.on_key(key(KeyCode::Left));
    let lines = text(&list, 60);
    assert!(
        lines.iter().any(|l| l == "   Effort  ▄▄ ▅▅ ▆▆  medium"),
        "{lines:?}"
    );
    // 宽时有 provider 列（只在一组的第一行写出），当前模型后面 `current`（槽的 `←→ effort` 由 `pane.rs` 画）。
    let lines = text(&list, 80);
    assert_eq!(lines[0], format!("   Z.ai   {GLM_5_ACCOUNT}   current"));
    assert_eq!(lines[1], format!("          {GLM_4_ACCOUNT}"));
    assert_eq!(lines[2], "   Effort ▄▄ ▅▅ ▆▆  medium");
    assert!(chose_model(
        list.on_key(key(KeyCode::Enter)),
        GLM_5_ACCOUNT,
        "medium"
    ));
    // 数字选中别的模型，用它自己的档位。
    assert!(chose_model(
        list.on_key(key(KeyCode::Char('2'))),
        GLM_4_ACCOUNT,
        "low"
    ));
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
        value: model_choice("p", label),
        levels: vec!["low".into(), "high".into()],
        level: 1,
        tag: String::new(),
    };
    let mut list = ListView::new(
        Purpose::Model,
        "Select model".into(),
        vec![
            model(GLM_5_ACCOUNT, "Z.ai", false),
            model(GLM_4_6_ACCOUNT, "Z.ai", true),
            model("Kimi K2", "Moonshot", false),
        ],
        "none".into(),
    )
    .with_subtitle("Applies to this session".into())
    .filterable();
    // 打开时焦点仍在当前模型上（过滤的词在草稿里，tui-kit.md §4.0；`ListView` 自己的过滤在这里直接按键）。
    assert_eq!(list.selected, 1);
    let lines = text(&list, 60);
    assert!(
        lines
            .iter()
            .any(|l| l == &format!("   {GLM_4_6_ACCOUNT}   current")),
        "{lines:?}"
    );
    // 数字输入到搜索里，不直接选中。
    assert!(matches!(
        list.on_key(key(KeyCode::Char('4'))),
        Outcome::Pending
    ));
    assert_eq!(list.visible().len(), 1);
    assert!(chose_model(
        list.on_key(key(KeyCode::Enter)),
        GLM_4_6_ACCOUNT,
        "high"
    ));
    // 也按 provider 过滤，不区分大小写。
    list.on_key(key(KeyCode::Backspace));
    for c in "MOON".chars() {
        list.on_key(key(KeyCode::Char(c)));
    }
    let visible: Vec<_> = list.visible().iter().map(|i| i.label.clone()).collect();
    assert_eq!(visible, ["Kimi K2"]);
    // 有条目但没有匹配：写 No matches，不用列表为空时的说法。
    list.on_key(key(KeyCode::Char('x')));
    let lines = text(&list, 60);
    assert!(lines.contains(&"   No matches".to_owned()), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("none")), "{lines:?}");
}
