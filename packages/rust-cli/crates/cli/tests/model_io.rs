//! 模型轨迹的真实进程（docs/specs/rust-cli/model-io.md §8）：临时 home 与本地假的 Anthropic 端点，`zcode -p` 跑一次后，
//! `<home>/.zcode/cli/{debug,rollout}/` 下有这个会话的 `model-io-<会话>.jsonl`，形状符合 spec；回复里解码器不认识的块
//! 记进 `decoder.skippedBlocks`；生产模式按 Node 的规则轮转；测试环境（`ZCODE_RUNTIME_ENV=test`）不写；文件里没有
//! API key。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、检查写出的文件"
)]

mod system_env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, UNIX_EPOCH};
use system_env::KeepSystem as _;

use serde_json::Value;

const KEY: &str = "test-key-model-io-5d81c0";
const PROMPT: &str = "model-io-probe";

/// 一段回复：文本块之后是一个解码器不认识的块（某些兼容端点在主回复里发的 `tool_result`）。
const REPLY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_io_1\",\"type\":\"message\",",
    "\"role\":\"assistant\",\"model\":\"m-reply\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":7,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_result\",\"tool_use_id\":\"srv_1\",\"content\":\"odd block\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

/// 每个连接回同一段 SSE 回复。
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
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nrequest-id: req_io_1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                REPLY.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(REPLY.as_bytes());
        }
    });
    format!("http://{address}")
}

struct Run {
    home: PathBuf,
    _dir: tempfile::TempDir,
}

/// 准备临时 home（`prepare` 可以先放文件），跑一次 `zcode -p`；`runtime` 是 `ZCODE_RUNTIME_ENV` 的值。
fn run(runtime: Option<&str>, prepare: impl FnOnce(&Path)) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    prepare(&home);
    let server = model_server();
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
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
        .stderr(Stdio::piped());
    if let Some(value) = runtime {
        command.env("ZCODE_RUNTIME_ENV", value);
    }
    let output = command.output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "done");
    Run { home, _dir: dir }
}

fn model_io_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default();
    files.retain(|path| {
        let name = path.file_name().unwrap().to_string_lossy();
        name.starts_with("model-io-") && name.ends_with(".jsonl")
    });
    files.sort();
    files
}

/// 这个目录里唯一的会话文件（`model-io-sess_*.jsonl`）的记录。
fn session_records(dir: &Path) -> (String, Vec<Value>) {
    let files: Vec<PathBuf> = model_io_files(dir)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("model-io-sess_")
        })
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert!(!text.contains(KEY), "the API key leaked:\n{text}");
    let records = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    (name, records)
}

#[test]
fn a_debug_build_records_the_round_under_cli_debug() {
    let run = run(None, |_| {});
    let dir = run.home.join(".zcode/cli/debug");
    let (name, records) = session_records(&dir);
    assert!(!run.home.join(".zcode/cli/rollout").exists());
    assert_eq!(records.len(), 1, "{records:#?}");
    let record = &records[0];
    let session = record["sessionId"].as_str().unwrap();
    assert!(session.starts_with("sess_"), "{record:#}");
    assert_eq!(name, format!("model-io-{session}.jsonl"));
    assert_eq!(record["type"], "model_io");
    assert_eq!(record["attempt"], 1);
    assert_eq!(record["querySource"], "main_turn");
    assert!(record["turnId"].as_str().unwrap().starts_with("run_"));
    assert!(!record["requestId"].as_str().unwrap().is_empty());
    assert_eq!(record["model"]["modelId"], "claude-opus-5-5");
    assert_eq!(record["model"]["protocol"], "anthropic-messages");
    // 请求：轨迹比对基线的形状，头里的 key 已脱敏，请求体是发出的 JSON。
    let request = &record["request"];
    assert_eq!(request["route"], "messages");
    assert_eq!(request["class"], "main");
    assert_eq!(request["method"], "POST");
    assert_eq!(request["path"], "/v1/messages?beta=true");
    assert_eq!(request["headers"]["x-api-key"], "[redacted]");
    assert_eq!(request["body"]["model"], "claude-opus-5-5");
    assert_eq!(request["body"]["stream"], true);
    assert!(request["body"]["messages"].to_string().contains(PROMPT));
    assert_eq!(request["bodyMessagesKind"], "full");
    // 回复：原始事件、汇总、跳过的块与结局。
    let response = &record["response"];
    assert_eq!(response["status"], 200);
    assert_eq!(response["requestId"], "req_io_1");
    assert_eq!(response["data"]["stream"], true);
    let chunks = response["data"]["chunks"].as_array().unwrap();
    assert_eq!(chunks.len(), 8, "{chunks:#?}");
    assert_eq!(chunks[4]["content_block"]["type"], "tool_result");
    assert_eq!(response["responseId"], "msg_io_1");
    // 工作线 F93（model-io.md §4.2）：App 读的回复字段是 Node 的形状。
    assert_eq!(response["finishReason"], "stop");
    assert_eq!(response["usage"]["inputTokens"], 7);
    assert!(response["toolCalls"].is_array(), "{response:#}");
    assert_eq!(
        record["decoder"]["skippedBlocks"],
        serde_json::json!([{"index": 1, "type": "tool_result"}])
    );
    assert_eq!(record["outcome"], "completed");
    assert!(record.get("error").is_none(), "{record:#}");
}

#[test]
fn production_rotates_the_shared_directory_like_node() {
    // 生产模式写 rollout：目录里已有 3 个别的会话文件时删掉最旧的一个，留位给新会话（最多 3 个）。
    let run = run(Some("production"), |home| {
        let dir = home.join(".zcode/cli/rollout");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, age) in [
            ("model-io-a.jsonl", 3),
            ("model-io-b.jsonl", 2),
            ("model-io-c.jsonl", 1),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, "{}\n").unwrap();
            let when = UNIX_EPOCH + Duration::from_secs(1_700_000_000 - age * 1000);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
        std::fs::write(dir.join("notes.txt"), "kept").unwrap();
    });
    let dir = run.home.join(".zcode/cli/rollout");
    let (_, records) = session_records(&dir);
    assert_eq!(records[0]["outcome"], "completed");
    let names: Vec<String> = model_io_files(&dir)
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(names.contains(&"model-io-b.jsonl".to_owned()), "{names:?}");
    assert!(names.contains(&"model-io-c.jsonl".to_owned()), "{names:?}");
    assert!(dir.join("notes.txt").exists());
    assert!(!run.home.join(".zcode/cli/debug").exists());
}

#[test]
fn the_test_environment_writes_nothing() {
    let run = run(Some("test"), |_| {});
    assert!(!run.home.join(".zcode/cli/debug").exists());
    assert!(!run.home.join(".zcode/cli/rollout").exists());
}
