use rust_cli_kernel_api::message::{ContentBlock, ImageSource};

use super::super::{Composer, InputMode};

/// 文字放回、光标在末尾；`!` 开头回到 bash 模式（tui-input.md §8）。
#[test]
fn the_text_comes_back_with_the_cursor_at_the_end_and_bash_mode() {
    let mut composer = Composer::default();
    composer.restore_input("First prompt.", Vec::new());
    assert_eq!(composer.area.text(), "First prompt.");
    assert_eq!(composer.area.cursor(), "First prompt.".len());
    assert_eq!(composer.mode, InputMode::Prompt);
    composer.clear();
    composer.restore_input("!sleep 30", Vec::new());
    assert_eq!(composer.area.text(), "sleep 30");
    assert_eq!(composer.mode, InputMode::Bash);
}

/// 图片按编号配给 `[Image #N]`，再发送时还在；之后的粘贴编号接着往后排。
#[test]
fn images_come_back_with_their_placeholders() {
    let mut composer = Composer::default();
    let images = vec![
        ("image/png".to_owned(), "AAAA".to_owned()),
        ("image/jpeg".to_owned(), "BBBB".to_owned()),
    ];
    composer.restore_input("[Image #3] and [Image #2] please", images);
    let blocks = composer.image_blocks(composer.area.text());
    let data: Vec<&str> = blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Image {
                source: ImageSource::Base64 { data, .. },
            } => data.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        data,
        ["AAAA", "BBBB"],
        "#2 is the first image, #3 the second"
    );
    let png = b"\x89PNG\r\n\x1a\nrest";
    let added = composer.add_image(png);
    assert_eq!(
        added,
        super::super::images::Added::Inserted("[Image #4]".to_owned())
    );
}

/// 回执之前键入的字接在放回的文字后面一行；历史去掉刚发送的那一条（发送时与上一条相同时不删）。
#[test]
fn typed_text_is_kept_and_the_submitted_entry_is_forgotten() {
    let mut composer = Composer::default();
    composer.load_history(vec!["older".to_owned()]);
    composer.area.set_text("First prompt.");
    assert_eq!(composer.submit(), "First prompt.");
    composer.area.set_text("more");
    composer.restore_input("First prompt.", Vec::new());
    assert_eq!(composer.area.text(), "First prompt.\nmore");
    composer.forget_submitted();
    composer.clear();
    assert!(composer.history_prev());
    assert_eq!(composer.area.text(), "older");
    // 与上一条相同的发送没有记进历史，撤回时也不删上一条。
    let mut same = Composer::default();
    same.load_history(vec!["again".to_owned()]);
    same.area.set_text("again");
    same.submit();
    same.forget_submitted();
    assert!(same.history_prev());
    assert_eq!(same.area.text(), "again");
}
