// 复制自 apps/zcode-host/crates/tui/src/ui/composer_tests.rs（feat/rs 79b6691c55）。
use super::*;

fn typed(text: &str) -> Composer {
    let mut composer = Composer::default();
    composer.area.insert_str(text);
    composer.refresh_popup();
    composer
}

#[test]
fn pastes_taller_than_five_rows_fold_into_a_cap_and_pasting_again_expands() {
    let mut c = Composer::default();
    // 一行、在 80 格宽的文字区里折出十二行：折成帽，大小写字数。
    let long = "x".repeat(900);
    c.area.insert_str("see ");
    assert_eq!(c.paste_text(&long, 80), Pasted::Folded);
    assert_eq!(c.area.text(), "see [Pasted · 900 chars]");
    // 五个换行也折叠；tab 换成 4 个空格。
    let tall = "a\r\nb\nc\nd\ne\n\tf";
    assert_eq!(c.paste_text(tall, 80), Pasted::Folded);
    assert_eq!(c.area.text(), "see [Pasted · 900 chars][Pasted · 6 lines]");
    // 同样的内容再粘贴一次：在原处展开。
    assert_eq!(c.paste_text("a\nb\nc\nd\ne\n\tf", 80), Pasted::Expanded);
    assert_eq!(
        c.area.text(),
        "see [Pasted · 900 chars]a\nb\nc\nd\ne\n    f"
    );
    // 不比五行高的直接插入（以前两行以上就折叠）。
    assert_eq!(c.paste_text(" ok\nfine\nthree", 80), Pasted::Inserted);
    let sent = c.submit();
    assert_eq!(
        sent,
        format!("see {long}a\nb\nc\nd\ne\n    f ok\nfine\nthree")
    );
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
fn an_empty_slash_lists_puffins_order_not_the_most_used() {
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
    // Puffin 的次序（目标、压缩、计划……），不按用量排（以前最常用的排在前面）。
    assert_eq!(&names[..3], ["goal", "compact", "plan"]);
    // 别名不单列（`/status` 是 `/stats` 的别名）。
    assert!(!names.contains(&"status"), "{names:?}");
    // 有输入时按匹配排：名字以它开头的在前。
    let c = typed("/st");
    assert_eq!(c.slash_matches()[0].name(), "stats");
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
    // 不带参数的命令补全后不加空格（Puffin `words`）。
    assert_eq!(c.area.text(), "/stats");
    // 只输入了前缀：补成命令名（别名不单列，Puffin `shelf::matches`）。
    assert_eq!(typed("/sta").accept_slash().as_deref(), Some("stats"));
}

#[test]
fn a_chosen_conversation_is_one_block_in_the_draft_and_goes_out_as_its_link() {
    use rust_cli_tui_kit::app::widgets::at_panel::AtItem;
    use rust_cli_tui_kit::app::widgets::at_panel::data::ConversationRef;
    let mut c = typed("read @fix");
    let item = AtItem::Conversation(ConversationRef {
        session_id: "sess_ab".into(),
        title: "Fix [it]".into(),
        updated_at: 1,
    });
    c.insert_at(&item);
    assert_eq!(c.area.text(), "read @Fix [it] ");
    assert_eq!(c.area.cursor(), c.area.text().len());
    assert!(c.popup.is_none());
    assert_eq!(c.area.marks()[0].kind, "conversation");
    assert_eq!(c.submit(), r"read [#Fix \[it\]](#sess_ab)");
    // 历史里是打出的样子。
    assert_eq!(c.history.last().map(String::as_str), Some("read @Fix [it]"));
    // 没有标题的会话只发 id。
    let mut c = typed("@");
    c.insert_at(&AtItem::Conversation(ConversationRef {
        session_id: "sess_cd".into(),
        title: String::new(),
        updated_at: 1,
    }));
    assert_eq!(c.area.text(), "@Untitled ");
    assert_eq!(c.submit(), "#sess_cd");
}
