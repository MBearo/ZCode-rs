//! 输出上限（F11，审查 R2-18；docs/specs/rust-cli/shell-permissions.md §2.2“输出上限”）：输出文件超过上限时
//! 结束命令。用小的上限与间隔代替缺省的 5 GB / 5 s。需要 `/bin/bash`，Windows 上跳过。
#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{CommandEvent, Outcome};
use rust_cli_kernel_api::env_shell::{BackgroundCommand, CwdScope, HookIo};
use rust_cli_kernel_api::ids::SessionId;

use super::tests::{collect, context, fixture, request};
use super::*;

/// 64 KiB，每 20 ms 查一次。
const SMALL: OutputLimit = OutputLimit {
    bytes: 64 * 1024,
    label: "64KB",
    every: Duration::from_millis(20),
};

/// 一直写输出的命令（内置 echo，不起子进程；写得快但不至于一瞬间写满磁盘）。
const FLOOD: &str = "while :; do echo yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy; done";

#[expect(clippy::disallowed_methods, reason = "测试检查输出文件是否还在")]
fn exists(path: &Path) -> bool {
    path.exists()
}

fn finished_parts(event: CommandEvent) -> (Outcome, Option<i32>, bool, u64, Option<String>) {
    let CommandEvent::Finished {
        outcome,
        exit_code,
        timed_out,
        output_bytes,
        output_limit,
        ..
    } = event
    else {
        panic!("not finished");
    };
    (outcome, exit_code, timed_out, output_bytes, output_limit)
}

#[tokio::test]
async fn a_foreground_command_past_the_limit_is_killed_and_its_file_removed() {
    let mut fixture = fixture(Vec::new());
    fixture.shells.set_output_limit(SMALL);
    let start = request(FLOOD, &fixture.root, CwdScope::Pinned);
    let events = fixture.shells.start(context("op1"), start).await.unwrap();
    let output = fixture
        .shells
        .command(&OperationId::new("op1"))
        .unwrap()
        .output
        .clone();
    let ran = collect(events).await;
    let (outcome, exit, timed_out, bytes, note) = finished_parts(ran.finished);
    assert_eq!(
        (outcome, exit, timed_out),
        (Outcome::Cancelled, Some(137), false)
    );
    assert_eq!(
        note.as_deref(),
        Some("Command killed: output file exceeded 64KB")
    );
    assert!(bytes > SMALL.bytes, "{bytes}");
    assert!(ran.output.starts_with("yyyy"));
    // 前台被结束：输出文件删除，磁盘上不留下超限的文件。
    assert!(!exists(&output));
}

#[tokio::test]
async fn a_background_command_past_the_limit_ends_as_killed() {
    let mut fixture = fixture(Vec::new());
    fixture.shells.set_output_limit(SMALL);
    let mut start = request(FLOOD, &fixture.root, CwdScope::Pinned);
    start.background = Some(BackgroundCommand {
        description: "Flood".to_owned(),
        tool_use: None,
        kind: Default::default(),
    });
    let _events = fixture.shells.start(context("op1"), start).await.unwrap();
    let notice = fixture
        .shells
        .notices(&SessionId::new("s1"), 0)
        .next()
        .await
        .unwrap()
        .unwrap();
    // 通知按 killed（`was stopped`），文件保留并以 `[killed]` 结尾。
    assert_eq!(
        (notice.outcome, notice.exit_code),
        (Outcome::Cancelled, Some(137))
    );
    #[expect(clippy::disallowed_methods, reason = "测试读回后台命令的输出文件")]
    let written = std::fs::read_to_string(notice.output.as_str()).unwrap();
    assert!(written.len() as u64 > SMALL.bytes);
    assert!(written.ends_with("\n[killed]\n"));
}

#[tokio::test]
async fn hook_files_count_together_against_the_limit() {
    let mut fixture = fixture(vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
    fixture.shells.set_output_limit(SMALL);
    let mut start = request(&format!("{FLOOD} >&2"), &fixture.root, CwdScope::Pinned);
    start.hook = Some(HookIo {
        stdin: "{}\n".to_owned(),
        env: BTreeMap::new(),
    });
    let events = fixture.shells.start(context("op1"), start).await.unwrap();
    let ran = collect(events).await;
    let (outcome, exit, _, _, note) = finished_parts(ran.finished);
    assert_eq!((outcome, exit), (Outcome::Cancelled, Some(137)));
    assert!(note.is_some());
}

#[test]
fn the_default_limit_is_five_gigabytes_every_five_seconds() {
    assert_eq!(OUTPUT_LIMIT.bytes, 5_368_709_120);
    assert_eq!(OUTPUT_LIMIT.label, "5GB");
    assert_eq!(OUTPUT_LIMIT.every, Duration::from_secs(5));
}
