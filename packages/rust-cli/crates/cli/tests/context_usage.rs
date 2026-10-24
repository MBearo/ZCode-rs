//! 工作线 F93（docs/specs/rust-cli/context-usage.md §1、§3、D13，用户决定 2026-10-24）：`/context` 只用本地估算。
//! 真实 `zcode -p "/context"` 连本地 SSE 夹具（显式覆盖），夹具收不到任何 count_tokens 请求，表格照常打印。
//! 修改前每部分各发一次 count_tokens（夹具回的不是计数，于是退回本地估算）。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具"
)]

mod system_env;
use system_env::KeepSystem as _;
mod sse;

use std::process::Stdio;

#[tokio::test]
async fn context_sends_no_count_tokens_request() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(home.join(".zcode/v2")).unwrap();
    let fixture = sse::serve(Vec::new());
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", "/context"])
        .current_dir(&work)
        .env_clear()
        .keep_system()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &home)
        .env("ZCODE_RUNTIME_ENV", "test")
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "claude-opus-5-5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("## Context Usage"), "{stdout}");
    assert!(stdout.contains("| System prompt | "), "{stdout}");
    let paths = fixture.paths();
    assert!(
        paths.iter().all(|path| !path.contains("count_tokens")),
        "{paths:?}"
    );
    // `-p "/context"` 不发任何模型请求。
    assert!(paths.is_empty(), "{paths:?}");
}
