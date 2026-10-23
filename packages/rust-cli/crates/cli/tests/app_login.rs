//! 只在 App 里登录过的用户（工作线 F50，docs/specs/rust-cli/platform.md §5.2、§5.7）：真实进程 `zcode -p` 读 App 写的
//! `credentials.json`（`app-login-golden.json` 里 App 的轮询登录写出的加密文件原文），按 App 的 profile id 换个人项目
//! 令牌、查个人订阅，用个人套餐请求模型；没有订阅时不可用。账号服务与模型都是本地假服务，home、数据目录与密钥都是
//! 临时的测试值。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具"
)]

mod system_env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use system_env::KeepSystem as _;

use serde_json::{Value, json};

const REPLY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",",
    "\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

fn golden() -> Value {
    serde_json::from_str(include_str!(
        "../../platform/tests/fixtures/app-login-golden.json"
    ))
    .unwrap()
}

/// 收到的请求：方法、路径与鉴权头。
type Seen = Arc<Mutex<Vec<(String, String, Option<String>)>>>;

/// 账号服务 + 模型：golden 的 customerInfo，个人项目 org-1/p-1 的 key 要创建；`subscription` 是订阅列表的 `status`。
fn server(subscription: &'static str) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let customer = golden()["customer"].clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut parts = first.split(' ');
            let method = parts.next().unwrap_or_default().to_owned();
            let path = parts.next().unwrap_or_default().to_owned();
            let mut authorization = None;
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    match name.trim().to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().unwrap_or(0),
                        "authorization" => authorization = Some(value.trim().to_owned()),
                        _ => {}
                    }
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            log.lock()
                .unwrap()
                .push((method.clone(), path.clone(), authorization));
            let ok = |data: Value| json!({"code": 200, "data": data}).to_string();
            let keys = "/api/biz/v1/organization/org-1/projects/p-1/api_keys";
            let (kind, reply) = match (method.as_str(), path.as_str()) {
                ("GET", "/api/biz/customer/getCustomerInfo") => {
                    ("application/json", ok(customer.clone()))
                }
                ("GET", p) if p == keys => ("application/json", ok(json!([]))),
                ("POST", p) if p == keys => (
                    "application/json",
                    ok(json!({"name": "zcode-api-key", "apiKey": "key-1"})),
                ),
                ("POST", p) if p == format!("{keys}/key-1/access_tokens") => (
                    "application/json",
                    ok(
                        json!({"accessToken": "pat-1", "tokenType": "Bearer", "expiresIn": 3600, "expiresAt": now + 3600}),
                    ),
                ),
                ("GET", "/api/biz/subscription/list") => (
                    "application/json",
                    ok(
                        json!([{"productName": "GLM Coding Plan", "status": subscription, "inCurrentPeriod": subscription == "VALID"}]),
                    ),
                ),
                (_, p) if p.contains("/v1/messages") => ("text/event-stream", REPLY.to_owned()),
                _ => ("application/json", json!({"code": 404}).to_string()),
            };
            let status = if reply.contains("\"code\":404") {
                "404 Not Found"
            } else {
                "200 OK"
            };
            let head = format!(
                "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                reply.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (format!("http://{address}"), seen)
}

/// 临时 home：App 写的凭据文件原文（只有 App 的键）；内置配置里这个 family 的个人 provider 指向假服务。
fn prepare(home: &Path, url: &str, family: &str) -> std::path::PathBuf {
    let v2 = home.join(".zcode/v2");
    std::fs::create_dir_all(&v2).unwrap();
    let file = golden()["logins"][family]["loginFile"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::write(v2.join("credentials.json"), file).unwrap();
    let bundled = include_str!("../../platform/assets/zcode-builtin.json");
    let mut builtin: Value = serde_json::from_str(bundled).unwrap();
    let provider = format!("account:{family}-individual-coding-plan");
    for rule in builtin["config"]["providerConfigRules"]["providerRules"]
        .as_array_mut()
        .unwrap()
    {
        if rule["providerId"] == provider.as_str() {
            rule["config"]["api"]["baseUrl"] = json!(format!("{url}/api/anthropic"));
        }
    }
    let file = home.join("builtin.json");
    std::fs::write(&file, builtin.to_string()).unwrap();
    file
}

fn zcode(home: &Path, builtin: &Path, url: &str) -> Output {
    let work = home.join("work");
    std::fs::create_dir_all(work.join(".git")).unwrap();
    let secret = golden()["secret"].as_str().unwrap().to_owned();
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(["-p", "hello"])
        .current_dir(&work)
        .env_clear()
        .keep_system()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", home)
        .env("ZCODE_CREDENTIAL_SECRET", secret)
        .env("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE", builtin)
        .env("ZAI_BUSINESS_BASE_URL", url)
        .env("BIGMODEL_API_BASE_URL", url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `decodeURIComponent`（键里只有 ASCII 的百分号编码）。
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            out.push(u8::from_str_radix(&text[index + 1..index + 3], 16).unwrap());
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

#[test]
fn print_mode_uses_the_individual_plan_from_the_app_login() {
    for family in ["zai", "bigmodel"] {
        let (url, seen) = server("VALID");
        let dir = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap();
        let builtin = prepare(&home, &url, family);
        let output = zcode(&home, &builtin, &url);
        assert!(
            output.status.success(),
            "{family}: {}",
            text(&output.stderr)
        );
        assert_eq!(text(&output.stdout).trim(), "done");
        let seen = seen.lock().unwrap().clone();
        let subscription = seen
            .iter()
            .find(|r| r.1 == "/api/biz/subscription/list")
            .unwrap();
        assert_eq!(subscription.2.as_deref(), Some("pat-1"), "{family}");
        let model = seen.iter().find(|r| r.1.contains("/v1/messages")).unwrap();
        assert_eq!(model.2.as_deref(), Some("Bearer pat-1"), "{family}");
        // 项目位置的键与 App 同一算法：账号是 App 写的 user_info 的 profile id。
        let raw: Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".zcode/v2/credentials.json")).unwrap(),
        )
        .unwrap();
        let locations: Vec<String> = raw
            .as_object()
            .unwrap()
            .keys()
            .filter_map(|key| key.strip_prefix("project-key-location:v1:"))
            .map(decode)
            .collect();
        let profile = golden()["logins"][family]["profileId"].clone();
        let expected = json!([url, family, profile, null, 1, "non-team"]).to_string();
        assert_eq!(locations, [expected], "{family}");
        // 不替 App 写键。
        assert!(
            raw.as_object()
                .unwrap()
                .keys()
                .all(|key| !key.starts_with("account-provider:")),
            "{family}"
        );
    }
}

#[test]
fn without_an_individual_subscription_the_plan_is_not_used() {
    let (url, seen) = server("EXPIRED");
    let dir = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(dir.path()).unwrap();
    let builtin = prepare(&home, &url, "zai");
    let output = zcode(&home, &builtin, &url);
    assert!(!output.status.success(), "{}", text(&output.stdout));
    let seen = seen.lock().unwrap();
    assert!(seen.iter().any(|r| r.1 == "/api/biz/subscription/list"));
    assert!(!seen.iter().any(|r| r.1.contains("/v1/messages")));
}
