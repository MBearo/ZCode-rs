use super::*;

#[test]
fn node_platform_uses_the_node_names() {
    let expected = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    assert_eq!(node_platform(), expected);
    assert!(!["macos", "windows"].contains(&node_platform()));
}

#[test]
fn the_timer_limit_is_the_js_maximum() {
    assert_eq!(MAX_TIMER_MS, u64::from(i32::MAX.unsigned_abs()));
    assert_eq!(MAX_TIMER_MS, 2_147_483_647);
}

/// F64：架构名照 Node 的 `os.arch()`。
#[test]
fn node_arch_uses_the_node_names() {
    for (rust, node) in [
        ("aarch64", "arm64"),
        ("x86_64", "x64"),
        ("x86", "ia32"),
        ("arm", "arm"),
        ("riscv64", "riscv64"),
        ("s390x", "s390x"),
        ("loongarch64", "loong64"),
        ("powerpc64", "ppc64"),
    ] {
        assert_eq!(node_arch_of(rust), node, "{rust}");
    }
    assert_eq!(node_arch(), node_arch_of(std::env::consts::ARCH));
}
