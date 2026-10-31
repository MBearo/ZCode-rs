//! 读过的文件在磁盘上被改动、Bash 读文件记为已读（docs/specs/rust-cli/tools-files.md §4.1、§4.2）的真实进程：
//! `zcode -p` 连本地 SSE 夹具，文件在两次请求之间从外部改写。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、在两次请求之间改写文件"
)]

mod sse;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use rust_cli_base::models::GLM_5;
use serde_json::{Value, json};

const NOTE: &str = "changed on disk since you last read it. That's usually deliberate, so take it as the current state rather than reverting it; if the change looks wrong, say so rather than undoing it yourself \u{2014} otherwise no need to call it out.";

/// 请求里的全部文字（各条消息的文字块与工具结果，按顺序）。
fn texts(request: &Value) -> Vec<String> {
    let mut found = Vec::new();
    for message in request["messages"].as_array().into_iter().flatten() {
        match &message["content"] {
            Value::String(text) => found.push(text.clone()),
            Value::Array(blocks) => {
                for block in blocks {
                    if let Some(text) = block["text"].as_str() {
                        found.push(text.to_owned());
                    }
                    match &block["content"] {
                        Value::String(text) => found.push(text.clone()),
                        Value::Array(parts) => found.extend(
                            parts
                                .iter()
                                .filter_map(|part| part["text"].as_str().map(str::to_owned)),
                        ),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    found
}

fn count(request: &Value, needle: &str) -> usize {
    texts(request)
        .iter()
        .map(|text| text.matches(needle).count())
        .sum()
}

fn tool(index: usize, id: &str, name: &str, input: Value) -> String {
    sse::tool_block(index, id, name, &input.to_string())
}

fn bash(id: &str, command: &str) -> String {
    sse::tools_reply(&[tool(
        0,
        id,
        "Bash",
        json!({"command": command, "description": "run"}),
    )])
}

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

fn run(sandbox: &Sandbox, fixture: &sse::Fixture) -> Output {
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", "go", "--permission-mode", "bypassPermissions"])
        .current_dir(&sandbox.work)
        .env_clear()
        .env("HOME", &sandbox.home)
        .env("USERPROFILE", &sandbox.home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &sandbox.home)
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
    output
}

/// 夹具收到这一次主请求时先改写文件，再回 `reply`。
fn after_writing(path: &Path, content: &'static str, reply: String) -> sse::Reply {
    let path = path.to_owned();
    sse::Reply::From(Box::new(move |_| {
        std::fs::write(&path, content).unwrap();
        reply.clone()
    }))
}

/// Write 写下的文件被外部改了：下一个请求里是改动片段；之后的请求不再重复。
#[tokio::test]
async fn a_written_file_changed_on_disk_is_announced_once() {
    let sandbox = sandbox();
    let file = sandbox.work.join("a.txt");
    let write = sse::tools_reply(&[tool(
        0,
        "toolu_write",
        "Write",
        json!({"file_path": file.display().to_string(), "content": "one\ntwo\nthree\n"}),
    )]);
    let fixture = sse::serve_with(vec![
        sse::Reply::Fixed(write),
        after_writing(&file, "one\nTWO\nthree\n", bash("toolu_echo", "echo hi")),
        sse::Reply::Fixed(bash("toolu_again", "echo again")),
        sse::Reply::Fixed(sse::text_reply("done")),
    ]);
    run(&sandbox, &fixture);
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 4);
    let note = format!(
        "Note: {} {NOTE} Here are the relevant changes (shown with line numbers):\n1\tone\n2\tTWO\n3\tthree",
        file.display()
    );
    assert_eq!(count(&requests[1], NOTE), 0);
    assert_eq!(count(&requests[2], &note), 1, "{:#?}", texts(&requests[2]));
    // 重新读进了记录（Read 的形状）：之后不再查，下一个请求只有历史里的那一条。
    assert_eq!(count(&requests[3], NOTE), 1);
}

/// `cat` 读过的文件：Edit 不用先 Read，结果说内容就在上下文里；外部改动同样提醒。
#[tokio::test]
async fn a_file_read_with_cat_counts_as_read() {
    let sandbox = sandbox();
    let file = sandbox.work.join("b.txt");
    std::fs::write(&file, "alpha\nbeta\n").unwrap();
    let edit = sse::tools_reply(&[tool(
        0,
        "toolu_edit",
        "Edit",
        json!({"file_path": file.display().to_string(), "old_string": "beta", "new_string": "BETA"}),
    )]);
    let fixture = sse::serve_with(vec![
        sse::Reply::Fixed(bash("toolu_cat", "cat b.txt")),
        sse::Reply::Fixed(edit),
        after_writing(&file, "alpha\nBETA\ngamma\n", bash("toolu_echo", "echo hi")),
        sse::Reply::Fixed(sse::text_reply("done")),
    ]);
    run(&sandbox, &fixture);
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 4);
    let edited = format!(
        "The file {} has been updated successfully. (file state is current in your context \u{2014} no need to Read it back)",
        file.display()
    );
    assert_eq!(
        count(&requests[2], &edited),
        1,
        "{:#?}",
        texts(&requests[2])
    );
    let note = format!(
        "Note: {} {NOTE} Here are the relevant changes (shown with line numbers):\n1\talpha\n2\tBETA\n3\tgamma",
        file.display()
    );
    assert_eq!(count(&requests[3], &note), 1, "{:#?}", texts(&requests[3]));
}

/// Read 读过的文件被外部改了：不提醒（Read 的记录不查）。
#[tokio::test]
async fn a_file_read_with_read_is_not_announced() {
    let sandbox = sandbox();
    let file = sandbox.work.join("c.txt");
    std::fs::write(&file, "x\n").unwrap();
    let read = sse::tools_reply(&[tool(
        0,
        "toolu_read",
        "Read",
        json!({"file_path": file.display().to_string()}),
    )]);
    let fixture = sse::serve_with(vec![
        sse::Reply::Fixed(read),
        after_writing(&file, "y\n", bash("toolu_echo", "echo hi")),
        sse::Reply::Fixed(sse::text_reply("done")),
    ]);
    run(&sandbox, &fixture);
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(count(&requests[2], NOTE), 0);
}
