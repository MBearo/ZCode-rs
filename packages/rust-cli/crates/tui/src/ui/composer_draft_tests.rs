use super::super::{Composer, InputMode};

#[test]
fn the_editor_sees_expanded_pastes_and_they_fold_back() {
    let mut composer = Composer::default();
    composer.area.set_text("see ");
    let long = "one\ntwo\nthree\nfour\nfive\nsix";
    composer.paste_text(long, 80);
    let expanded = composer.expanded_text();
    assert_eq!(expanded, format!("see {long}"));
    // 编辑器加了一个末尾换行：去掉；粘贴内容原样还在：折回占位文字。
    composer.load_edited(&format!("please {expanded}\n"));
    assert_eq!(composer.area.text(), "please see [Pasted · 6 lines]");
    // 以空行结尾时不去掉换行；内容改过的粘贴不再折回。
    composer.load_edited("rewritten\n\n");
    assert_eq!(composer.area.text(), "rewritten\n\n");
    assert_eq!(composer.placeholders().count(), 0);
}

/// 修复线 F65 的 T4：Windows 的编辑器常写 CRLF。读回的文字先统一换行（与粘贴相同），再去掉单个末尾换行、折回粘贴；
/// 以前 `\r` 留在输入里，折叠的粘贴（存的是统一过换行的原文）也折不回去。
#[test]
fn crlf_from_the_editor_is_normalized_like_a_paste() {
    let mut composer = Composer::default();
    composer.paste_text("one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix", 80);
    let expanded = composer.expanded_text();
    assert_eq!(expanded, "one\ntwo\nthree\nfour\nfive\nsix");
    composer.load_edited("ask: one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\n");
    assert_eq!(composer.area.text(), "ask: [Pasted · 6 lines]");
    composer.load_edited("a\r\nb\rc\r\n\r\n");
    assert_eq!(composer.area.text(), "a\nb\nc\n\n");
}

#[test]
fn stash_keeps_the_mode() {
    let mut composer = Composer {
        mode: InputMode::Bash,
        ..Composer::default()
    };
    composer.area.set_text("ls");
    assert!(composer.toggle_stash());
    assert_eq!(composer.mode, InputMode::Prompt);
    assert!(composer.pop_stash());
    assert_eq!(
        (composer.area.text(), composer.mode),
        ("ls", InputMode::Bash)
    );
}
