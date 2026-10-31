//! 登记只放还在跑的命令（工作线 F119，docs/specs/rust-cli/shell-permissions.md §2.1）：结束的命令不留登记，已经拿到流的
//! 一方照常读完。以前每条命令（Bash、hook、Monitor）按 operationId 一直留着，远端跑了 23 小时的 TUI 留着约 2 万条。
//! 需要 `/bin/bash`，Windows 上跳过。
#![cfg(unix)]

use futures_util::StreamExt;
use rust_cli_kernel_api::env_shell::{
    BackgroundCommand, BackgroundInfo, BackgroundStatus, BackgroundStop, BackgroundTask, CwdScope,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId};

use super::tests::{collect, context, fixture, request};

#[tokio::test]
async fn finished_commands_leave_no_registration() {
    let fixture = fixture(Vec::new());
    for n in 0..20 {
        let operation = format!("op_registry_{n}");
        let events = fixture
            .shells
            .start(
                context(&operation),
                request("true", &fixture.root, CwdScope::Pinned),
            )
            .await
            .unwrap();
        collect(events).await;
    }
    assert_eq!(fixture.shells.shared.state().commands.len(), 0);
    // 结束的命令不能再按 ID 接回。
    assert!(
        fixture
            .shells
            .watch(&OperationId::new("op_registry_0"), 0)
            .is_none()
    );
}

/// 结束、通知还没 ack 的后台命令：登记已删，TaskStop 仍查得到命令原文与终态（原文跟通知一起记着）；ack 之后查不到。
#[tokio::test]
async fn a_finished_background_command_is_found_until_its_notice_is_acked() {
    let fixture = fixture(Vec::new());
    let mut start = request("echo done", &fixture.root, CwdScope::Pinned);
    start.background = Some(BackgroundCommand {
        description: "Done".to_owned(),
        tool_use: None,
        kind: Default::default(),
    });
    let key = BackgroundTask {
        session: SessionId::new("s1"),
        task: start.task.clone(),
    };
    let events = fixture.shells.start(context("op_bg"), start).await.unwrap();
    collect(events).await;
    let notice = fixture
        .shells
        .notices(&SessionId::new("s1"), 0)
        .next()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixture.shells.shared.state().commands.len(), 0);
    let found = fixture.shells.background_command(&key);
    assert!(
        matches!(
            &found,
            Some(BackgroundInfo { command, status: BackgroundStatus::Completed, .. }) if command == "echo done"
        ),
        "{found:?}"
    );
    assert!(matches!(
        fixture.shells.stop(&key),
        BackgroundStop::NotRunning {
            status: BackgroundStatus::Completed
        }
    ));
    fixture.shells.ack(&SessionId::new("s1"), notice.seq);
    assert!(fixture.shells.background_command(&key).is_none());
    assert!(matches!(
        fixture.shells.stop(&key),
        BackgroundStop::NotFound
    ));
}
