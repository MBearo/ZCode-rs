//! 进程内的本地 HTTP/1.1 夹具：记录收到的请求，按脚本逐个回复。
//!
//! 每个连接只处理一个请求，回复带 `connection: close`、不带 `content-length`，响应体到连接关闭为止，
//! 所以脚本可以任意切分响应字节、在块之间停顿，或者停在半途不再发送。

#![allow(
    clippy::disallowed_methods,
    reason = "测试直接搭本地 HTTP 夹具（docs/specs/rust-cli/README.md §4）"
)]
#![allow(
    clippy::unwrap_used,
    reason = "夹具不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]
#![allow(dead_code, reason = "net 与 model 的测试各用夹具的一部分")]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// 收到的一个请求。header 名为小写。
#[derive(Debug, Clone)]
pub struct Captured {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Captured {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// 回复里的一步。
#[derive(Debug, Clone)]
pub enum Step {
    Send(Vec<u8>),
    Pause(Duration),
    /// 不再发送，也不关闭连接（测试取消与超时）。
    Hang,
}

#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub steps: Vec<Step>,
    /// 收到请求后、发送响应头之前的停顿。
    pub delay_head: Option<Duration>,
}

impl Reply {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            steps: Vec::new(),
            delay_head: None,
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn send(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.steps.push(Step::Send(bytes.into()));
        self
    }

    pub fn pause(mut self, millis: u64) -> Self {
        self.steps.push(Step::Pause(Duration::from_millis(millis)));
        self
    }

    pub fn hang(mut self) -> Self {
        self.steps.push(Step::Hang);
        self
    }

    pub fn delay_head(mut self, millis: u64) -> Self {
        self.delay_head = Some(Duration::from_millis(millis));
        self
    }
}

pub struct Fixture {
    port: u16,
    captured: Arc<Mutex<Vec<Captured>>>,
    task: JoinHandle<()>,
}

impl Fixture {
    pub async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let log = captured.clone();
        let task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let log = log.clone();
                let replies = replies.clone();
                tokio::spawn(async move { serve(socket, log, replies).await });
            }
        });
        Self {
            port,
            captured,
            task,
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn requests(&self) -> Vec<Captured> {
        self.captured.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    mut socket: TcpStream,
    log: Arc<Mutex<Vec<Captured>>>,
    replies: Arc<Mutex<VecDeque<Reply>>>,
) {
    let Some(request) = read_request(&mut socket).await else {
        return;
    };
    log.lock().unwrap().push(request);
    let reply = replies.lock().unwrap().pop_front().unwrap_or_else(|| {
        Reply::new(500)
            .send(r#"{"type":"error","error":{"type":"api_error","message":"no scripted reply"}}"#)
    });
    if let Some(delay) = reply.delay_head {
        tokio::time::sleep(delay).await;
    }
    let mut head = format!(
        "HTTP/1.1 {} Scripted\r\nconnection: close\r\n",
        reply.status
    );
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    if socket.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    for step in reply.steps {
        match step {
            Step::Send(bytes) => {
                if socket.write_all(&bytes).await.is_err() || socket.flush().await.is_err() {
                    return;
                }
            }
            Step::Pause(duration) => tokio::time::sleep(duration).await,
            Step::Hang => std::future::pending::<()>().await,
        }
    }
    let _ = socket.shutdown().await;
}

/// 读一个 HTTP 请求：请求行、header，按 `content-length` 读 body。
async fn read_request(socket: &mut TcpStream) -> Option<Captured> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(at) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    while bytes.len() < head_end + length {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    let body = String::from_utf8_lossy(&bytes[head_end..head_end + length]).into_owned();
    Some(Captured {
        method,
        target,
        headers,
        body,
    })
}
