//! 进程被杀后续写（docs/specs/rust-cli/session-lifecycle.md §6.3，工作线 F81 R3）的真实进程：一次回复里先 Read、
//! 再 Bash；Read 的结果落库之后、Bash 还在执行时进程被杀掉。`-p -c` 续写时 Bash 补一条中断结果，排在 Read 的结果之后，
//! 再是 `No response requested.`；再续写一次，补的结果已在库里、不再补。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、杀掉进程"
)]

mod sse;

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use rust_cli_base::models::GLM_5;
use serde_json::{Value, json};

const INTERRUPTED: &str = "[Tool call interrupted: the session ended before this call's result was recorded, so its outcome is unknown. Check whether it took effect before relying on it or running it again.]";

struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

fn sandbox() -> Sandbox {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(work.join("a.txt"), "alpha\n").unwrap();
    Sandbox {
        _dir: dir,
        home,
        work,
    }
}

fn spawn(sandbox: &Sandbox, fixture: &sse::Fixture, args: &[&str]) -> Child {
    rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(args)
        .args(["--permission-mode", "bypassPermissions"])
        .current_dir(&sandbox.work)
        .env_clear()
        .env("HOME", &sandbox.home)
        .env("USERPROFILE", &sandbox.home)
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/bash")
        .env("ZCODE_DATA_BASE_DIR", &sandbox.home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", GLM_5)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait_for(path: &Path) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "{path:?} never appeared"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// assistant 与 user 消息里的工具块：`use:<id>`、`res:<id>[:err]:<文字>`；文字块略去。
fn tool_blocks(message: &Value) -> Vec<String> {
    message["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| match block["type"].as_str()? {
                    "tool_use" => Some(format!("use:{}", block["id"].as_str()?)),
                    "tool_result" => {
                        // 结果正文可能是字符串，也可能是文字块数组（随模型的请求形状）。
                        let body = match &block["content"] {
                            Value::String(text) => text.clone(),
                            Value::Array(parts) => parts
                                .iter()
                                .filter_map(|part| part["text"].as_str())
                                .collect::<Vec<_>>()
                                .join(""),
                            other => other.to_string(),
                        };
                        let error = if block["is_error"] == true {
                            ":err"
                        } else {
                            ""
                        };
                        Some(format!(
                            "res:{}{error}:{body}",
                            block["tool_use_id"].as_str()?
                        ))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn has_text(message: &Value, wanted: &str) -> bool {
    match &message["content"] {
        Value::String(text) => text.contains(wanted),
        Value::Array(blocks) => blocks.iter().any(|block| {
            block["text"]
                .as_str()
                .is_some_and(|text| text.contains(wanted))
        }),
        _ => false,
    }
}

#[tokio::test]
async fn a_killed_call_gets_an_interrupted_result_when_the_session_continues() {
    let sandbox = sandbox();
    let (started, release) = (sandbox.work.join("started"), sandbox.work.join("release"));
    let read = json!({"file_path": sandbox.work.join("a.txt")}).to_string();
    let command = format!(
        "touch {}; until [ -f {} ]; do sleep 0.1; done",
        started.display(),
        release.display()
    );
    let bash = json!({"command": command, "description": "Wait"}).to_string();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[
            sse::tool_block(0, "toolu_1", "Read", &read),
            sse::tool_block(1, "toolu_2", "Bash", &bash),
        ]),
        sse::text_reply("second answer"),
        sse::text_reply("third answer"),
    ]);
    let mut first = spawn(&sandbox, &fixture, &["-p", "first question"]);
    wait_for(&started);
    // Read 的结果在 Bash 开工之前就已交出并落库；等一会儿让提交完成。
    std::thread::sleep(Duration::from_millis(1000));
    first.kill().unwrap();
    first.wait().unwrap();
    // 放掉被杀的进程留下的 shell。
    std::fs::write(&release, "").unwrap();

    let second = spawn(&sandbox, &fixture, &["-p", "-c", "second question"]);
    let output = second.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"].as_array().unwrap().clone();
    let assistant = messages
        .iter()
        .position(|message| message["role"] == "assistant")
        .unwrap();
    assert_eq!(
        tool_blocks(&messages[assistant]),
        ["use:toolu_1", "use:toolu_2"]
    );
    let results = tool_blocks(&messages[assistant + 1]);
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(results[0].starts_with("res:toolu_1:"), "{results:?}");
    assert_eq!(results[1], format!("res:toolu_2:err:{INTERRUPTED}"));
    assert!(has_text(&messages[assistant + 2], "No response requested."));
    // 新的输入在最后一条 user 消息里（有卡片形状的模型把这一步的提醒放在其后的 system 消息里）。
    let last_user = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .unwrap();
    assert!(has_text(last_user, "second question"));

    // 再续写一次：补的结果是从库里读出来的，不再补第二条。
    let third = spawn(&sandbox, &fixture, &["-p", "-c", "third question"]);
    let output = third.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = fixture.main_requests();
    let messages = requests[2]["messages"].as_array().unwrap();
    let interrupted: Vec<String> = messages
        .iter()
        .flat_map(tool_blocks)
        .filter(|block| block.contains(INTERRUPTED))
        .collect();
    assert_eq!(interrupted.len(), 1, "{interrupted:?}");
    assert_eq!(tool_blocks(&messages[assistant + 1]), results);
}
