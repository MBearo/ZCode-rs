//! 远端机器的会话级真实进程测试（docs/specs/rust-cli/remote-exec.md §20.11）：`zcode -p` 连本地 SSE 夹具，debug 构建的
//! `_ZCODE_ASSUME_LOOPBACK_MACHINES` 把 `machines.json` 里连着的 dev-a 经 loopback 连上（一个执行器子进程，工作目录与
//! 本机不同）。模型（剧本）调用带 `machine` 的工具，结果回到本机。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录与配置、起本地 HTTP 夹具、读回 hook 写下的文件"
)]

mod sse;

use std::path::PathBuf;
use std::process::{Output, Stdio};

use rust_cli_base::models::GLM_5;
use serde_json::{Value, json};

struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
    remote: PathBuf,
}

/// 临时 HOME、本机工作区、dev-a 的工作目录；`machines.json` 里 dev-a 连着；用户级 hook 把 PreToolUse 的输入写进文件。
fn sandbox() -> Sandbox {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work, remote) = (base.join("home"), base.join("work"), base.join("remote"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&remote).unwrap();
    std::fs::create_dir_all(home.join(".zcode/rust-cli")).unwrap();
    std::fs::create_dir_all(home.join(".zcode/cli")).unwrap();
    std::fs::write(work.join("notes.txt"), "local notes\n").unwrap();
    std::fs::write(remote.join("notes.txt"), "remote notes\n").unwrap();
    let machines = json!({"version": 1, "machines": [
        {"name": "dev-a", "ssh": "dev-a", "workingDirectory": remote.display().to_string(), "connected": true}
    ]});
    std::fs::write(
        home.join(".zcode/rust-cli/machines.json"),
        machines.to_string(),
    )
    .unwrap();
    let hooks = json!({"hooks": {"enabled": true, "events": {"PreToolUse": [{"matcher": "Bash|Read", "hooks": [
        {"type": "command", "command": format!("cat >> {}", base.join("hooks.jsonl").display())}
    ]}]}}});
    std::fs::write(home.join(".zcode/cli/config.json"), hooks.to_string()).unwrap();
    Sandbox {
        _dir: dir,
        home,
        work,
        remote,
    }
}

fn run(sandbox: &Sandbox, fixture: &sse::Fixture, machines: bool) -> Output {
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        // 缺省模式：Edit 没读过就拒绝（bypass 时不要求先读）；只读的 Bash 与工作目录里的 Read 不用询问。
        .args(["-p", "go", "--permission-mode", "default"])
        .current_dir(&sandbox.work)
        .env_clear()
        .env("HOME", &sandbox.home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &sandbox.home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", GLM_5)
        .env("ZCODE_RUNTIME_ENV", "test")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if machines {
        command.env("_ZCODE_ASSUME_LOOPBACK_MACHINES", "1");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn tool(index: usize, id: &str, name: &str, input: Value) -> String {
    sse::tool_block(index, id, name, &input.to_string())
}

fn schema<'a>(request: &'a Value, name: &str) -> &'a Value {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == name)
        .map(|tool| &tool["input_schema"])
        .unwrap()
}

/// 请求最后一条 user 消息里的工具结果文字。
fn last_result(request: &Value) -> String {
    // 最后一条 user 消息（有卡片形状的模型把这一步的提醒放在其后的 system 消息里）。
    let messages = request["messages"].as_array().unwrap();
    let last = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .unwrap();
    last["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["type"] == "tool_result")
        .map(|block| match &block["content"] {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn all_text(request: &Value) -> String {
    request["messages"].to_string()
}

#[tokio::test]
async fn tool_calls_run_on_the_named_machine_and_come_back() {
    let sandbox = sandbox();
    let remote_notes = sandbox.remote.join("notes.txt").display().to_string();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[tool(
            0,
            "toolu_pwd",
            "Bash",
            json!({"command": "pwd", "description": "where", "machine": "dev-a"}),
        )]),
        sse::tools_reply(&[tool(
            0,
            "toolu_read",
            "Read",
            json!({"file_path": "notes.txt", "machine": "dev-a"}),
        )]),
        // 在 dev-a 上读它工作目录里的文件（绝对路径），再在本机改同一个路径（loopback 的“远端”与本机共用文件系统）：
        // “先读后改”按机器分开，本机没读过。
        sse::tools_reply(&[tool(
            0,
            "toolu_read2",
            "Read",
            json!({"file_path": remote_notes, "machine": "dev-a"}),
        )]),
        sse::tools_reply(&[tool(
            0,
            "toolu_edit",
            "Edit",
            json!({"file_path": remote_notes, "old_string": "remote", "new_string": "REMOTE"}),
        )]),
        sse::tools_reply(&[tool(
            0,
            "toolu_here",
            "Bash",
            json!({"command": "pwd", "description": "here"}),
        )]),
        sse::text_reply("done"),
    ]);
    run(&sandbox, &fixture, true);
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 6);
    // 工具定义：带参数的工具多 `machine`，其余不变。
    assert_eq!(
        schema(&requests[0], "Bash")["properties"]["machine"],
        json!({"type": "string", "enum": ["local", "dev-a"], "description": "Machine to run on; omit to use the local machine."})
    );
    assert!(schema(&requests[0], "Read")["properties"]["machine"].is_object());
    assert!(
        schema(&requests[0], "ToolSearch")["properties"]
            .get("machine")
            .is_none()
    );
    // 机器清单提醒在第一次请求里，只发一次。
    let line = "- dev-a: ";
    let reminder = "Remote machines connected to this session.";
    assert!(all_text(&requests[0]).contains(reminder));
    assert!(all_text(&requests[0]).contains(line));
    assert!(
        all_text(&requests[0]).contains(&format!("working directory {}", sandbox.remote.display()))
    );
    assert_eq!(all_text(&requests[5]).matches(reminder).count(), 1);
    // Bash 在 dev-a 的工作目录跑；相对路径的 Read 读 dev-a 那边的文件。
    assert!(last_result(&requests[1]).contains(&sandbox.remote.display().to_string()));
    assert!(last_result(&requests[2]).contains("remote notes"));
    // 同一台机器上第二次读同一个文件：重复读取的提示（记录在 dev-a 上）。
    assert!(
        last_result(&requests[3])
            .starts_with("Wasted call \u{2014} file unchanged since your last Read."),
        "{}",
        last_result(&requests[3])
    );
    // dev-a 上读过不算本机读过。
    let edit = last_result(&requests[4]);
    assert!(edit.contains("File has not been read yet"), "{edit}");
    assert_eq!(
        std::fs::read_to_string(sandbox.remote.join("notes.txt")).unwrap(),
        "remote notes\n"
    );
    // 不写 `machine` 就是本机。
    assert!(last_result(&requests[5]).contains(&sandbox.work.display().to_string()));
    // hooks 在本机跑：远端调用的输入带 `machine`，本机的没有；`tool_input` 里没有 `machine`。
    let hooks =
        std::fs::read_to_string(sandbox.home.parent().unwrap().join("hooks.jsonl")).unwrap();
    let events: Vec<Value> = serde_json::Deserializer::from_str(&hooks)
        .into_iter::<Value>()
        .map(Result::unwrap)
        .collect();
    assert_eq!(events.len(), 4, "{hooks}");
    assert_eq!(events[0]["machine"], "dev-a");
    assert_eq!(
        events[0]["tool_input"],
        json!({"command": "pwd", "description": "where"})
    );
    assert_eq!(events[0]["cwd"], sandbox.work.display().to_string());
    assert!(events[3].get("machine").is_none());
}

#[tokio::test]
async fn without_the_loopback_declaration_print_mode_connects_nothing() {
    let sandbox = sandbox();
    let fixture = sse::serve(vec![sse::text_reply("done")]);
    run(&sandbox, &fixture, false);
    let requests = fixture.main_requests();
    assert!(
        schema(&requests[0], "Bash")["properties"]
            .get("machine")
            .is_none()
    );
    assert!(!all_text(&requests[0]).contains("Remote machines"));
}
