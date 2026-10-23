use std::collections::BTreeMap;

use super::Accel;

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn a_native_terminal_starts_at_one_row_and_accelerates_in_bursts() {
    let mut accel = Accel::new(&vars(&[]), false);
    // 第一格：基数 1。
    assert_eq!(accel.rows_at(1, 1000.0), 1);
    // 40 ms 内连续：倍数每格加 0.3（1.3、1.6、1.9、2.2 …），上限 6。
    let rows: Vec<usize> = (1..=6)
        .map(|n| accel.rows_at(1, 1000.0 + 10.0 * f64::from(n)))
        .collect();
    assert_eq!(rows, vec![1, 1, 1, 2, 2, 2]);
    for n in 7..40 {
        accel.rows_at(1, 1000.0 + 10.0 * f64::from(n));
    }
    assert_eq!(accel.rows_at(1, 1400.0), 6);
    // 停一会儿：回到基数。
    assert_eq!(accel.rows_at(1, 2000.0), 1);
}

#[test]
fn the_first_notch_after_a_direction_change_does_not_scroll() {
    let mut accel = Accel::new(&vars(&[]), false);
    assert_eq!(accel.rows_at(1, 0.0), 1);
    assert_eq!(accel.rows_at(-1, 100.0), 0);
    // 接着同一个新方向：按新方向从基数开始。
    assert_eq!(accel.rows_at(-1, 150.0), 1);
}

#[test]
fn bouncing_back_within_200_ms_enters_wheel_mode() {
    let mut accel = Accel::new(&vars(&[]), false);
    accel.rows_at(1, 0.0);
    assert_eq!(accel.rows_at(-1, 100.0), 0);
    // 200 ms 内又回到原方向（认作鼠标滚轮）：按衰减加速，每次最多加 3（1.3 + 3 → 4）。
    assert_eq!(accel.rows_at(1, 150.0), 4);
}

#[test]
fn vs_code_and_windows_terminal_use_the_decay_curve_with_base_three() {
    let mut accel = Accel::new(&vars(&[("TERM_PROGRAM", "vscode")]), false);
    // 第一格：方向从 0 变成 1，倍数 max(2, 3) = 3。
    assert_eq!(accel.rows_at(1, 1000.0), 3);
    // 同方向 5 ms 内：1 行。
    assert_eq!(accel.rows_at(1, 1002.0), 1);
    let mut windows = Accel::new(&vars(&[("WT_SESSION", "x")]), false);
    assert_eq!(windows.rows_at(1, 1000.0), 3);
}

#[test]
fn cursor_floods_and_the_scroll_speed_variable_overrides_the_base() {
    let mut cursor = Accel::new(&vars(&[("CURSOR_TRACE_ID", "x")]), false);
    // 洪泛：间隔大于 40 ms 时 基数 × 3，否则基数。
    assert_eq!(cursor.rows_at(1, 1000.0), 3);
    assert_eq!(cursor.rows_at(1, 1010.0), 1);
    // 名字是 `ZCODE_SCROLL_SPEED`（config.md §2.5）；上限 20。
    let mut fast = Accel::new(&vars(&[("ZCODE_SCROLL_SPEED", "5")]), false);
    assert_eq!(fast.rows_at(1, 1000.0), 5);
    let mut capped = Accel::new(&vars(&[("ZCODE_SCROLL_SPEED", "50")]), false);
    assert_eq!(capped.rows_at(1, 1000.0), 20);
    let mut jetbrains = Accel::new(&vars(&[("TERMINAL_EMULATOR", "JetBrains-JediTerm")]), false);
    assert_eq!(jetbrains.rows_at(1, 1000.0), 2);
}
