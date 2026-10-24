//! L28 命令的解析（tui-display.md §4.1、§4.3）。
use rust_cli_kernel_api::permission::PermissionMode;

use super::*;

#[test]
fn exit_words_match_the_whole_line() {
    for word in ["exit", "quit", ":q", ":q!", ":wq", ":wq!", "  :q  "] {
        assert!(is_exit_word(word), "{word}");
    }
    for text in ["exit now", "/exit", ":w", "q"] {
        assert!(!is_exit_word(text), "{text}");
    }
}

#[test]
fn node_modes_map_to_permission_modes() {
    assert_eq!(node_mode("plan"), Some(PermissionMode::Plan));
    assert_eq!(node_mode("build"), Some(PermissionMode::Default));
    // 修复线 F20：guarded 是真正的 guarded（以前当 bypassPermissions）。
    assert_eq!(node_mode("guarded"), Some(PermissionMode::Guarded));
    assert_eq!(node_mode("yolo"), Some(PermissionMode::BypassPermissions));
    assert_eq!(node_mode("auto"), None);
    // 工作线 F76：照 App 只留三个模式，edit 不再收。
    assert_eq!(node_mode("edit"), None);
    assert_eq!(
        MENU.map(PermissionMode::node_name),
        ["build", "guarded", "yolo"]
    );
}

/// 修复线 F20 的 shift+tab 循环：build → guarded → yolo → build；不在循环里的模式回到 build。
#[test]
fn shift_tab_cycles_build_guarded_yolo() {
    use crate::ui::compose::next_mode;
    assert_eq!(next_mode(PermissionMode::Default), PermissionMode::Guarded);
    assert_eq!(
        next_mode(PermissionMode::Guarded),
        PermissionMode::BypassPermissions
    );
    assert_eq!(
        next_mode(PermissionMode::BypassPermissions),
        PermissionMode::Default
    );
    for outside in [
        PermissionMode::AcceptEdits,
        PermissionMode::DontAsk,
        PermissionMode::Auto,
        PermissionMode::Plan,
    ] {
        assert_eq!(next_mode(outside), PermissionMode::Default, "{outside:?}");
    }
}
