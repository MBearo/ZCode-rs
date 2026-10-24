//! 续写时补 `No response requested.`（docs/specs/rust-cli/session-lifecycle.md §6.3.1）的真实进程：第一次运行
//! 在等回复时被杀掉，`-p -c` 续写时旧输入之后先是一条 assistant，再是新输入；这条 assistant 落库，再续写一次时
//! 仍在原处（工作线 F77）；第一次运行以 API 错误结束时不补，新旧输入照旧并成一条。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、杀掉进程"
)]

mod sse;

use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

const SENTINEL: &str = "No response requested.";

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
    Sandbox {
        _dir: dir,
        home,
        work,
    }
}

fn spawn(sandbox: &Sandbox, fixture: &sse::Fixture, args: &[&str]) -> Child {
    rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(args)
        .current_dir(&sandbox.work)
        .env_clear()
        .env("HOME", &sandbox.home)
        .env("USERPROFILE", &sandbox.home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &sandbox.home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "glm-5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// 消息的 (角色, 全部文字)。
fn turns(request: &Value) -> Vec<(String, String)> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| {
            let text = match &message["content"] {
                Value::String(text) => text.clone(),
                Value::Array(blocks) => blocks
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            (message["role"].as_str().unwrap().to_owned(), text)
        })
        .collect()
}

/// 只有 user 与 assistant 两种角色的轮次（`role: system` 的提醒并进前面的 user）。
fn roles(request: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (role, text) in turns(request) {
        let role = if role == "system" {
            "user".to_owned()
        } else {
            role
        };
        match out.last_mut() {
            Some((last, joined)) if *last == role => {
                joined.push('\n');
                joined.push_str(&text);
            }
            _ => out.push((role, text)),
        }
    }
    out
}

/// 一个 SSE 的错误事件（请求本身被拒绝，不重试）。
fn api_error() -> String {
    "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"rejected by the fixture\"}}\n\n".to_owned()
}

#[tokio::test]
async fn a_killed_turn_gets_a_reply_before_the_next_input() {
    let sandbox = sandbox();
    let (arrived, wait) = mpsc::channel::<()>();
    let fixture = sse::serve_with(vec![
        // 第一次运行的请求：告诉测试它到了，挂着不回，进程被杀掉。
        sse::Reply::From(Box::new(move |_| {
            let _ = arrived.send(());
            std::thread::sleep(Duration::from_secs(3));
            sse::text_reply("too late")
        })),
        sse::Reply::Fixed(sse::text_reply("second answer")),
        sse::Reply::Fixed(sse::text_reply("third answer")),
    ]);
    let mut first = spawn(&sandbox, &fixture, &["-p", "first question"]);
    wait.recv_timeout(Duration::from_secs(60)).unwrap();
    first.kill().unwrap();
    first.wait().unwrap();
    let second = spawn(&sandbox, &fixture, &["-p", "-c", "second question"]);
    let output = second.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 2);
    let turns = roles(&requests[1]);
    let shape: Vec<&str> = turns.iter().map(|(role, _)| role.as_str()).collect();
    assert_eq!(shape, ["user", "assistant", "user"], "{turns:#?}");
    assert!(turns[0].1.contains("first question"));
    assert_eq!(turns[1].1, SENTINEL);
    assert!(turns[2].1.contains("second question"));
    assert!(!turns[2].1.contains("first question"));
    // 再续写一次：那条 assistant 是从库里读出来的，旧输入与第二条输入仍然分开。
    let third = spawn(&sandbox, &fixture, &["-p", "-c", "third question"]);
    let output = third.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 3);
    let turns = roles(&requests[2]);
    let shape: Vec<&str> = turns.iter().map(|(role, _)| role.as_str()).collect();
    assert_eq!(
        shape,
        ["user", "assistant", "user", "assistant", "user"],
        "{turns:#?}"
    );
    assert!(turns[0].1.contains("first question"));
    assert_eq!(turns[1].1, SENTINEL);
    assert!(turns[2].1.contains("second question") && !turns[2].1.contains("first question"));
    assert_eq!(turns[3].1, "second answer");
    assert!(turns[4].1.contains("third question"));
}

#[tokio::test]
async fn a_failed_turn_gets_no_reply() {
    let sandbox = sandbox();
    let fixture = sse::serve(vec![api_error(), sse::text_reply("second answer")]);
    let first = spawn(&sandbox, &fixture, &["-p", "first question"]);
    let output = first.wait_with_output().unwrap();
    assert!(!output.status.success());
    let second = spawn(&sandbox, &fixture, &["-p", "-c", "second question"]);
    let output = second.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 2);
    let turns = roles(&requests[1]);
    let shape: Vec<&str> = turns.iter().map(|(role, _)| role.as_str()).collect();
    assert_eq!(shape, ["user"], "{turns:#?}");
    assert!(turns[0].1.contains("first question") && turns[0].1.contains("second question"));
    assert!(!turns[0].1.contains(SENTINEL));
}
