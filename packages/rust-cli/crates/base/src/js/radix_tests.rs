use super::*;

/// 与 JavaScript 的 `toString(36)` 逐个对照（node -e 'console.log((n).toString(36))'）。
#[test]
fn base36_matches_javascript_to_string_36() {
    assert_eq!(base36(0), "0");
    assert_eq!(base36(35), "z");
    assert_eq!(base36(36), "10");
    assert_eq!(base36(1_760_000_000_000), "mgj6k3cw");
    assert_eq!(base36(1_790_219_323_612), "mueyd2os");
    assert_eq!(base36(u64::from(u32::MAX)), "1z141z3");
}
