// 复制自 apps/zcode-host/crates/tui/src/ui/textarea_tests.rs（feat/rs 79b6691c55）。
use super::*;

fn area(text: &str) -> TextArea {
    let mut area = TextArea::default();
    area.insert_str(text);
    area
}

#[test]
fn editing_moves_by_characters_not_bytes() {
    let mut a = area("你好ab");
    a.left();
    a.left();
    a.backspace();
    assert_eq!(a.text(), "你ab");
    a.home();
    a.delete();
    assert_eq!(a.text(), "ab");
    a.end();
    a.insert('!');
    assert_eq!(a.text(), "ab!");
}

#[test]
fn words_and_kills_follow_emacs_bindings() {
    let mut a = area("hello brave world");
    a.word_left();
    assert_eq!(a.cursor(), 12);
    a.word_left();
    a.word_right();
    assert_eq!(a.cursor(), 11);
    a.end();
    a.kill_word();
    assert_eq!(a.text(), "hello brave ");
    a.yank();
    assert_eq!(a.text(), "hello brave world");
    a.home();
    a.word_right();
    a.kill_to_end();
    assert_eq!(a.text(), "hello");
    a.kill_to_start();
    assert_eq!(a.text(), "");
    a.yank();
    assert_eq!(a.text(), "hello");
}

#[test]
fn kill_to_end_at_the_end_of_a_line_joins_the_next_line() {
    let mut a = area("one\ntwo");
    a.home();
    a.left();
    a.kill_to_end();
    assert_eq!(a.text(), "onetwo");
}

#[test]
fn rows_wrap_at_spaces_and_track_the_cursor() {
    let a = area("hello wide world");
    let rows: Vec<&str> = a
        .rows(11)
        .iter()
        .map(|row| &a.text()[row.start..row.end])
        .collect();
    assert_eq!(rows, ["hello wide ", "world"]);
    assert_eq!(a.cursor_position(11), (1, 5));
    let a = area("ab\ncd");
    assert_eq!(a.rows(10).len(), 2);
    assert_eq!(a.cursor_position(10), (1, 2));
    // 比一行还宽的词硬断。
    let a = area("abcdefgh");
    assert_eq!(a.rows(3).len(), 3);
}

#[test]
fn vertical_moves_keep_the_column_and_report_the_edges() {
    let mut a = area("first line\nab\nthird line");
    assert!(a.at_last_row(40));
    assert!(a.vertical(40, false));
    assert_eq!(a.cursor_position(40), (1, 2), "clamped to the short line");
    assert!(a.vertical(40, false));
    assert_eq!(a.cursor_position(40), (0, 10), "the goal column is kept");
    assert!(!a.vertical(40, false));
    assert!(a.at_first_row(40));
}

#[test]
fn undo_steps_back_and_typing_within_a_second_is_one_step() {
    let start = std::time::Instant::now();
    let at = |ms: u64| start + std::time::Duration::from_millis(ms);
    let mut a = TextArea::default();
    for (index, c) in "abc".chars().enumerate() {
        a.checkpoint(at(index as u64 * 100), true);
        a.insert(c);
    }
    // 停了 2 秒再输入：另起一步。
    a.checkpoint(at(2_500), true);
    a.insert('d');
    a.checkpoint(at(2_600), false);
    a.backspace();
    assert_eq!(a.text(), "abc");
    assert!(a.undo());
    assert_eq!(a.text(), "abcd");
    assert!(a.undo());
    assert_eq!(a.text(), "abc");
    assert!(a.undo());
    assert_eq!(a.text(), "");
    assert!(!a.undo(), "nothing left");
}

#[test]
fn the_kill_ring_rotates_with_yank_pop() {
    let mut a = area("one two three");
    a.kill_word();
    a.kill_word();
    assert_eq!(a.text(), "one ");
    // ctrl+y 粘回最近的一次；alt+y 紧接着换成更早的一次。
    a.yank();
    assert_eq!(a.text(), "one two ");
    assert!(a.yank_pop());
    assert_eq!(a.text(), "one three");
    // 不是紧接在 yank 之后：alt+y 不生效。
    a.insert('!');
    assert!(!a.yank_pop());
    // alt+d 删掉后一个词。
    let mut a = area("alpha beta");
    a.home();
    assert_eq!(a.kill_word_forward(), 5);
    assert_eq!(a.text(), " beta");
}

#[test]
fn a_selection_is_replaced_by_typing_and_removed_by_delete_keys() {
    // 选中 "llo w"（2..7）。
    let selected = || {
        let mut a = area("hello world");
        a.select(2, 7, 7);
        a
    };
    let mut a = selected();
    assert_eq!(a.selection(), Some((2, 7)));
    a.insert('X');
    assert_eq!((a.text(), a.cursor(), a.selection()), ("heXorld", 3, None));
    for delete in [TextArea::backspace, TextArea::delete] {
        let mut a = selected();
        delete(&mut a);
        assert_eq!((a.text(), a.cursor(), a.selection()), ("heorld", 2, None));
    }
    // kill 类只删掉选区，删掉的进 kill ring，ctrl+y 粘回。
    for kill in [
        TextArea::kill_to_end,
        TextArea::kill_to_start,
        TextArea::kill_word,
        TextArea::kill_word_forward,
    ] {
        let mut a = selected();
        assert_eq!(kill(&mut a), 5);
        assert_eq!((a.text(), a.cursor()), ("heorld", 2));
        a.yank();
        assert_eq!(a.text(), "hello world");
    }
}

#[test]
fn moving_or_replacing_the_text_clears_the_selection() {
    for step in [
        TextArea::left,
        TextArea::right,
        TextArea::home,
        TextArea::end,
        TextArea::word_left,
        TextArea::word_right,
    ] {
        let mut a = area("hello world");
        a.select(2, 7, 7);
        step(&mut a);
        assert_eq!(a.selection(), None);
    }
    let mut a = area("hello world");
    a.select(2, 7, 2);
    a.set_text("new");
    assert_eq!(a.selection(), None);
    // 空范围不是选区，只放光标。
    a.select(1, 1, 1);
    assert_eq!((a.selection(), a.cursor()), (None, 1));
}

#[test]
fn unit_ranges_are_the_click_point_a_word_or_a_logical_line() {
    let a = area("say hello_2 now\nnext");
    assert_eq!(a.unit_range(6, 1), (6, 6));
    assert_eq!(a.unit_range(6, 2), (4, 11), "hello_2");
    assert_eq!(a.unit_range(3, 2), (3, 4), "a space is its own word");
    assert_eq!(a.unit_range(6, 3), (0, 16), "the line with its newline");
    assert_eq!(a.unit_range(18, 3), (16, 20));
}
