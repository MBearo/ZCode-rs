//! 时区的真实进程（docs/specs/rust-cli/time-zone.md §4）：临时 home 与本地假的 Anthropic 端点，按不同的 `TZ`
//! 跑 `zcode -p`，请求里 `Today's date is …` 与运行日志的文件名都是该时区的本地日期。
//!
//! 修复（R2-6）：以前系统时钟不认时区，一律给 UTC 的日期。UTC+14 与 UTC−12 相差 26 小时，两者的日期总是不同，
//! 旧代码上这一项一定失败（不依赖测试运行的时刻）。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、读系统时间算期望的日期"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use rust_cli_kernel_api::clock::civil_date;

const REPLY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",",
    "\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

/// 每个连接回同一段 SSE 文本回复，并把请求体交给测试。
fn model_server() -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
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
            let _ = sender.send(String::from_utf8_lossy(&body).into_owned());
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                REPLY.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(REPLY.as_bytes());
        }
    });
    (format!("http://{address}"), receiver)
}

fn local_date(offset_minutes: i64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let local = i64::try_from(now.as_millis()).unwrap() + offset_minutes * 60_000;
    civil_date(u64::try_from(local).unwrap())
}

/// 按 `tz` 跑一次 `zcode -p`：返回请求里的日期、运行日志的文件名，以及运行前后按偏移算的期望日期（跨过午夜时
/// 两者不同，接受其一）。
fn run(tz: &str, offset_minutes: i64) -> (String, String, [String; 2]) {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let (server, bodies) = model_server();
    let before = local_date(offset_minutes);
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", "what day is it"])
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("TZ", tz)
        .env("ZCODE_DATA_BASE_DIR", &home)
        .env("ZCODE_MODEL_BASE_URL", &server)
        .env("ZCODE_API_KEY", "test-key-time-zone")
        .env("ZCODE_MODEL", "claude-opus-5-5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let after = local_date(offset_minutes);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body = bodies
        .try_iter()
        .find(|body| body.contains("Today's date is "))
        .expect("a request carries the date");
    let date = body
        .split("Today's date is ")
        .nth(1)
        .and_then(|rest| rest.get(..10))
        .unwrap()
        .to_owned();
    let log = home.join(".zcode/rust-cli/log");
    let files: Vec<String> = std::fs::read_dir(&log)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    (date, files[0].clone(), [before, after])
}

fn assert_local(tz: &str, offset_minutes: i64) -> String {
    let (date, log, expected) = run(tz, offset_minutes);
    assert!(expected.contains(&date), "{tz}: {date} not in {expected:?}");
    assert_eq!(log, format!("zcode-{date}.jsonl"), "{tz}");
    date
}

#[test]
fn the_prompt_date_and_the_log_file_follow_the_time_zone() {
    let east = assert_local("Etc/GMT-14", 14 * 60);
    let west = assert_local("Etc/GMT+12", -12 * 60);
    assert_ne!(
        east, west,
        "UTC+14 and UTC-12 are always on different dates"
    );
    assert_local("Asia/Shanghai", 8 * 60);
    assert_local("UTC", 0);
}
