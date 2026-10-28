//! 真实进程测试用的 Anthropic SSE 夹具：主请求（带工具定义的）按剧本逐个回复并记下请求体，其余请求
//! （标题等旁路）回一段文字。每个连接只处理一个请求（`connection: close`）。
#![allow(dead_code, reason = "不同的测试文件用到的辅助函数不同")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

fn event(kind: &str, data: Value) -> String {
    let mut data = data;
    data["type"] = json!(kind);
    format!("event: {kind}\ndata: {data}\n\n")
}

fn start() -> String {
    start_with(1)
}

/// `message_start`，用量里的输入是 `input_tokens`（压缩的 token 以最近一次用量为锚点，修复线 F61）。
fn start_with(input_tokens: u64) -> String {
    event(
        "message_start",
        json!({"message": {"id": "msg_fx", "type": "message", "role": "assistant", "model": "m",
            "content": [], "stop_reason": null, "usage": {"input_tokens": input_tokens, "output_tokens": 1}}}),
    )
}

fn end(stop: &str) -> String {
    event(
        "message_delta",
        json!({"delta": {"stop_reason": stop}, "usage": {"output_tokens": 1}}),
    ) + &event("message_stop", json!({}))
}

/// 一个 tool_use 块；`input` 是 `input_json_delta` 的原文（可以不是合法 JSON）。
pub fn tool_block(index: usize, id: &str, name: &str, input: &str) -> String {
    event(
        "content_block_start",
        json!({"index": index, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}),
    ) + &event(
        "content_block_delta",
        json!({"index": index, "delta": {"type": "input_json_delta", "partial_json": input}}),
    ) + &event("content_block_stop", json!({"index": index}))
}

/// 只有 tool_use 块的响应。
pub fn tools_reply(blocks: &[String]) -> String {
    start() + &blocks.concat() + &end("tool_use")
}

/// 同 [`tools_reply`]，用量里的输入是 `input_tokens`。
pub fn tools_reply_with_usage(blocks: &[String], input_tokens: u64) -> String {
    start_with(input_tokens) + &blocks.concat() + &end("tool_use")
}

/// 一个 SSE 事件（`type` 取 `kind`），用来拼端点自己产生的块（工作线 F90）。
pub fn raw_event(kind: &str, data: Value) -> String {
    event(kind, data)
}

/// 由任意事件组成的响应：`message_start`、`events`、按 `stop` 收尾。
pub fn reply_of(events: &[String], stop: &str) -> String {
    start() + &events.concat() + &end(stop)
}

/// 一段文字的响应。
pub fn text_reply(text: &str) -> String {
    start()
        + &event(
            "content_block_start",
            json!({"index": 0, "content_block": {"type": "text", "text": ""}}),
        )
        + &event(
            "content_block_delta",
            json!({"index": 0, "delta": {"type": "text_delta", "text": text}}),
        )
        + &event("content_block_stop", json!({"index": 0}))
        + &end("end_turn")
}

/// 一个请求的 header（名字小写，值去掉首尾空白）。
pub type Headers = Vec<(String, String)>;

/// 夹具的地址与收到的主请求体。
pub struct Fixture {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Value>>>,
    /// 主请求的 `anthropic-beta` header（没有时为 None），与 `requests` 同序（工作线 F52）。
    pub betas: Arc<Mutex<Vec<Option<String>>>>,
    /// 主请求的全部 header（名字小写），与 `requests` 同序（工作线 F64）。
    pub headers: Arc<Mutex<Vec<Headers>>>,
    /// 全部请求（不只主请求）的请求行的路径，按到达顺序（工作线 F93：`/context` 不发 count_tokens）。
    pub paths: Arc<Mutex<Vec<String>>>,
    /// 旁路请求（不带工具定义的）的请求体，按到达顺序（工作线 F97：标题请求）。
    pub side: Arc<Mutex<Vec<Value>>>,
}

impl Fixture {
    pub fn main_requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    pub fn main_betas(&self) -> Vec<Option<String>> {
        self.betas.lock().unwrap().clone()
    }

    pub fn main_headers(&self) -> Vec<Headers> {
        self.headers.lock().unwrap().clone()
    }

    pub fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }

    pub fn side_requests(&self) -> Vec<Value> {
        self.side.lock().unwrap().clone()
    }
}

/// 剧本的一步：固定的回复，或按这一次主请求的请求体生成（要用只有请求里才有的值，例如落盘位置）；`Status` 是一个
/// 非 2xx 的 JSON 回复（修复线 F108：端点报上下文超长）。
pub enum Reply {
    Fixed(String),
    From(Box<dyn Fn(&Value) -> String + Send>),
    Status { status: u16, body: String },
}

/// 起夹具：第 n 个主请求回 `script[n]`，剧本用完后回一段文字。
pub fn serve(script: Vec<String>) -> Fixture {
    serve_with(script.into_iter().map(Reply::Fixed).collect())
}

/// 同 [`serve`]，剧本的每一步可以按请求体生成。
pub fn serve_with(script: Vec<Reply>) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let betas = Arc::new(Mutex::new(Vec::new()));
    let recorded_betas = betas.clone();
    let headers = Arc::new(Mutex::new(Vec::new()));
    let recorded_headers = headers.clone();
    let paths = Arc::new(Mutex::new(Vec::new()));
    let recorded_paths = paths.clone();
    let side = Arc::new(Mutex::new(Vec::new()));
    let recorded_side = side.clone();
    std::thread::spawn(move || {
        let mut script = script.into_iter();
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            let mut beta = None;
            let mut lines = Vec::new();
            let mut request_line = String::new();
            let _ = reader.read_line(&mut request_line);
            if let Some(path) = request_line.split_whitespace().nth(1) {
                recorded_paths.lock().unwrap().push(path.to_owned());
            }
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    lines.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("anthropic-beta:") {
                    beta = Some(value.trim().to_owned());
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let main = body["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty());
            let mut status = (200, "OK", "text/event-stream");
            let reply = if main {
                let reply = match script.next() {
                    Some(Reply::Fixed(reply)) => reply,
                    Some(Reply::From(make)) => make(&body),
                    Some(Reply::Status { status: code, body }) => {
                        status = (code, "Error", "application/json");
                        body
                    }
                    None => text_reply("done"),
                };
                recorded.lock().unwrap().push(body);
                recorded_betas.lock().unwrap().push(beta);
                recorded_headers.lock().unwrap().push(lines);
                reply
            } else {
                recorded_side.lock().unwrap().push(body);
                text_reply("Side")
            };
            let (code, reason, kind) = status;
            let head = format!(
                "HTTP/1.1 {code} {reason}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                reply.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    Fixture {
        url,
        requests,
        betas,
        headers,
        paths,
        side,
    }
}
