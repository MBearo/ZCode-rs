//! 任务工具的周期提醒（docs/specs/rust-cli/agents.md §7.4）的真实进程：TaskCreate 之后连续十轮没有再用任务工具，
//! 下一个请求带上提醒与现在的任务。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具"
)]

mod sse;

use std::process::Stdio;

use serde_json::{Value, json};

const REMINDER: &str = "The task tools haven't been used recently.";

fn mentions(request: &Value, needle: &str) -> usize {
    request.to_string().matches(needle).count()
}

#[tokio::test]
async fn ten_rounds_without_task_tools_bring_the_reminder() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let create = json!({"subject": "Ship the parser", "description": "Make it pass"}).to_string();
    let mut script = vec![sse::tools_reply(&[sse::tool_block(
        0,
        "toolu_task",
        "TaskCreate",
        &create,
    )])];
    for round in 0..10 {
        let echo = json!({"command": format!("echo {round}"), "description": "echo"}).to_string();
        script.push(sse::tools_reply(&[sse::tool_block(
            0,
            &format!("toolu_echo_{round}"),
            "Bash",
            &echo,
        )]));
    }
    script.push(sse::text_reply("done"));
    let fixture = sse::serve(script);
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
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
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 12);
    // 第 11 个请求之前只有九轮：还没有；第 12 个请求带一条，列出任务。
    assert_eq!(mentions(&requests[10], REMINDER), 0);
    assert_eq!(mentions(&requests[11], REMINDER), 1);
    assert_eq!(
        mentions(
            &requests[11],
            "Here are the existing tasks:\\n\\n#1. [pending] Ship the parser"
        ),
        1
    );
}
