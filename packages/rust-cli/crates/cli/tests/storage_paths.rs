//! 会话库与存储根的位置（修复线 F2，docs/specs/rust-cli/config.md §2.3）：真实的 `zcode -p`，临时 home 与本地假的
//! Anthropic 端点。库按 Node 的顺序解析（`ZCODE_SESSION_DB_PATH` / `ZCODE_SESSION_DB` 覆盖配置文件的
//! `storage.sessionDbPath`），存储根跟随 `storage.dir`（自动记忆目录建在它下面）；缺省位置不再出现库。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、检查写出的文件"
)]

mod system_env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Stdio;
use system_env::KeepSystem as _;

const REPLY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",",
    "\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

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

/// 在 `work` 里跑一次 `zcode -p`，`extra` 是额外的环境变量。
fn run(home: &Path, work: &Path, extra: &[(&str, &Path)]) {
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(["-p", "storage probe"])
        .current_dir(work)
        .env_clear()
        .keep_system()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", home)
        .env("ZCODE_MODEL_BASE_URL", model_server())
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "claude-opus-5-5");
    for (name, value) in extra {
        command.env(name, value);
    }
    let output = command
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
}

#[test]
fn the_session_db_and_the_storage_root_follow_node_settings() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(home.join(".zcode/cli")).unwrap();
    let config = serde_json::json!({"storage": {
        "dir": base.join("store"),
        "sessionDbPath": base.join("config.sqlite"),
    }});
    std::fs::write(home.join(".zcode/cli/config.json"), config.to_string()).unwrap();
    // 配置文件的 `storage.sessionDbPath`。
    run(&home, &work, &[]);
    assert!(base.join("config.sqlite").exists());
    // 环境变量覆盖配置文件（Node `parseEnvConfig`）。
    run(
        &home,
        &work,
        &[("ZCODE_SESSION_DB_PATH", &base.join("env.sqlite"))],
    );
    assert!(base.join("env.sqlite").exists());
    run(
        &home,
        &work,
        &[("ZCODE_SESSION_DB", &base.join("short.sqlite"))],
    );
    assert!(base.join("short.sqlite").exists());
    assert!(!home.join(".zcode/cli/db/db.sqlite").exists());
    // `storage.dir` 是存储根：自动记忆目录在它下面（`<storage.dir>/projects/<项目>/memory`）。
    let projects: Vec<_> = std::fs::read_dir(base.join("store/projects"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(projects.len(), 1, "{projects:?}");
    assert!(projects[0].join("memory").is_dir());
    assert!(!home.join(".zcode/projects").exists());
}
