//! 整块的提及：跟着前后的编辑移动，编辑落进里面时变回普通文字，⌫ 整块删除，发出时换成它的写法。
use super::super::TextArea;

fn area(text: &str, mark: (usize, usize)) -> TextArea {
    let mut a = TextArea::default();
    a.set_text(text);
    a.mark(mark.0, mark.1, "conversation", "[#Fix it](#sess_ab)".into());
    a
}

#[test]
fn a_mark_moves_with_edits_before_it_and_goes_out_as_its_wire() {
    let mut a = area("see @Fix it now", (4, 11));
    a.set_cursor(0);
    a.insert_str("please ");
    a.sync_marks();
    assert_eq!(a.marks()[0].start, 11);
    assert_eq!(a.wired(), "please see [#Fix it](#sess_ab) now");
    // 在它后面打字：不动。
    a.set_cursor(a.text().len());
    a.insert_str("!");
    assert_eq!(a.wired(), "please see [#Fix it](#sess_ab) now!");
}

#[test]
fn an_edit_inside_turns_it_back_into_text_and_backspace_takes_it_whole() {
    let mut a = area("@Fix it ", (0, 7));
    a.set_cursor(3);
    a.insert_str("x");
    a.sync_marks();
    assert!(a.marks().is_empty());
    assert_eq!(a.wired(), "@Fixx it ");
    let mut a = area("@Fix it ", (0, 7));
    a.set_cursor(7);
    a.backspace();
    a.sync_marks();
    assert_eq!(a.text(), " ");
    assert!(a.marks().is_empty());
}
