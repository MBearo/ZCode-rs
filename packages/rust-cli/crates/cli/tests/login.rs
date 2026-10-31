//! `zcode login` / `zcode logout` 的真实进程（docs/specs/rust-cli/platform.md §5.6）：提示与输出文本、写出的
//! 文件。账号服务是本地假服务，不连任何真实服务；数据目录、home 与密钥都是临时的。
#![allow(
    clippy::disallowed_methods,
    reason = "测试搭本地 HTTP 夹具、读临时目录里的文件"
)]
#![allow(
    clippy::unwrap_used,
    reason = "集成测试：辅助函数不在 #[test] 里，clippy 的测试豁免覆盖不到"
)]

mod system_env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Output, Stdio};
use system_env::KeepSystem as _;

use rust_cli_base::models::GLM_5_3_ACCOUNT;
use serde_json::{Value, json};

/// 在临时 home 里运行 `zcode <args>`，标准输入给 `stdin`。
fn zcode(home: &Path, args: &[&str], stdin: &str, server: Option<&str>) -> Output {
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(args)
        .env_clear()
        .keep_system()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ZCODE_DATA_BASE_DIR", home)
        .env("ZCODE_CREDENTIAL_SECRET", "test-secret")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(url) = server {
        command
            .env("ZCODE_BASE_URL", url)
            .env("BIGMODEL_API_BASE_URL", url);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn api_key_login_and_logout() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".zcode/v2");
    let output = zcode(
        home.path(),
        &["login", "zai-coding-plan-api-key"],
        "sk-test\n",
        None,
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        format!(
            "Configured Z.AI Coding Plan.\nModel: account:zai-individual-coding-plan/{GLM_5_3_ACCOUNT}\nModel selection: {}\n",
            config.join("provider_config.json").display()
        )
    );
    let credentials: Value =
        serde_json::from_str(&std::fs::read_to_string(config.join("credentials.json")).unwrap())
            .unwrap();
    let identity = "account-provider:account:zai-individual-coding-plan:identity";
    assert!(
        credentials[identity]
            .as_str()
            .unwrap()
            .starts_with("enc:v1:")
    );
    assert!(
        !std::fs::read_to_string(config.join("credentials.json"))
            .unwrap()
            .contains("sk-test")
    );

    let output = zcode(home.path(), &["logout"], "", None);
    assert!(output.status.success());
    assert_eq!(
        text(&output.stdout),
        format!(
            "Logged out from Coding Plan accounts. Credentials: {}\n",
            config.join("credentials.json").display()
        )
    );
    let credentials = std::fs::read_to_string(config.join("credentials.json")).unwrap();
    assert_eq!(credentials, "{}\n");
}

#[test]
fn usage_errors_follow_node() {
    let home = tempfile::tempdir().unwrap();
    let output = zcode(home.path(), &["login", "other"], "", None);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).starts_with("Error: Usage: zcode login [zai|bigmodel] [--no-browser]")
    );
    let output = zcode(
        home.path(),
        &["login", "bigmodel-coding-plan-api-key"],
        "\n",
        None,
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stderr), "Error: API key must not be empty.\n");
}

/// 按路径回复的假服务：CLI OAuth（init 与 ready）与 BigModel 的项目接口。
fn serve(listener: TcpListener) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let path = line.split(' ').nth(1).unwrap_or_default().to_owned();
        let mut length = 0;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            if header.trim().is_empty() {
                break;
            }
            if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let reply = match path.as_str() {
            "/api/v1/oauth/cli/init" => json!({"code": 0, "data": {
                "flow_id": "f1",
                "authorize_url": "https://open.example.com/authorize?flow=f1",
                "expires_at": now + 600,
                "poll_interval_sec": 1,
            }}),
            "/api/v1/oauth/cli/poll/f1" => json!({"code": 0, "data": {
                "status": "ready",
                "token": "zcode-jwt",
                "user": {"user_id": "u-1", "email": "dev@example.com"},
                "bigmodel": {"access_token": "at-big"},
            }}),
            // 登录不发 `/api/biz/` 请求（F50 追加，照 App：不再换项目令牌验证账号）：账号接口都回 404。
            _ => json!({"code": 404}),
        }
        .to_string();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
            reply.len()
        );
    }
}

#[test]
fn oauth_login_without_a_browser() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || serve(listener));
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".zcode/v2");
    let output = zcode(
        home.path(),
        &["login", "bigmodel", "--no-browser"],
        "",
        Some(&url),
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        format!(
            "Open this URL to sign in:\nhttps://open.example.com/authorize?flow=f1\n\
             Login successful as dev@example.com.\n\
             Model: account:bigmodel-individual-coding-plan/{GLM_5_3_ACCOUNT}\n\
             Credentials: {}\nModel selection: {}\n",
            config.join("credentials.json").display(),
            config.join("provider_config.json").display()
        )
    );
}

/// `--json`（Node `login-command.ts` 的 `options.json`，工作线 F47）：提示写到 stderr，stdout 只有一段
/// `JSON.stringify(…, null, 2)` 加换行。
#[test]
fn json_output_follows_node() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || serve(listener));
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".zcode/v2");
    let output = zcode(
        home.path(),
        &["login", "bigmodel", "--no-browser", "--json"],
        "",
        Some(&url),
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stderr),
        "Open this URL to sign in:\nhttps://open.example.com/authorize?flow=f1\n"
    );
    let expected = json!({
        "status": "ready",
        "provider": "bigmodel",
        "user": {"user_id": "u-1", "email": "dev@example.com"},
        "model": format!("account:bigmodel-individual-coding-plan/{GLM_5_3_ACCOUNT}"),
        "credentialsPath": config.join("credentials.json").display().to_string(),
        "configPath": config.join("provider_config.json").display().to_string(),
        "browserOpened": false,
    });
    assert_eq!(
        text(&output.stdout),
        format!("{}\n", serde_json::to_string_pretty(&expected).unwrap())
    );

    let output = zcode(home.path(), &["logout", "--json"], "", None);
    assert!(output.status.success(), "{}", text(&output.stderr));
    // Node 写死 provider 为 zai。
    let expected = json!({
        "status": "logged_out",
        "provider": "zai",
        "credentialsPath": config.join("credentials.json").display().to_string(),
    });
    assert_eq!(
        text(&output.stdout),
        format!("{}\n", serde_json::to_string_pretty(&expected).unwrap())
    );
    // 手工 key 是本工程才有的命令，没有 JSON 形状：用法错误。
    let output = zcode(
        home.path(),
        &["login", "zai-coding-plan-api-key", "--json"],
        "sk\n",
        None,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("Error: Usage: zcode login"));
}
