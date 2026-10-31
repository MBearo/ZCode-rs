use super::*;

/// 收拢前的三种写法（逐字照抄）：kernel-api 的 `(units + 2) / 4`、runtime 的 `(units * 2 + per) / (per * 2)`、
/// tools 的浮点 `round`。
#[test]
fn estimate_matches_the_old_formulas() {
    for units in 0..100_000_u64 {
        assert_eq!(estimate(units, CHARS_PER_TOKEN), (units + 2) / 4, "{units}");
        for per in [
            JSON_CHARS_PER_TOKEN,
            NEWER_MODEL_CHARS_PER_TOKEN,
            CHARS_PER_TOKEN,
        ] {
            assert_eq!(estimate(units, per), (units * 2 + per) / (per * 2));
            assert_eq!(
                estimate(units, per),
                (units as f64 / per as f64).round() as u64,
                "{units} / {per}"
            );
        }
    }
}
