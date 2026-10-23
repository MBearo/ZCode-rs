use super::*;

#[test]
fn whole_file_reads_normalize_crlf_and_count_the_trailing_empty_line() {
    let selected = select("first\r\nsecond\r\n", 15, 0, None);
    assert_eq!(selected.content, "first\nsecond\n");
    assert_eq!((selected.line_count, selected.total_lines), (3, 3));
}

#[test]
fn bom_is_dropped_from_content_and_size() {
    let selected = select("\u{feff}bom\n", 7, 0, None);
    assert_eq!(selected.content, "bom\n");
    assert_eq!(selected.total_bytes, 4);
}

#[test]
fn ranges_strip_one_carriage_return_per_line() {
    let selected = select("a\r\r\nb\r\nc", 9, 1, Some(5));
    assert_eq!(selected.content, "b\nc");
    assert_eq!((selected.line_count, selected.total_lines), (2, 3));
}

#[test]
fn offsets_past_the_end_select_nothing() {
    let selected = select("one\ntwo\n", 8, 100, None);
    assert_eq!(selected.content, "");
    assert_eq!((selected.line_count, selected.total_lines), (0, 3));
}
