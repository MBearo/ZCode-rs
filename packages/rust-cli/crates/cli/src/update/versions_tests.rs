// Codex `update_versions.rs` 的测试用例（c3d3b142d）。
use super::*;

#[test]
fn prerelease_versions_are_not_considered_newer() {
    assert_eq!(is_newer("0.11.0-beta.1", "0.11.0"), None);
    assert_eq!(is_newer("1.0.0-rc.1", "1.0.0"), None);
}

#[test]
fn plain_semver_comparisons_work() {
    assert_eq!(is_newer("0.11.1", "0.11.0"), Some(true));
    assert_eq!(is_newer("0.11.0", "0.11.1"), Some(false));
    assert_eq!(is_newer("1.0.0", "0.9.9"), Some(true));
    assert_eq!(is_newer("0.9.9", "1.0.0"), Some(false));
    assert_eq!(is_newer("0.1.47", "0.1.46"), Some(true));
    assert_eq!(is_newer("0.1.46", "0.1.46"), Some(false));
    // 数值比较，不是字符串比较。
    assert_eq!(is_newer("0.1.100", "0.1.99"), Some(true));
}

#[test]
fn source_build_versions_are_not_checked() {
    assert!(is_source_build("0.0.0"));
    assert!(!is_source_build("0.1.0"));
}

#[test]
fn whitespace_is_ignored() {
    assert_eq!(parse(" 1.2.3 \n"), Some((1, 2, 3)));
    assert_eq!(is_newer(" 1.2.3 ", "1.2.2"), Some(true));
}

#[test]
fn malformed_versions_cannot_be_compared() {
    for bad in ["", "1", "1.2", "a.b.c", "-1.0.0"] {
        assert_eq!(is_newer(bad, "0.1.0"), None, "{bad}");
        assert_eq!(is_newer("0.1.0", bad), None, "{bad}");
    }
}
