use super::super::Composer;
use super::*;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

#[test]
fn image_paths_follow_the_drag_and_drop_shapes() {
    assert_eq!(
        image_path("'/Users/dev/My Shot.PNG' ").as_deref(),
        Some("/Users/dev/My Shot.PNG")
    );
    assert_eq!(
        image_path("file:///Users/dev/a\\ b.jpg").as_deref(),
        Some("/Users/dev/a b.jpg")
    );
    assert_eq!(image_path("/Users/dev/notes.txt"), None);
    assert_eq!(image_path("a.png\nb.png"), None);
}

#[test]
fn formats_come_from_the_file_header() {
    assert_eq!(media_type(PNG), Some("image/png"));
    assert_eq!(media_type(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
    assert_eq!(media_type(b"GIF89a.."), Some("image/gif"));
    assert_eq!(media_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
    assert_eq!(media_type(b"plain"), None);
}

/// 文件头的判断（修复线 F58，kernel-api `image::media_type`，Read 也用它）：PNG 只看前 4 字节。以前这里要
/// 完整的 8 字节签名，与 Read 的判断不同。
#[test]
fn formats_follow_the_shared_header_check() {
    assert_eq!(media_type(b"\x89PNGxxxx"), Some("image/png"));
    assert_eq!(media_type(b"GIF8xa.."), None);
    assert_eq!(media_type(&[0xff, 0xd8, 0xff]), None);
}

#[test]
fn images_share_the_paste_counter_and_drop_when_their_placeholder_goes() {
    let mut composer = Composer::default();
    composer.paste_text(&"x".repeat(900));
    assert_eq!(
        composer.add_image(PNG),
        Added::Inserted("[Image #2]".into())
    );
    assert_eq!(composer.add_image(b"text"), Added::NotImage);
    let text = composer.area.text().to_owned();
    assert!(text.ends_with("[Image #2]"), "{text}");
    assert_eq!(composer.image_blocks(&text).len(), 1);
    assert!(composer.image_blocks("no placeholder").is_empty());
    let huge = [PNG, &vec![0; MAX_IMAGE_BYTES]].concat();
    assert_eq!(composer.add_image(&huge), Added::TooLarge);
}

/// 修复线 F13（tui-input.md §3.1）：插入 `[Image #N]` 不带尾空格，之后键入的第一个
/// 字符不是空白或 `.,?!:;)]` 时才补一个空格；紧跟着再粘一张图片时前面补空格。修前插入
/// `[Image #N] `，接着键入空格就成了两个空格。
#[test]
fn the_space_after_an_image_comes_with_the_next_typed_character() {
    let typed = |keys: &str| {
        let mut composer = Composer::default();
        composer.add_image(PNG);
        for c in keys.chars() {
            composer.before_typing(c, false);
            composer.area.insert(c);
        }
        composer.area.text().to_owned()
    };
    assert_eq!(typed(" what"), "[Image #1] what");
    assert_eq!(typed("what"), "[Image #1] what");
    assert_eq!(typed(", ok"), "[Image #1], ok");
    let mut composer = Composer::default();
    composer.add_image(PNG);
    composer.add_image(PNG);
    assert_eq!(composer.area.text(), "[Image #1] [Image #2]");
    // 粘贴文字与 alt 组合键不补空格，之后也不再补。
    composer.paste_text("x");
    composer.before_typing('y', false);
    assert_eq!(composer.area.text(), "[Image #1] [Image #2]x");
    let mut composer = Composer::default();
    composer.add_image(PNG);
    composer.before_typing('b', true);
    composer.before_typing('c', false);
    assert_eq!(composer.area.text(), "[Image #1]");
}

/// 修复线 F26（tui-input.md §3.1，每次编辑后同步删掉图片）：backspace 整块删掉
/// `[Image #N]` 时图片一起去掉；撤销让占位文字回来时图片一起回来；粘回删掉的文字不找回图片。
#[test]
fn deleting_the_placeholder_detaches_the_image_and_undo_brings_it_back() {
    let now = std::time::Instant::now();
    let mut composer = Composer::default();
    composer.area.insert_str("see ");
    composer.add_image(PNG);
    assert!(composer.has_images());
    composer.area.checkpoint(now, false);
    composer.area.backspace();
    composer.sync_images(false);
    assert_eq!(composer.area.text(), "see ");
    assert!(
        !composer.has_images(),
        "the image goes with its placeholder"
    );
    // 撤销：占位文字与图片都回来。
    composer.area.undo();
    composer.sync_images(true);
    assert_eq!(composer.area.text(), "see [Image #1]");
    assert!(composer.has_images());
    assert_eq!(composer.image_blocks(composer.area.text()).len(), 1);
    // ctrl+w 删掉、ctrl+y 粘回：只有文字回来。
    composer.area.kill_word();
    composer.sync_images(false);
    composer.area.yank();
    composer.sync_images(false);
    assert_eq!(composer.area.text(), "see [Image #1]");
    assert!(!composer.has_images());
    assert!(composer.image_blocks(composer.area.text()).is_empty());
}
