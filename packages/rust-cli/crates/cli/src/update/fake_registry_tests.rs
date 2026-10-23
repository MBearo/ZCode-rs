//! 本地假 npm registry（docs/specs/rust-cli/update.md §12）：每个连接一个请求，按路径回复，记下收到的请求行与
//! `accept` 头。不连任何真实服务。

#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "测试直接搭本地 HTTP 夹具（docs/specs/rust-cli/README.md §4）"
)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::{Value, json};

/// 收到的请求：路径与 `accept` 头。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Seen {
    pub(crate) path: String,
    pub(crate) accept: Option<String>,
}

pub(crate) struct FakeRegistry {
    pub(crate) url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeRegistry {
    /// `routes`：路径（例如 `/@scope%2fzcode`）→（状态，正文）。没有的路径回 404。
    pub(crate) fn start(routes: Vec<(String, u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).unwrap_or(0) == 0 {
                    continue;
                }
                let mut accept = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("accept")
                    {
                        accept = Some(value.trim().to_owned());
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("").to_owned();
                log.lock().unwrap().push(Seen {
                    path: path.clone(),
                    accept,
                });
                let (status, body) = routes
                    .iter()
                    .find(|(route, _, _)| *route == path)
                    .map_or((404, "{}".to_owned()), |(_, status, body)| {
                        (*status, body.clone())
                    });
                let head = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        Self { url, seen }
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

/// 一个包的缩略元数据：`latest` 与带完整 dist 的版本。
pub(crate) fn package(name: &str, latest: &str, versions: &[&str]) -> String {
    let versions: serde_json::Map<String, Value> = versions
        .iter()
        .map(|version| {
            (
                (*version).to_owned(),
                json!({
                    "name": name,
                    "version": version,
                    "dist": {
                        "tarball": format!("https://registry.example.com/{name}/-/{version}.tgz"),
                        "integrity": format!("sha512-{version}"),
                    },
                }),
            )
        })
        .collect();
    json!({"name": name, "dist-tags": {"latest": latest}, "versions": versions}).to_string()
}

/// 没有代理的出口。
pub(crate) async fn egress() -> Egress {
    Egress::new(NetworkPolicy::default()).await.unwrap()
}
