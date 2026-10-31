//! 压缩后带回调用过的 skill（docs/specs/rust-cli/compact.md §3.4）的真实进程：调用项目里的 skill，之后的用量触发自动
//! 压缩，压缩后的第一个请求带回 skill 的正文。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具"
)]

mod sse;

use std::process::Stdio;

use rust_cli_base::models::GLM_5;
use serde_json::json;

#[tokio::test]
async fn an_invoked_skill_comes_back_after_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(work.join(".zcode/skills/deploy")).unwrap();
    std::fs::write(
        work.join(".zcode/skills/deploy/SKILL.md"),
        "---\ndescription: Deploy the service\n---\nRun the deploy checklist.\n",
    )
    .unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let skill = json!({"skill": "deploy"}).to_string();
    let echo = json!({"command": "echo hi", "description": "echo"}).to_string();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(0, "toolu_skill", "Skill", &skill)]),
        // 报很大的用量：下一步先自动压缩。
        sse::tools_reply_with_usage(
            &[sse::tool_block(0, "toolu_echo", "Bash", &echo)],
            2_000_000,
        ),
        sse::text_reply("<summary>The user asked to deploy.</summary>"),
        sse::text_reply("done"),
    ]);
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
        .env("ZCODE_MODEL", GLM_5)
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
    assert_eq!(requests.len(), 4);
    let after = requests[3].to_string();
    assert!(
        after.contains("The following skills were invoked EARLIER in this session"),
        "{after}"
    );
    assert!(
        after.contains("### Skill: deploy\\nPath: projectSettings:deploy\\n\\n"),
        "{after}"
    );
    assert!(after.contains("Run the deploy checklist."), "{after}");
}
