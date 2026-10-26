//! 前台命令被要求转到后台（修复线 L-A，docs/specs/rust-cli/tui-input.md §11）：给了 `on_timeout` 的前台命令转过去、
//! 看输出的一方收到带原因的事件、命令照常跑完并记通知；hook 那样没有 `on_timeout` 的命令不转。
//! 夹具在 shell_tests.rs（需要 `/bin/bash`，Windows 上跳过）。
#![cfg(unix)]
use futures_util::StreamExt;
use rust_cli_kernel_api::env::{BackgroundReason, CommandEvent};
use rust_cli_kernel_api::env_shell::{BackgroundCommand, CwdScope, TimeoutBackground};
use rust_cli_kernel_api::ids::{OperationId, SessionId, ToolUseId};

use super::tests::{context, fixture, request};

#[tokio::test]
async fn a_requested_move_backgrounds_the_foreground_command_with_its_reason() {
    let fixture = fixture(Vec::new());
    let mut start = request(
        "echo start; sleep 1; echo late",
        &fixture.root,
        CwdScope::Pinned,
    );
    start.on_timeout = Some(TimeoutBackground {
        command: BackgroundCommand {
            description: "Long build".to_owned(),
            tool_use: Some(ToolUseId::new("toolu_1")),
            kind: Default::default(),
        },
        deadline_ms: None,
    });
    let task = start.task.clone();
    let mut hook = request("sleep 1", &fixture.root, CwdScope::Pinned);
    hook.timeout_ms = Some(10_000);
    fixture.shells.start(context("op1"), start).await.unwrap();
    fixture.shells.start(context("op2"), hook).await.unwrap();
    let mut events = fixture.shells.watch(&OperationId::new("op1"), 0).unwrap();
    // 先等命令开始输出，再要求转。
    loop {
        if let Some(Ok(CommandEvent::Output { .. })) = events.next().await {
            break;
        }
    }
    let moved = fixture
        .shells
        .background_foreground(&SessionId::new("s1"), BackgroundReason::User);
    assert_eq!(
        moved,
        std::slice::from_ref(&task),
        "only the command with on_timeout moves"
    );
    let mut seen = None;
    while let Some(event) = events.next().await {
        match event.unwrap() {
            CommandEvent::BackgroundedBy { reason } => seen = Some(reason),
            CommandEvent::Finished { .. } => break,
            _ => {}
        }
    }
    assert_eq!(seen, Some(BackgroundReason::User));
    // 再要求一次：已经在后台，不再转。
    assert!(
        fixture
            .shells
            .background_foreground(&SessionId::new("s1"), BackgroundReason::DeliverMessage)
            .is_empty()
    );
}
