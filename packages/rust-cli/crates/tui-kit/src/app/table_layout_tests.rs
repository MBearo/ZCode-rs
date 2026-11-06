//! 表格列宽分配的测试。
//! 移植自 packages/tui/tui/test/src/zcode/tui/app/TableLayoutSuite.scala（feat/tui a2bf878b34）。
use super::table_layout::column_widths;

#[test]
fn columns_keep_their_natural_width_when_they_fit() {
    assert_eq!(column_widths(&[3, 4], &[1, 1], 20), vec![3, 4]);
}

#[test]
fn surplus_is_shared_in_proportion_to_slack_summing_to_avail() {
    // 自然宽度 10+10=20 > 12；最小宽度 2+2=4；多出的 8 平分。
    assert_eq!(column_widths(&[10, 10], &[2, 2], 12), vec![6, 6]);
}

#[test]
fn a_column_with_no_slack_is_not_shrunk_the_slack_column_absorbs_it() {
    // 第 0 列 natural == min（没有余量），余量都在第 1 列。
    let w = column_widths(&[10, 2], &[2, 2], 8);
    assert_eq!(w, vec![6, 2]);
    assert_eq!(w.iter().sum::<usize>(), 8);
}

#[test]
fn shrink_widths_always_sum_exactly_to_avail() {
    let w = column_widths(&[10, 10, 10], &[1, 1, 1], 14);
    assert_eq!(w.iter().sum::<usize>(), 14);
    assert!(w.iter().all(|&x| x >= 1), "{w:?}");
}

#[test]
fn when_even_the_minimums_overflow_widths_scale_down_but_stay_at_least_one() {
    let w = column_widths(&[10, 10], &[8, 8], 10);
    assert_eq!(w.iter().sum::<usize>(), 10);
    assert!(w.iter().all(|&x| x >= 1), "{w:?}");
}

#[test]
fn empty_input_yields_no_columns() {
    assert_eq!(column_widths(&[], &[], 40), Vec::<usize>::new());
}

#[test]
fn leftover_units_go_to_the_largest_fractions_ties_by_index() {
    // 10 × 1/3 = 3.33…：三列各 3，剩的 1 给下标最小的（小数部分相同）。
    assert_eq!(column_widths(&[9, 9, 9], &[0, 0, 0], 10), vec![4, 3, 3]);
}
