//! Bash 超时的推导（cli-args.md §9.3，修复线 F42）。

use super::BashTimeouts;

fn timeouts(default: Option<u64>, max: Option<u64>) -> (u64, u64, u64, u64) {
    let t = BashTimeouts::new(default, max);
    (
        t.default_ms,
        t.max_ms,
        t.background_default_ms,
        t.background_max_ms,
    )
}

/// 与 Node `bootstrap/tests/runtime-config.test.ts` 的几组相同（同一套规则），另加后台的两个值。
#[test]
fn the_timeouts_follow_the_derivation_rules() {
    assert_eq!(timeouts(None, None), (120_000, 600_000, 600_000, 7_200_000));
    assert_eq!(BashTimeouts::default(), BashTimeouts::new(None, None));
    assert_eq!(
        timeouts(Some(250), Some(900)),
        (250, 900, 600_000, 7_200_000)
    );
    // 上限比缺省小：上限取缺省。
    assert_eq!(
        timeouts(Some(500_000), Some(300_000)),
        (500_000, 500_000, 600_000, 7_200_000)
    );
    assert_eq!(
        timeouts(Some(900_000), Some(300_000)),
        (900_000, 900_000, 900_000, 7_200_000)
    );
    // 没设上限：max(600000, 缺省)。
    assert_eq!(
        timeouts(Some(700_000), None),
        (700_000, 700_000, 700_000, 7_200_000)
    );
    // 后台上限 max(7200000, 前台上限)，不超过 32 位定时器的上限。
    assert_eq!(timeouts(None, Some(8_000_000)).3, 8_000_000);
    assert_eq!(timeouts(None, Some(9_000_000_000)).3, 2_147_483_647);
}

#[test]
fn the_effective_timeout_uses_the_default_and_clamps_to_the_max() {
    let t = BashTimeouts::new(Some(5_000), Some(9_000));
    assert_eq!(t.effective(None, false), 5_000);
    assert_eq!(t.effective(Some(7_000.0), false), 7_000);
    assert_eq!(t.effective(Some(90_000.0), false), 9_000);
    assert_eq!(t.effective(Some(0.0), false), 5_000);
    assert_eq!(t.effective(Some(f64::NAN), false), 5_000);
    assert_eq!(t.effective(None, true), 600_000);
    assert_eq!(t.effective(Some(9e9), true), 7_200_000);
}

/// 没设时文字与工具定义（`prompts/tools/Bash.json`）逐字相同。
#[test]
fn the_default_text_matches_the_tool_definition() {
    let t = BashTimeouts::default();
    assert_eq!(
        t.foreground_line(),
        "- `timeout` is in milliseconds: default 120000, max 600000 for a foreground command."
    );
    assert_eq!(
        t.timeout_parameter(),
        "Optional timeout in milliseconds (max 600000 for a foreground command)"
    );
    assert!(
        t.deadline_sentence()
            .contains("(default 600000, max 7200000); at that limit")
    );
    assert!(
        t.deadline_parameter()
            .ends_with("(default 600000 ms, max 7200000 ms).")
    );
}
