//! 名字太长的 MCP 工具（docs/specs/rust-cli/ext.md §3.4，修复线 F67）：真实 `zcode -p` 连一个 stdio MCP 服务器
//! （`fixtures/mcp-long-names.mjs`，用 node 跑），服务器列出的工具里有一个 `mcp__<server>__<tool>` 超过 128 个字符的。
//! 发给模型的工具表没有它（API 会拒绝整个请求），普通工具照常在；运行日志里记一行 warn。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "集成测试：准备临时目录、找 node、起本地 HTTP 夹具、检查写出的文件"
)]

mod sse;

use std::path::PathBuf;
use std::process::Stdio;

use serde_json::{Value, json};

/// 测试进程的 PATH 里的 node（`pnpm test:rust-cli` 由 node 启动，PATH 里一定有）。
fn node() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("node"))
        .find(|candidate| candidate.is_file())
        .expect("node is not on PATH")
}

#[test]
fn a_tool_whose_name_is_too_long_is_not_sent_to_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let server =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-long-names.mjs");
    let config = json!({"mcpServers": {"names": {"command": node(), "args": [server]}}});
    let fixture = sse::serve(vec![sse::text_reply("done")]);
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", "hi", "--strict-mcp-config", "--mcp-config"])
        .arg(config.to_string())
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &home)
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
    let long = format!("mcp__names__{}", "x".repeat(120));
    let request = fixture.main_requests().remove(0);
    let names: Vec<&str> = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    // 夹具是本地 ToolSearch（工作线 F84，tools-more.md §3.2）：MCP 工具在延迟工具清单里，不在 tools 里。
    let text = request.to_string();
    assert!(text.contains("\\nmcp__names__lookup"), "{names:?}");
    assert!(!names.contains(&"mcp__names__lookup"), "{names:?}");
    assert!(!text.contains(&long), "{names:?}");
    // 运行日志（开发构建写 warn）：每个名字一行，带服务器、工具名与长度。
    let log = home.join(".zcode/rust-cli/log");
    let file = std::fs::read_dir(&log)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let lines: Vec<Value> = std::fs::read_to_string(file)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let skipped: Vec<&Value> = lines
        .iter()
        .filter(|line| line["event"] == "mcp.tool.skipped")
        .collect();
    assert_eq!(skipped.len(), 1, "{lines:#?}");
    assert_eq!(skipped[0]["level"], "warn");
    assert_eq!(
        skipped[0]["message"],
        format!(
            "Tool \"{long}\" is not sent to the model: its name has 132 characters and the API accepts at most 128"
        )
    );
    assert_eq!(skipped[0]["context"]["server"], "names");
    assert_eq!(skipped[0]["context"]["length"], 132);
}
