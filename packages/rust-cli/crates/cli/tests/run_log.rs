//! 运行日志的真实进程（docs/specs/rust-cli/logging.md §7）：临时 home 与本地假的 Anthropic 端点，`zcode -p`
//! 跑一次后，`<home>/.zcode/rust-cli/log/` 下有当天的 JSONL 文件，生命周期事件都在，且不含提示词与密钥。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、检查写出的文件"
)]

mod system_env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Stdio;
use system_env::KeepSystem as _;

use serde_json::Value;

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

#[test]
fn a_print_run_writes_info_lines_to_the_daily_log() {
    const PROMPT: &str = "log-probe-prompt-7f3a";
    const KEY: &str = "test-key-log-probe-91c2";
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let server = model_server();
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", PROMPT])
        .current_dir(&work)
        .env_clear()
        .keep_system()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &home)
        .env("ZCODE_MODEL_BASE_URL", &server)
        .env("ZCODE_API_KEY", KEY)
        .env("ZCODE_MODEL", "claude-opus-5-5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "done");
    // 日志只写文件，不写终端。
    assert!(!stderr.contains("process.start"), "{stderr}");

    let log = home.join(".zcode/rust-cli/log");
    let files: Vec<_> = std::fs::read_dir(&log)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("zcode-") && name.ends_with(".jsonl") && name.len() == 22,
        "{name}"
    );
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert!(
        !text.contains(PROMPT),
        "the prompt leaked into the log:\n{text}"
    );
    assert!(
        !text.contains(KEY),
        "the API key leaked into the log:\n{text}"
    );
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let info: Vec<&str> = lines
        .iter()
        .filter(|line| line["level"] == "info")
        .filter_map(|line| line["event"].as_str())
        .collect();
    for event in [
        "process.start",
        "session.open",
        "model.request.start",
        "model.request.end",
        "process.exit",
    ] {
        assert!(info.contains(&event), "missing {event} in {info:?}\n{text}");
    }
    assert_eq!(info.first(), Some(&"process.start"));
    assert_eq!(info.last(), Some(&"process.exit"));
    let exit = lines
        .iter()
        .find(|line| line["event"] == "process.exit")
        .unwrap();
    assert_eq!(exit["status"], "completed");
    let start = lines
        .iter()
        .find(|line| line["event"] == "model.request.start")
        .unwrap();
    assert!(start["sessionId"].as_str().unwrap().starts_with("sess_"));
}
