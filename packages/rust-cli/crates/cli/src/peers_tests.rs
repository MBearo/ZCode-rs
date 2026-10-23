//! 过期时长的环境变量（cross-session.md §2.4）。

use super::hold_timeout;

/// 名字是 `ZCODE_USER_DIALOG_TIMEOUT_MS`（config.md §2.5）；不是数字时缺省。
#[test]
fn the_hold_timeout_reads_the_zcode_variable() {
    let env = |name: &str, value: &str| vec![(name.to_owned(), value.to_owned())];
    let zcode = |value: &str| env("ZCODE_USER_DIALOG_TIMEOUT_MS", value);
    assert_eq!(hold_timeout(&[]), 300_000);
    assert_eq!(hold_timeout(&zcode("1500")), 1500);
    assert_eq!(hold_timeout(&zcode("soon")), 300_000);
}
