//! 占位文字是一个整体（docs/specs/rust-cli/tui-input.md §3.1，修复线 F26）：
//! ← / → 整块跳过，backspace / delete 整块删除，词移动与 kill 的范围移出占位文字，↑ / ↓ 与单击落在图片里时移到
//! 较近的一端。旧代码按字符移动与删除，这些测试都失败。
use super::*;

fn area(text: &str) -> TextArea {
    let mut area = TextArea::default();
    area.insert_str(text);
    area
}

#[test]
fn chips_are_the_image_and_paste_placeholders() {
    let text = "a [Image #1] b [Pasted text #2 +3 lines] c [Pasted text #12]";
    let found: Vec<&str> = super::chips::chips(text)
        .into_iter()
        .map(|(start, end)| &text[start..end])
        .collect();
    assert_eq!(
        found,
        [
            "[Image #1]",
            "[Pasted text #2 +3 lines]",
            "[Pasted text #12]"
        ]
    );
    // 不是占位文字的方括号照常是文字。
    for text in [
        "[Image]",
        "[Image #]",
        "[Image #1 +2]",
        "[foo #1]",
        "[Image #1",
    ] {
        assert!(super::chips::chips(text).is_empty(), "{text}");
    }
}

#[test]
fn backspace_at_the_end_and_delete_at_the_start_remove_the_whole_chip() {
    let mut a = area("look [Image #1]");
    a.backspace();
    assert_eq!((a.text(), a.cursor()), ("look ", 5));
    let mut a = area("[Image #2] here");
    a.home();
    a.delete();
    assert_eq!((a.text(), a.cursor()), (" here", 0));
    // 折叠的粘贴也是一个整体（所有占位文字都这样）。
    let mut a = area("x[Pasted text #3 +10 lines]");
    a.backspace();
    assert_eq!(a.text(), "x");
    // 不在边上时照常一个字符。
    let mut a = area("[Image #1] ab");
    a.backspace();
    assert_eq!(a.text(), "[Image #1] a");
}

#[test]
fn arrows_jump_over_chips_and_never_stop_inside() {
    let text = "a[Image #1]b";
    let mut a = area(text);
    a.home();
    let mut stops = vec![a.cursor()];
    for _ in 0..4 {
        a.right();
        stops.push(a.cursor());
    }
    assert_eq!(stops, [0, 1, 11, 12, 12]);
    let mut back = Vec::new();
    for _ in 0..3 {
        a.left();
        back.push(a.cursor());
    }
    assert_eq!(back, [11, 1, 0]);
}

#[test]
fn word_moves_and_kills_step_out_of_chips() {
    let mut a = area("see [Image #1]");
    a.word_left();
    assert_eq!(a.cursor(), 4, "to the start of the chip, not into `1`");
    a.home();
    a.right();
    a.right();
    a.right();
    a.right();
    a.word_right();
    assert_eq!(a.cursor(), 14, "to the end of the chip");
    // ctrl+w 紧跟在占位文字后面：删掉整块，不剩半个。
    let mut a = area("see [Image #1]");
    a.kill_word();
    assert_eq!(a.text(), "see ");
    let mut a = area("[Image #1] go");
    a.set_cursor(0);
    a.kill_word_forward();
    assert_eq!(a.text(), " go");
}

#[test]
fn clicks_and_vertical_moves_into_an_image_go_to_the_nearer_edge() {
    let mut a = area("[Image #1]");
    a.select(2, 2, 2);
    assert_eq!(a.cursor(), 0);
    a.select(8, 8, 8);
    assert_eq!(a.cursor(), 10);
    // 折叠的粘贴不调整（只对图片这样做）。
    let mut a = area("[Pasted text #1]");
    a.select(3, 3, 3);
    assert_eq!(a.cursor(), 3);
    // ↑ / ↓：按目标列落到图片里时移到较近的一端。
    let mut a = area("abcdefgh\n[Image #1]");
    a.set_cursor(3);
    a.vertical(80, true);
    assert_eq!(a.cursor(), 9);
}
