// 复制自 rust/crates/kit/src/js/date_tests.rs（feat/rs 79b6691c55）。
use super::*;

#[test]
fn timestamps_are_js_iso_strings() {
    assert_eq!(iso_timestamp(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(iso_timestamp(1_767_225_600_000), "2026-01-01T00:00:00.000Z");
    assert_eq!(iso_timestamp(951_782_400_123), "2000-02-29T00:00:00.123Z");
    assert_eq!(iso_timestamp(1_790_000_000_999), "2026-09-21T14:13:20.999Z");
}
