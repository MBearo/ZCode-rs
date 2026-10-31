//! 会话的当前目录，真实进程（F19，docs/specs/rust-cli/tools-files.md §3.3）：`zcode -p` 里 Bash `cd sub` 之后，
//! Write 的相对路径与它的权限判定是同一个文件。deny 规则保护工作区根下的 x.txt；修前判决链按 shell 的当前目录
//! 看 `sub/x.txt`（放行），Write 按工作区根写了被保护的 `x.txt`。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、检查写出的文件"
)]

mod sse;

use std::process::Stdio;

use rust_cli_base::models::GLM_5_3;
use serde_json::{Value, json};

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

fn call(id: &str, name: &str, input: &Value) -> String {
    sse::tools_reply(&[sse::tool_block(0, id, name, &input.to_string())])
}

#[tokio::test]
async fn zcode_writes_the_relative_path_the_permission_check_judged_after_cd() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(work.join("sub")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let fixture = sse::serve(vec![
        call(
            "toolu_cd",
            "Bash",
            &json!({"command": "cd sub", "description": "enter sub"}),
        ),
        call(
            "toolu_write",
            "Write",
            &json!({"file_path": "x.txt", "content": "in sub\n"}),
        ),
        call(
            "toolu_up",
            "Write",
            &json!({"file_path": "../x.txt", "content": "denied\n"}),
        ),
        call(
            "toolu_reset",
            "Bash",
            &json!({"command": "cd / && pwd", "description": "leave"}),
        ),
        call(
            "toolu_root",
            "Write",
            &json!({"file_path": "y.txt", "content": "root\n"}),
        ),
        sse::text_reply("after tools"),
    ]);
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args([
            "-p",
            "go",
            "--permission-mode",
            "acceptEdits",
            "--disallowedTools",
            "Edit(/x.txt)",
            // `cd /` 是工作区外的路径，要 Bash 的 allow 规则才不询问。
            "--allowedTools",
            "Bash",
        ])
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/bash")
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
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "after tools"
    );

    // 磁盘：写在 sub 里；被 deny 规则保护的根下 x.txt 没有被写（修前被写成 `in sub`）。
    let read = |name: &str| std::fs::read_to_string(work.join(name)).ok();
    assert_eq!(read("sub/x.txt").as_deref(), Some("in sub\n"));
    assert_eq!(read("x.txt"), None);
    // `cd /` 越出工作区：cwd 回到工作区根，之后的相对路径按根解析。
    assert_eq!(read("y.txt").as_deref(), Some("root\n"));

    let requests = fixture.main_requests();
    let results = tool_results(requests.last().unwrap());
    let text = |id: &str| {
        results
            .iter()
            .find(|(found, _)| found == id)
            .map(|(_, text)| text.clone())
            .unwrap()
    };
    let at = |name: &str| format!("{}/{name}", work.display());
    // 结果里是输入的原样路径（`filePath` 是输入）。
    assert!(
        text("toolu_write").starts_with("File created successfully at: x.txt"),
        "{}",
        text("toolu_write")
    );
    assert_eq!(
        text("toolu_up"),
        format!("Permission to edit {} has been denied.", at("x.txt"))
    );
    assert_eq!(
        text("toolu_reset"),
        format!("/\nShell cwd was reset to {}", work.display())
    );
    assert!(text("toolu_root").starts_with("File created successfully at: y.txt"));
}

/// 子代理：派生时取父会话当时的当前目录。主线程 `cd sub` 之后派前台子代理，子代理的
/// `Write x.txt` 写进 sub、`pwd` 是 sub；子代理里的 `cd ..` 不保留（`preventCwdChanges`），主线程之后仍在 sub。修前子会话
/// 没有自己的目录，落回工作区根。
#[tokio::test]
async fn a_subagent_starts_in_the_directory_the_parent_was_in() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(work.join("sub")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let agent = json!({
        "description": "write in place",
        "prompt": "Write the file.",
        "subagent_type": "general-purpose",
        "run_in_background": false,
    });
    // 主请求与子代理的请求都带工具，按到达的顺序回复：主线程 cd → 派子代理 → 子代理的三步与报告 → 主线程。
    let fixture = sse::serve(vec![
        call(
            "toolu_cd",
            "Bash",
            &json!({"command": "cd sub", "description": "enter sub"}),
        ),
        call("toolu_agent", "Agent", &agent),
        call(
            "toolu_child_write",
            "Write",
            &json!({"file_path": "x.txt", "content": "from the agent\n"}),
        ),
        call(
            "toolu_child_cd",
            "Bash",
            &json!({"command": "cd .. && pwd", "description": "go up"}),
        ),
        call(
            "toolu_child_pwd",
            "Bash",
            &json!({"command": "pwd", "description": "where"}),
        ),
        sse::text_reply("agent report"),
        call(
            "toolu_main_pwd",
            "Bash",
            &json!({"command": "pwd", "description": "where"}),
        ),
        sse::text_reply("after tools"),
    ]);
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", "go", "--permission-mode", "bypassPermissions"])
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/bash")
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
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "after tools"
    );

    let read = |name: &str| std::fs::read_to_string(work.join(name)).ok();
    assert_eq!(read("sub/x.txt").as_deref(), Some("from the agent\n"));
    assert_eq!(read("x.txt"), None);

    let requests = fixture.main_requests();
    let results: Vec<(String, String)> = requests.iter().flat_map(tool_results).collect();
    let text = |id: &str| {
        results
            .iter()
            .find(|(found, _)| found == id)
            .map(|(_, text)| text.clone())
            .unwrap_or_else(|| panic!("no result for {id}"))
    };
    let sub = work.join("sub").display().to_string();
    assert_eq!(text("toolu_child_cd"), work.display().to_string());
    assert_eq!(text("toolu_child_pwd"), sub);
    assert_eq!(text("toolu_main_pwd"), sub);
}
