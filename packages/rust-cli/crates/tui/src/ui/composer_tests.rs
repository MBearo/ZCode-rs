// 复制自 apps/zcode-host/crates/tui/src/ui/composer_tests.rs（feat/rs 79b6691c55）。
use super::*;

fn typed(text: &str) -> Composer {
    let mut composer = Composer::default();
    composer.area.insert_str(text);
    composer.refresh_popup();
    composer
}

#[test]
fn long_or_multi_line_pastes_fold_and_pasting_again_expands() {
    let mut c = Composer::default();
    let long = "x".repeat(PASTE_FOLD_CHARS + 1);
    c.area.insert_str("see ");
    assert_eq!(c.paste_text(&long), Pasted::Folded);
    assert_eq!(c.area.text(), "see [Pasted text #1]");
    // 超过 2 行也折叠；tab 换成 4 个空格。
    assert_eq!(c.paste_text("a\r\nb\n\tc"), Pasted::Folded);
    assert_eq!(
        c.area.text(),
        "see [Pasted text #1][Pasted text #2 +3 lines]"
    );
    // 同样的内容再粘贴一次：在原处展开。
    assert_eq!(c.paste_text("a\nb\n\tc"), Pasted::Expanded);
    assert_eq!(c.area.text(), "see [Pasted text #1]a\nb\n    c");
    // 短的直接插入。
    assert_eq!(c.paste_text(" ok\nfine"), Pasted::Inserted);
    let sent = c.submit();
    assert_eq!(sent, format!("see {long}a\nb\n    c ok\nfine"));
    assert!(c.is_empty());
}

#[test]
fn history_and_reverse_search() {
    let mut c = Composer::default();
    c.load_history(vec!["run tests".into()]);
    for text in ["fix lint", "run build"] {
        c.area.insert_str(text);
        c.submit();
    }
    c.area.insert_str("draft");
    assert!(c.history_prev());
    assert_eq!(c.area.text(), "run build");
    assert_eq!(c.history_position(), Some((1, 3)));
    assert!(c.history_prev());
    assert_eq!(c.history_position(), Some((2, 3)));
    assert!(c.history_next());
    assert!(c.history_next());
    assert_eq!(c.area.text(), "draft");
    assert_eq!(c.history_position(), None);
    c.search_update("run".into());
    assert_eq!(c.search_match(), Some("run build"));
    c.search_older();
    assert_eq!(c.search_match(), Some("run tests"));
    c.search_accept();
    assert_eq!(c.area.text(), "run tests");
}

#[test]
fn an_empty_slash_lists_the_most_used_commands_first() {
    let mut c = Composer::default();
    c.load_history(vec![
        "/status".into(),
        "/copy".into(),
        "/status".into(),
        "hello".into(),
    ]);
    c.area.insert_str("/");
    c.refresh_popup();
    let names: Vec<&str> = c
        .slash_matches()
        .iter()
        .map(crate::ui::commands::Entry::name)
        .collect();
    assert_eq!(&names[..3], ["status", "copy", "help"]);
    assert_eq!(names.iter().filter(|n| **n == "status").count(), 1);
    // 有输入时按匹配排，不看用量。
    let mut c = typed("/s");
    c.load_history(vec!["/status".into()]);
    assert_eq!(c.slash_matches()[0].name(), "status");
}

#[test]
fn slash_popup_follows_the_command_name_and_inserts_it() {
    let mut c = typed("/mo");
    assert_eq!(c.popup, Some(Popup::Slash { selected: 0 }));
    assert_eq!(c.accept_slash().as_deref(), Some("model"));
    assert_eq!(c.area.text(), "/model ");
    // 进入参数部分后关闭。
    c.refresh_popup();
    assert_eq!(c.popup, None);
    assert_eq!(typed("hello /mo").popup, None);
}

#[test]
fn accepting_a_typed_alias_keeps_the_alias() {
    // `/stats` 与 `/usage` 是同一个命令，但打开不同的标签页（usage.md §5.1）。
    let mut c = typed("/stats");
    assert_eq!(c.accept_slash().as_deref(), Some("stats"));
    assert_eq!(c.area.text(), "/stats ");
    // 只输入了前缀：照旧补成命令名。
    assert_eq!(typed("/sta").accept_slash().as_deref(), Some("status"));
}
