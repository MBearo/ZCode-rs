//! 发请求前的阻塞线（docs/specs/rust-cli/compact.md §2.2）的真实进程：关掉压缩（`DISABLE_COMPACT`）时，上一个回复报的
//! 用量到了阻塞线，下一步不发请求，`-p` 以 `Prompt is too long` 结束、退出码 1。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具"
)]

mod sse;

use std::process::{Output, Stdio};

fn run(fixture: &sse::Fixture, disable_compact: bool) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(["-p", "go", "--permission-mode", "bypassPermissions"])
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "glm-5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if disable_compact {
        command.env("DISABLE_COMPACT", "1");
    }
    command.output().unwrap()
}

fn echo() -> String {
    sse::tool_block(
        0,
        "toolu_echo",
        "Bash",
        r#"{"command": "echo hi", "description": "echo"}"#,
    )
}

#[tokio::test]
async fn the_blocking_line_ends_the_turn_without_a_request() {
    // 第一个回复报 2,000,000 输入 token：比任何窗口的阻塞线都高。
    let fixture = sse::serve(vec![
        sse::tools_reply_with_usage(&[echo()], 2_000_000),
        sse::text_reply("never sent"),
    ]);
    let output = run(&fixture, true);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Prompt is too long"
    );
    assert_eq!(fixture.main_requests().len(), 1);
}

/// 用量在阻塞线以下：照常发请求。
#[tokio::test]
async fn below_the_line_the_request_is_sent() {
    let fixture = sse::serve(vec![
        sse::tools_reply_with_usage(&[echo()], 1_000),
        sse::text_reply("answered"),
    ]);
    let output = run(&fixture, true);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "answered");
    assert_eq!(fixture.main_requests().len(), 2);
}
