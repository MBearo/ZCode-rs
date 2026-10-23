//! 本地假服务（项目令牌与登录的测试）：按测试给的处理函数回复，记录收到的请求。每个连接一个请求，回复带
//! content-length。不连任何真实服务。

#![expect(
    clippy::disallowed_methods,
    reason = "测试直接搭本地 HTTP 夹具（docs/specs/rust-cli/README.md §4）"
)]

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_net::{Egress, NetworkPolicy};
use rust_cli_paths::StoragePaths;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::credentials::{CredentialStore, Credentials};
use crate::test_support::{FixedClock, write};
use crate::{AccountOrigins, AccountServices, CredentialSecret, Platform};

/// 收到的一个请求。header 名为小写。
#[derive(Debug, Clone)]
pub struct Captured {
    pub method: String,
    pub path: String,
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

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

/// 一个回复：状态、正文与发送前的停顿。
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub delay: Duration,
}

impl Reply {
    pub fn json(status: u16, body: &Value) -> Self {
        Self {
            status,
            body: body.to_string(),
            delay: Duration::ZERO,
        }
    }

    pub fn after(mut self, millis: u64) -> Self {
        self.delay = Duration::from_millis(millis);
        self
    }
}

type Handler = Arc<dyn Fn(&Captured) -> Reply + Send + Sync>;

pub struct Server {
    port: u16,
    captured: Arc<Mutex<Vec<Captured>>>,
    task: JoinHandle<()>,
}

impl Server {
    pub async fn start(handler: impl Fn(&Captured) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let handler: Handler = Arc::new(handler);
        let log = captured.clone();
        let task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (log, handler) = (log.clone(), handler.clone());
                tokio::spawn(async move { serve(socket, log, handler).await });
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

    /// 路径以 `prefix` 开头的请求数。
    pub fn count(&self, prefix: &str) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.path.starts_with(prefix))
            .count()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(mut socket: TcpStream, log: Arc<Mutex<Vec<Captured>>>, handler: Handler) {
    let Some(request) = read_request(&mut socket).await else {
        return;
    };
    log.lock().unwrap().push(request.clone());
    let reply = handler(&request);
    tokio::time::sleep(reply.delay).await;
    let head = format!(
        "HTTP/1.1 {} Scripted\r\nconnection: close\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        reply.status,
        reply.body.len()
    );
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(reply.body.as_bytes()).await;
    let _ = socket.shutdown().await;
}

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
    let path = request_line.next()?.to_owned();
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
        path,
        headers,
        body,
    })
}

// ---- 账号测试的公共部分：指向假服务的平台层、可调的时钟、解密后的凭据 ----

/// 测试时钟的起点（毫秒）。
pub const NOW_MS: u64 = 1_700_000_000_000;
pub const ZAI: &str = "account:zai-individual-coding-plan";
pub const BIGMODEL: &str = "account:bigmodel-individual-coding-plan";

pub fn secret() -> CredentialSecret {
    CredentialSecret {
        explicit: Some(RedactedString::new("test-secret")),
        platform: "linux".to_owned(),
        home: "/Users/dev".to_owned(),
        username: "dev".to_owned(),
    }
}

pub struct Harness {
    /// 临时目录随测试结束删除。
    _dir: tempfile::TempDir,
    pub paths: StoragePaths,
    pub clock: Arc<FixedClock>,
    pub platform: Platform,
    secret: CredentialSecret,
}

impl Harness {
    /// 平台层：账号服务的地址全部指向 `url`。
    pub async fn new(url: &str) -> Self {
        Self::with_secret(url, secret()).await
    }

    /// 同上，凭据密钥是 `secret`（读 App 写的 golden 文件时用 App 的测试密钥，工作线 F50）。
    pub async fn with_secret(url: &str, secret: CredentialSecret) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
        let clock = Arc::new(FixedClock(AtomicU64::new(NOW_MS)));
        let platform = Self::platform_with(&paths, url, clock.clone(), &secret).await;
        Self {
            _dir: dir,
            paths,
            clock,
            platform,
            secret,
        }
    }

    /// 同一份文件上的另一个平台层（新进程）。
    pub async fn platform(paths: &StoragePaths, url: &str, clock: Arc<FixedClock>) -> Platform {
        Self::platform_with(paths, url, clock, &secret()).await
    }

    pub async fn platform_with(
        paths: &StoragePaths,
        url: &str,
        clock: Arc<FixedClock>,
        secret: &CredentialSecret,
    ) -> Platform {
        let services = AccountServices {
            egress: Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap()),
            clock,
            origins: AccountOrigins {
                zcode_api: format!("{url}/api/v1"),
                zai_api: url.to_owned(),
                bigmodel_api: url.to_owned(),
                bigmodel_team_plans: format!("{url}/coding-plan/team/plans"),
                client_version: "0.1.0-test".to_owned(),
            },
        };
        Platform::new(paths, secret, services)
    }

    pub fn advance(&self, millis: u64) {
        self.clock.0.fetch_add(millis, Ordering::SeqCst);
    }

    pub fn store(&self) -> CredentialStore {
        CredentialStore::new(self.paths.credentials(), &self.secret)
    }

    pub async fn credentials(&self) -> Credentials {
        self.store().read().await.unwrap()
    }

    /// 明文写一份凭据（Node 也接受没有 `enc:v1:` 前缀的旧值）。
    pub async fn write_credentials(&self, record: &Value) {
        write(self.paths.credentials().as_path(), record.to_string()).await;
    }
}

pub fn selection(provider: &str) -> ModelSelection {
    ModelSelection {
        provider: provider.to_owned(),
        model: "GLM-5.3".to_owned(),
        reasoning: None,
        connection: None,
    }
}

/// `header.<{"exp": exp}>.signature`。
pub fn jwt(exp_seconds: u64, tag: usize) -> String {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(json!({"exp": exp_seconds, "n": tag}).to_string());
    format!("header.{payload}.signature")
}

/// 一个账号服务的标准回复（Z.ai 与 BigModel 同形）：业务 JWT、customerInfo、key 列表 / 创建、签发、个人订阅。
/// 签发的 PAT 依次是 `pat-1`、`pat-2`……，有效期 1 小时。
pub struct Account {
    pub business: AtomicUsize,
    pub issued: AtomicUsize,
    /// key 列表里是否已有 `zcode-api-key`。
    pub has_key: bool,
}

impl Account {
    pub fn new(has_key: bool) -> Arc<Self> {
        Arc::new(Self {
            business: AtomicUsize::new(0),
            issued: AtomicUsize::new(0),
            has_key,
        })
    }

    pub fn reply(&self, request: &Captured) -> Reply {
        let now = NOW_MS / 1000;
        let ok = |data: Value| Reply::json(200, &json!({"code": 200, "data": data}));
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/api/auth/z/login") => {
                let n = self.business.fetch_add(1, Ordering::SeqCst) + 1;
                Reply::json(
                    200,
                    &json!({"code": 0, "data": {"access_token": jwt(now + 7200, n)}}),
                )
            }
            ("GET", "/api/biz/customer/getCustomerInfo") => ok(json!({"organizations": [
                {"organizationId": "org-x", "organizationName": "其他", "projects": [{"projectId": "p-x"}]},
                {"organizationId": "org-1", "organizationName": "默认机构（个人）", "projects": [
                    {"projectId": "p-2", "projectName": "实验", "projectType": 2},
                    {"projectId": "p-1", "projectName": "默认项目", "projectType": "1"}
                ]}
            ]})),
            ("GET", "/api/biz/v1/organization/org-1/projects/p-1/api_keys") => {
                let mut keys = vec![json!({"name": "other", "apiKey": "key-0"})];
                if self.has_key {
                    keys.push(json!({"name": "zcode-api-key", "apiKey": " key-1 ", "keyType": 1}));
                }
                ok(Value::Array(keys))
            }
            ("POST", "/api/biz/v1/organization/org-1/projects/p-1/api_keys") => {
                ok(json!({"name": "zcode-api-key", "apiKey": "key-1"}))
            }
            // 个人订阅（App `/api/biz/subscription/list`，工作线 F50）：一个有效的 Coding Plan。
            ("GET", "/api/biz/subscription/list") => ok(json!([
                {"productName": "GLM Coding Plan Pro", "status": "VALID", "inCurrentPeriod": true}
            ])),
            (
                "POST",
                "/api/biz/v1/organization/org-1/projects/p-1/api_keys/key-1/access_tokens",
            ) => {
                let n = self.issued.fetch_add(1, Ordering::SeqCst) + 1;
                ok(json!({
                    "accessToken": format!("pat-{n}"),
                    "tokenType": "Bearer",
                    "expiresIn": 3600,
                    "expiresAt": now + 3600,
                }))
            }
            _ => Reply::json(404, &json!({"code": 404})),
        }
    }
}
