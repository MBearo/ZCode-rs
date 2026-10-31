//! R5-1 的真实进程（docs/specs/rust-cli/query.md §3「tool_use 输入不是 JSON」）：同一响应里 Bash 合法、
//! Read 的输入不是 JSON。`zcode -p` 不再失败：Bash 跑完（标记文件在退出前写出），模型在第二个请求里收到
//! Bash 的结果与 Read 的 InputValidationError，回合正常结束；库里没有未结清的意图。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、检查写出的文件"
)]

mod sse;

use std::process::Stdio;
use std::sync::Arc;

use rust_cli_base::models::GLM_5_3;
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_node_store::{NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use serde_json::Value;

/// 请求里 tool_result 的 (tool_use_id, 文本)。
fn tool_results(request: &Value) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for message in request["messages"].as_array().into_iter().flatten() {
        for block in message["content"].as_array().into_iter().flatten() {
            if block["type"] != "tool_result" {
                continue;
            }
            let text = match &block["content"] {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect(),
                _ => String::new(),
            };
            found.push((
                block["tool_use_id"].as_str().unwrap().to_owned(),
                sse::tool_output(&text).to_owned(),
            ));
        }
    }
    found
}

#[tokio::test]
async fn a_tool_input_that_is_not_json_becomes_an_input_validation_error() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let mark = work.join("MARK");
    let bash = serde_json::json!({
        "command": format!("sleep 1; echo done > {}", mark.display()),
        "description": "mark",
    })
    .to_string();
    let raw = format!("{{\"file_path\": \"{}\"", work.join("x.txt").display());
    let fixture = sse::serve(vec![
        sse::tools_reply(&[
            sse::tool_block(0, "toolu_bash", "Bash", &bash),
            sse::tool_block(1, "toolu_read", "Read", &raw),
        ]),
        sse::text_reply("after tools"),
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
        .env("ZCODE_MODEL", GLM_5_3)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // 修复前：`zcode: tool input for Read is not JSON …`，退出码 1，Bash 被丢下（退出后才写出标记）。
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "after tools"
    );
    assert!(mark.exists(), "Bash finished before the process exited");

    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 2);
    let results = tool_results(&requests[1]);
    let ids: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["toolu_bash", "toolu_read"]);
    let expected = format!(
        "<tool_use_error>InputValidationError: Read was called with input that could not be parsed as JSON.\nYou sent (first {len} of {len} bytes): {raw}\nCommon causes: unescaped backslashes in file paths (use / or \\\\), unescaped control characters, or truncated output. Retry with valid JSON.</tool_use_error>",
        len = raw.len()
    );
    assert_eq!(results[1].1, expected);
    // 回放给模型的 tool_use 输入是标记对象。
    let replayed = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .find(|block| block["id"] == "toolu_read")
        .unwrap();
    assert_eq!(
        replayed["input"]["__unparsedToolInput"]["raw"],
        raw.as_str()
    );

    // 落库：assistant、两个结果都在，意图都已结清。
    let paths = StoragePaths::resolve(&StorageSettings::default(), &home, &work);
    // 与入口相同的时钟与 ID（F7：NodeStore 收入口注入的 NodeIds）。
    let ids = NodeIds::new(Arc::new(SystemClock::utc()));
    let store = NodeStore::new(paths, Arc::new(ids));
    let listed = store
        .list(rust_cli_kernel_api::store::SessionFilter {
            workspace: rust_cli_kernel_api::env::WorkspaceRef {
                environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
                root: rust_cli_kernel_api::env::EnvPath::new(work.to_string_lossy()),
            },
            limit: 1,
        })
        .await
        .unwrap();
    let loaded = store.load(&listed[0].id).await.unwrap();
    let text = serde_json::to_string(&loaded.messages).unwrap();
    assert!(text.contains("toolu_bash") && text.contains("InputValidationError"));
}
