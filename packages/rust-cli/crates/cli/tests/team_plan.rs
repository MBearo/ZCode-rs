//! Team Plan 的真实进程（docs/specs/rust-cli/platform.md §9，工作线 F47、F49）：`zcode -p` 列出团队项目、查每个项目
//! 的权益、换所选团队项目的令牌、用它请求模型；App 在 `setting.json` 里选的连接不影响它；`--model` 的 `[<团队>]`
//! 选团队项目（cli-args.md §1.3）；套餐过期时报 App 的文案。账号服务与模型都是本地假服务，home、数据目录与密钥都是
//! 临时的。
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

/// 收到的请求：方法、路径与 header（名字小写）。
type Seen = Arc<Mutex<Vec<(String, String, Vec<(String, String)>)>>>;

/// 账号服务 + 模型：一个已订阅产品，org-t 下两个团队项目 p-t、p-u；`status` 是两个项目的订阅详情的 `status`。
fn server(status: &'static str) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let log = seen.clone();
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
            let mut headers = Vec::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    let name = name.trim().to_ascii_lowercase();
                    if name == "content-length" {
                        length = value.trim().parse().unwrap_or(0);
                    }
                    headers.push((name, value.trim().to_owned()));
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            log.lock()
                .unwrap()
                .push((method.clone(), path.clone(), headers));
            let ok = |data: Value| json!({"code": 200, "data": data}).to_string();
            let project = ["p-t", "p-u"]
                .into_iter()
                .find(|p| path.contains(&format!("/projects/{p}/")));
            let (kind, reply) = match path.as_str() {
                "/api/biz/subscription/enterprise/v2/pricing" => (
                    "application/json",
                    ok(json!({"productList": [
                        {"productId": "prod-1", "tier": "PRO", "subscribed": true, "organizationId": "org-t"}
                    ]})),
                ),
                "/api/biz/customer/getCustomerInfo" => (
                    "application/json",
                    ok(json!({"organizations": [
                        {"organizationId": "org-t", "organizationName": "团队", "projects": [
                            {"projectId": "p-t", "projectType": "2"},
                            {"projectId": "p-u", "projectType": "2"}
                        ]}
                    ]})),
                ),
                "/api/biz/team/subscribe/product/querySubscribeDetail" => (
                    "application/json",
                    ok(
                        json!({"hasSubscription": true, "status": status, "memberGrantStatus": "VALID"}),
                    ),
                ),
                p if p.ends_with("/api_keys") && project.is_some() => (
                    "application/json",
                    ok(
                        json!([{"name": "zcode-team-api-key", "apiKey": format!("key-{}", project.unwrap()), "keyType": 2}]),
                    ),
                ),
                p if p.ends_with("/access_tokens") && project.is_some() => (
                    "application/json",
                    ok(
                        json!({"accessToken": format!("pat-{}", project.unwrap()), "tokenType": "Bearer", "expiresIn": 3600, "expiresAt": now + 3600}),
                    ),
                ),
                p if p.contains("/v1/messages") => ("text/event-stream", REPLY.to_owned()),
                _ => ("application/json", json!({"code": 404}).to_string()),
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                reply.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (format!("http://{address}"), seen)
}

/// 临时 home：bigmodel 的 OAuth 登录（只有 App 写的键）；账号没有个人项目，个人项目令牌换不到、个人订阅是 unknown，
/// 个人套餐不可用（工作线 F50 照 App，platform.md §5.7）；App 在 `setting.json` 里选了个人套餐（本工程不读）；内置配置
/// 里团队 provider 的地址指向假服务。
fn prepare(home: &Path, url: &str) -> std::path::PathBuf {
    let v2 = home.join(".zcode/v2");
    std::fs::create_dir_all(&v2).unwrap();
    // 明文值（Node 也接受没有 `enc:v1:` 前缀的旧值）。
    let credentials = json!({
        "oauth:active_provider": "bigmodel",
        "oauth:bigmodel:access_token": "at-big",
        "oauth:bigmodel:user_info": r#"{"id":"key-1","username":"dev","displayName":"Dev"}"#,
        "zcodejwttoken": "zcode-jwt",
    });
    std::fs::write(v2.join("credentials.json"), credentials.to_string()).unwrap();
    let settings = json!({
        "providerFamilyDomain": "bigmodel",
        "providerFamilyDomainUpdatedAt": 1,
        "providerFamilyConnectionSelections": {"bigmodel": {"kind": "individual-coding-plan"}},
    });
    std::fs::write(v2.join("setting.json"), settings.to_string()).unwrap();
    let bundled = include_str!("../../platform/assets/zcode-builtin.json");
    let mut builtin: Value = serde_json::from_str(bundled).unwrap();
    for rule in builtin["config"]["providerConfigRules"]["providerRules"]
        .as_array_mut()
        .unwrap()
    {
        if rule["providerId"] == "account:bigmodel-team-coding-plan" {
            rule["config"]["api"]["baseUrl"] = json!(format!("{url}/api/anthropic"));
        }
    }
    let file = home.join("builtin.json");
    std::fs::write(&file, builtin.to_string()).unwrap();
    file
}

fn zcode(home: &Path, builtin: &Path, url: &str, extra: &[&str]) -> Output {
    let work = home.join("work");
    std::fs::create_dir_all(work.join(".git")).unwrap();
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(["-p", "hello"])
        .args(extra)
        .current_dir(&work)
        .env_clear()
        .keep_system()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", home)
        .env("ZCODE_CREDENTIAL_SECRET", "test-secret")
        .env("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE", builtin)
        .env("BIGMODEL_API_BASE_URL", url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

type Request = (String, String, Vec<(String, String)>);

fn header(request: &Request, name: &str) -> Option<String> {
    request
        .2
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

fn model_auth(seen: &Seen) -> Option<String> {
    let seen = seen.lock().unwrap();
    let model = seen.iter().find(|r| r.1.contains("/v1/messages"))?;
    header(model, "authorization")
}

#[test]
fn print_mode_uses_a_team_plan_without_the_app_connection() {
    let (url, seen) = server("EFFECTIVE");
    let dir = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(dir.path()).unwrap();
    let builtin = prepare(&home, &url);
    // 个人套餐不可用：默认是目录第一项，第一个团队项目（App 选的“个人”不影响）。
    let output = zcode(&home, &builtin, &url, &[]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout).trim(), "done");
    let mut projects: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.1 == "/api/biz/team/subscribe/product/querySubscribeDetail")
        .filter_map(|r| header(r, "bigmodel-project"))
        .collect();
    projects.sort();
    assert_eq!(projects, ["p-t", "p-u"]);
    assert_eq!(model_auth(&seen).as_deref(), Some("Bearer pat-p-t"));
}

#[test]
fn the_model_flag_names_the_team_project() {
    let (url, seen) = server("EFFECTIVE");
    let dir = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(dir.path()).unwrap();
    let builtin = prepare(&home, &url);
    let team = "account:bigmodel-team-coding-plan/GLM-5.3";
    let output = zcode(&home, &builtin, &url, &["--model", &format!("{team}[p-u]")]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(model_auth(&seen).as_deref(), Some("Bearer pat-p-u"));
    // 两个团队项目、没写哪一个：报错，不请求模型。显示名相同（同一组织），写法用项目 ID。
    seen.lock().unwrap().clear();
    let output = zcode(&home, &builtin, &url, &["--model", team]);
    assert!(!output.status.success());
    let all = format!("{}{}", text(&output.stdout), text(&output.stderr));
    let expected =
        format!("model {team} matches several team plans; pass one of: {team}[p-t], {team}[p-u]");
    assert!(all.contains(&expected), "{all}");
    assert_eq!(model_auth(&seen), None);
}

#[test]
fn an_expired_team_plan_explains_itself() {
    let (url, seen) = server("EXPIRED");
    let dir = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(dir.path()).unwrap();
    let builtin = prepare(&home, &url);
    let output = zcode(&home, &builtin, &url, &[]);
    assert!(!output.status.success());
    let all = format!("{}{}", text(&output.stdout), text(&output.stderr));
    // 管理页照 App 只看 `ZCODE_ENV`，不跟随指向本地替身的 `BIGMODEL_API_BASE_URL`（修复线 F58）。
    let notice = "Team plan expired. Contact your team administrator. Manage team plan: https://bigmodel.cn/coding-plan/team/plans";
    assert!(all.contains(notice), "{all}");
    assert_eq!(model_auth(&seen), None);
}
