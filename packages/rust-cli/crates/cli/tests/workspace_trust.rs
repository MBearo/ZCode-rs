//! 工作区信任的真实进程（docs/specs/rust-cli/workspace-trust.md §2.1）：临时 home 与工作区、本地假的 Anthropic
//! 端点。`-p` 在没有信任记录的目录里照常运行、不写记录；TUI 没有终端时与之前相同地失败，也不写记录。
//! 对话框的画面与按键由 tui 的集成测试覆盖（`crates/tui/tests/suite/trust.rs`）。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、检查写出的记录"
)]

mod system_env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Output, Stdio};
use system_env::KeepSystem as _;

use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::workspace_trust::TrustKey;

const REPLY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",",
    "\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

/// 每个连接回同一段 SSE 文本回复。
fn model_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                REPLY.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(REPLY.as_bytes());
        }
    });
    format!("http://{address}")
}

fn zcode(home: &Path, work: &Path, args: &[&str], server: &str) -> Output {
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(args)
        .current_dir(work)
        .env_clear()
        .keep_system()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", home)
        .env("ZCODE_MODEL_BASE_URL", server)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "claude-opus-5-5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// 这台机器的会话库里，工作区与它的父目录有没有信任记录（与入口相同的存储位置）。
async fn trusted(home: &Path, work: &Path) -> Option<TrustKey> {
    let paths = rust_cli_paths::StoragePaths::resolve(&Default::default(), home, work);
    let store = rust_cli_node_store::NodeStore::new(
        paths,
        std::sync::Arc::new(rust_cli_node_store::NodeIds::new(std::sync::Arc::new(
            rust_cli_node_store::SystemClock::utc(),
        ))),
    );
    let keys = [work, work.parent().unwrap()].map(|dir| TrustKey::new(dir.to_string_lossy()));
    store.trusted_workspace(&keys).await.unwrap()
}

#[tokio::test]
async fn print_mode_neither_asks_nor_records_trust() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let server = model_server();
    let output = zcode(&home, &work, &["-p", "hello"], &server);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout).trim(), "done");
    assert_eq!(trusted(&home, &work).await, None);
}

#[tokio::test]
async fn the_interface_without_a_terminal_fails_before_asking() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let output = zcode(&home, &work, &[], &model_server());
    assert!(!output.status.success());
    assert!(
        text(&output.stderr).contains("TUI requires an interactive terminal."),
        "{}",
        text(&output.stderr)
    );
    assert_eq!(trusted(&home, &work).await, None);
}
