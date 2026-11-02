//! Stop hook 阻止继续（docs/specs/rust-cli/query.md §3 的 Stop hooks，工作线 F134）：真实的 `zcode -p`，配置里一个
//! Stop hook 回 `{"continue": false}`。回合只请求一次模型，result 是 `success`、不是错误，`terminal_reason` 是
//! `stop_hook_prevented`。修复前主循环按正常结束处理，`terminal_reason` 写的是 `completed`。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、读写出的 JSON"
)]

mod sse;

use std::process::Stdio;

use rust_cli_base::models::GLM_5_3;

#[test]
fn a_stop_hook_that_prevents_continuation_ends_print_mode_with_its_own_reason() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(home.join(".zcode/cli")).unwrap();
    // Node 配置文件的形状（config.md §2）：`hooks.enabled` 与 `hooks.events`。
    let stop = r#"printf '%s' '{"continue":false,"stopReason":"done for today"}'"#;
    let config = serde_json::json!({"hooks": {"enabled": true, "events": {"Stop": [{"hooks": [
        {"type": "command", "command": stop}
    ]}]}}});
    std::fs::write(home.join(".zcode/cli/config.json"), config.to_string()).unwrap();
    let fixture = sse::serve(vec![sse::text_reply("first"), sse::text_reply("second")]);
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", "go", "--output-format", "json"])
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", GLM_5_3)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    let result: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(
        (
            &result["subtype"],
            &result["is_error"],
            &result["terminal_reason"],
            &result["result"],
        ),
        (
            &serde_json::json!("success"),
            &serde_json::json!(false),
            &serde_json::json!("stop_hook_prevented"),
            &serde_json::json!("first"),
        ),
        "{text}"
    );
    // 阻止继续之后不再请求模型。
    assert_eq!(fixture.main_requests().len(), 1);
}
