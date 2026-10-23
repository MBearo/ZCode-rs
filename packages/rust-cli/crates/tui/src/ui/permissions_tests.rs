//! `/permissions` 页面的界面状态与排版（tui-manage.md §1.2）。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::{RuleBehavior, RuleChange, RuleSource, SourcedRule};

use super::*;

fn rule(source: RuleSource, behavior: RuleBehavior, text: &str) -> SourcedRule {
    SourcedRule {
        source,
        behavior,
        rule: rust_cli_runtime::parse_permission_rule(text),
    }
}

fn view() -> PermissionsView {
    PermissionsView::new(
        vec![
            rule(RuleSource::LocalSettings, RuleBehavior::Allow, "read"),
            rule(
                RuleSource::UserSettings,
                RuleBehavior::Allow,
                "Bash(git status:*)",
            ),
            rule(RuleSource::ProjectSettings, RuleBehavior::Deny, "WebFetch"),
        ],
        "/Users/dev/project".to_owned(),
    )
}

fn key(view: &mut PermissionsView, code: KeyCode) -> Outcome {
    view.on_key(KeyEvent::new(code, KeyModifiers::NONE), Typing::default())
}

fn text(view: &PermissionsView) -> String {
    super::super::permissions_lines::lines(
        view,
        Lang::En,
        &crate::ui::palette::Palette::new(
            crate::ui::palette::ColorLevel::TrueColor,
            Some((0, 0, 0)),
        ),
        100,
        30,
    )
    .iter()
    .map(super::super::text::plain)
    .collect::<Vec<_>>()
    .join("\n")
}

#[test]
fn tabs_list_rules_sorted_with_the_add_entry_first() {
    let mut view = view();
    let screen = text(&view);
    assert!(screen.contains("Permissions:  Allow"), "{screen}");
    assert!(screen.contains("ZCode won't ask before using allowed tools."));
    let add = screen.find("› Add a new rule…").unwrap();
    let bash = screen.find("Bash(git status:*)").unwrap();
    let read = screen.find("    read").unwrap();
    assert!(add < bash && bash < read, "{screen}");
    key(&mut view, KeyCode::Right);
    key(&mut view, KeyCode::Right);
    let screen = text(&view);
    assert!(screen.contains("ZCode will always reject requests to use denied tools."));
    assert!(screen.contains("WebFetch"));
    key(&mut view, KeyCode::Right);
    let screen = text(&view);
    assert!(
        screen.contains("- /Users/dev/project (Original working directory)"),
        "{screen}"
    );
    // 输入可见字符开始过滤：不列 Add a new rule…，esc 先清空过滤。
    key(&mut view, KeyCode::Right);
    key(&mut view, KeyCode::Char('r'));
    key(&mut view, KeyCode::Char('e'));
    let screen = text(&view);
    assert!(!screen.contains("Add a new rule"), "{screen}");
    assert!(screen.contains("› read"), "{screen}");
    assert!(matches!(key(&mut view, KeyCode::Esc), Outcome::Pending));
    assert!(matches!(key(&mut view, KeyCode::Esc), Outcome::Close));
}

#[test]
fn adding_goes_through_the_input_and_the_destination() {
    let mut view = view();
    key(&mut view, KeyCode::Right);
    key(&mut view, KeyCode::Enter);
    assert!(text(&view).contains("Add ask permission rule"));
    assert!(text(&view).contains("Enter permission rule…"));
    // 只有空白时不提交。
    key(&mut view, KeyCode::Char(' '));
    key(&mut view, KeyCode::Enter);
    assert!(matches!(view.stage, Stage::Input(_)));
    for c in "Bash(rm *)".chars() {
        key(&mut view, KeyCode::Char(c));
    }
    key(&mut view, KeyCode::Enter);
    let screen = text(&view);
    assert!(
        screen.contains("Where should this rule be saved?"),
        "{screen}"
    );
    assert!(
        screen.contains("Any Bash command starting with rm"),
        "{screen}"
    );
    assert!(screen.contains("1. Project settings (local)"));
    let Outcome::Change(change) = key(&mut view, KeyCode::Enter) else {
        panic!("no change");
    };
    assert_eq!(
        change,
        RuleChange::Add {
            behavior: RuleBehavior::Ask,
            rules: vec!["Bash(rm *)".to_owned()],
        }
    );
    assert_eq!(view.stage, Stage::Saving);
    assert!(matches!(key(&mut view, KeyCode::Esc), Outcome::Pending));
    let mut rules = view.rules.clone();
    rules.push(rule(
        RuleSource::LocalSettings,
        RuleBehavior::Ask,
        "Bash(rm *)",
    ));
    view.changed(&change, Ok(rules), Lang::En);
    assert_eq!(view.stage, Stage::List);
    assert_eq!(view.results, ["Added ask rule Bash(rm *)"]);
    assert!(text(&view).contains("Bash(rm *)"));
}

#[test]
fn deleting_asks_first_and_config_rules_are_read_only() {
    let mut view = view();
    // 焦点移到 read（LocalSettings）。
    key(&mut view, KeyCode::Down);
    key(&mut view, KeyCode::Down);
    key(&mut view, KeyCode::Enter);
    let screen = text(&view);
    assert!(screen.contains("Delete allowed tool?"), "{screen}");
    assert!(screen.contains("Any use of the read tool"), "{screen}");
    assert!(screen.contains("From project local settings"));
    assert!(screen.contains("› No"));
    // 焦点在 No：enter 回到列表。
    assert!(matches!(key(&mut view, KeyCode::Enter), Outcome::Pending));
    assert_eq!(view.stage, Stage::List);
    key(&mut view, KeyCode::Enter);
    key(&mut view, KeyCode::Up);
    let Outcome::Change(change) = key(&mut view, KeyCode::Enter) else {
        panic!("no change");
    };
    assert!(
        matches!(&change, RuleChange::Remove { rule } if rule.source == RuleSource::LocalSettings)
    );
    view.changed(&change, Err("disk full".to_owned()), Lang::En);
    assert_eq!(
        view.results,
        ["Could not delete read — the rule is unchanged"]
    );
    // 配置文件里的规则：只读详情，enter 不改。
    key(&mut view, KeyCode::Up);
    key(&mut view, KeyCode::Enter);
    let screen = text(&view);
    assert!(screen.contains("Rule details"), "{screen}");
    assert!(screen.contains("From user settings"));
    assert!(
        screen.contains("read-only source (the user config file)"),
        "{screen}"
    );
    assert!(matches!(key(&mut view, KeyCode::Enter), Outcome::Pending));
    assert_eq!(view.stage, Stage::List);
}
