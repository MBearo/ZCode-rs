use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use super::*;
use crate::test_support::run;

fn verdict(
    mode: PermissionMode,
    entries: &[(RuleBehavior, &str)],
    access: Access,
    path: &str,
) -> Check {
    run(mode, entries, |inputs| check(access, Some(path), inputs))
}

#[test]
fn reads_inside_are_allowed_and_outside_ask() {
    assert_eq!(
        verdict(PermissionMode::Default, &[], Access::Read, "a.txt"),
        Check::Allow
    );
    assert_eq!(
        verdict(PermissionMode::Default, &[], Access::Read, "/etc/hosts"),
        Check::ask(
            "ZCode requested permissions to read from /etc/hosts, but you haven't granted it yet."
        )
    );
    assert_eq!(
        verdict(
            PermissionMode::Default,
            &[(RuleBehavior::Allow, "Read(//etc/**)")],
            Access::Read,
            "/etc/hosts"
        ),
        Check::Allow
    );
    assert_eq!(
        verdict(
            PermissionMode::Default,
            &[(RuleBehavior::Deny, "Read(*.env)")],
            Access::Read,
            ".env"
        ),
        Check::deny("Permission to read /w/.env has been denied.")
    );
}

#[test]
fn writes_need_accept_edits_inside_and_always_ask_for_sensitive_paths() {
    assert_eq!(
        verdict(PermissionMode::Default, &[], Access::Write, "src/a.rs"),
        Check::ask(
            "ZCode requested permissions to write to /w/src/a.rs, but you haven't granted it yet."
        )
    );
    assert_eq!(
        verdict(PermissionMode::AcceptEdits, &[], Access::Write, "src/a.rs"),
        Check::Allow
    );
    assert!(matches!(
        verdict(PermissionMode::AcceptEdits, &[], Access::Write, "/tmp/x"),
        Check::Ask { .. }
    ));
    assert_eq!(
        verdict(
            PermissionMode::AcceptEdits,
            &[],
            Access::Write,
            ".git/config"
        ),
        Check::Ask {
            message:
                "ZCode requested permissions to edit /w/.git/config which is a sensitive file."
                    .to_owned(),
            kind: AskKind::Safety,
            suggestions: Vec::new(),
        }
    );
    assert_eq!(
        verdict(
            PermissionMode::Default,
            &[(RuleBehavior::Allow, "Edit(src/**)")],
            Access::Write,
            "src/a.rs"
        ),
        Check::Allow
    );
    assert_eq!(
        verdict(
            PermissionMode::AcceptEdits,
            &[(RuleBehavior::Deny, "Edit(src/**)")],
            Access::Write,
            "src/a.rs"
        ),
        Check::deny("Permission to edit /w/src/a.rs has been denied.")
    );
}

/// 敏感文件表用 zcode 的文件名（修复线 F58，README §4.1）：项目配置是
/// `zcode.json`（带 MCP 服务器与 hooks），zcode 没有 `.zcode.json`。以前表里是机械替换出来的 `.zcode.json`。
#[test]
fn the_project_config_is_a_sensitive_file() {
    assert_eq!(
        verdict(
            PermissionMode::AcceptEdits,
            &[],
            Access::Write,
            "zcode.json"
        ),
        Check::Ask {
            message: "ZCode requested permissions to edit /w/zcode.json which is a sensitive file."
                .to_owned(),
            kind: AskKind::Safety,
            suggestions: Vec::new(),
        }
    );
    assert_eq!(
        verdict(
            PermissionMode::AcceptEdits,
            &[],
            Access::Write,
            ".zcode.json"
        ),
        Check::Allow
    );
    assert!(matches!(
        verdict(PermissionMode::AcceptEdits, &[], Access::Write, ".mcp.json"),
        Check::Ask { .. }
    ));
}

/// F14：判定与工具用同一条路径规则（权限检查与执行都先规范化路径，
/// tools-files.md §3.2）。工具会去掉首尾空白，判定若不去，`" /etc/hosts"` 会被当成工作区里的相对路径而放行。
#[test]
fn file_paths_are_resolved_like_the_tools_resolve_them() {
    assert_eq!(
        verdict(PermissionMode::Default, &[], Access::Read, " /etc/hosts\n"),
        Check::ask(
            "ZCode requested permissions to read from /etc/hosts, but you haven't granted it yet."
        )
    );
    // 规则按 NFC 的路径匹配：分解形式的 `é` 也命中按合成形式写的 deny 规则。
    assert_eq!(
        verdict(
            PermissionMode::Default,
            &[(RuleBehavior::Deny, "Read(caf\u{e9}.env)")],
            Access::Read,
            "cafe\u{301}.env"
        ),
        Check::deny("Permission to read /w/caf\u{e9}.env has been denied.")
    );
    // 只认 `~/`：`~x` 是工作区里的文件。
    assert_eq!(
        verdict(PermissionMode::Default, &[], Access::Read, "~x"),
        Check::Allow
    );
}
