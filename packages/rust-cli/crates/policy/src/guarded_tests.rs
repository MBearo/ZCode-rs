//! guarded 的危险命令询问（shell-permissions.md §4.6.1）：什么时候问。识别本身在 `shell/danger_tests.rs`。

use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::permission::PermissionMode;

use super::*;

#[test]
fn only_guarded_bash_calls_apply() {
    let command = Some("rm -rf build");
    assert!(applies(PermissionMode::Guarded, command, PathStyle::Posix));
    // plan 开时判决用的模式是 plan；yolo 与其他模式都不询问。
    for mode in [
        PermissionMode::Plan,
        PermissionMode::BypassPermissions,
        PermissionMode::Default,
    ] {
        assert!(!applies(mode, command, PathStyle::Posix), "{mode:?}");
    }
    // 不是 Bash 的命令（调用方不给命令）。
    assert!(!applies(PermissionMode::Guarded, None, PathStyle::Posix));
    assert!(!applies(
        PermissionMode::Guarded,
        Some("ls -la"),
        PathStyle::Posix
    ));
}
