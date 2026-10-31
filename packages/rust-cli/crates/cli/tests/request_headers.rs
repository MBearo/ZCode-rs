//! 工作线 F64（docs/specs/rust-cli/model.md §2.4）：真实的 `zcode -p` 发出的模型请求带 Node 的来源头与每请求头。
//! 显式覆盖连本地夹具；环境变量只给这几项（不继承父进程的环境）。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具"
)]

mod sse;

use rust_cli_base::hosts::{ZCODE_ORIGIN, ZCODE_ORIGIN_TEST};
use rust_cli_base::models::GLM_5_3;
use std::process::Stdio;

const SESSION: &str = "2d480878-38af-4bae-a54f-096e2fb0f4d7";

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.char_indices().all(|(index, char)| {
            if [8, 13, 18, 23].contains(&index) {
                char == '-'
            } else {
                char.is_ascii_hexdigit()
            }
        })
}

fn version() -> String {
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .arg("--version")
        .env_clear()
        .output()
        .unwrap();
    // `zcode <版本> (rust-cli)`。
    let text = String::from_utf8(output.stdout).unwrap();
    text.split_whitespace().nth(1).unwrap().to_owned()
}

/// 跑一次 `zcode -p hi`：第一个回复调一次 Read，第二个回复结束回合；返回两个主请求的 header。
fn run(vars: &[(&str, &str)]) -> Vec<sse::Headers> {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let read = sse::tool_block(0, "toolu_1", "Read", r#"{"file_path":"/nonexistent-f64"}"#);
    let fixture = sse::serve(vec![sse::tools_reply(&[read]), sse::text_reply("done")]);
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(["-p", "hi", "--session-id", SESSION])
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", GLM_5_3)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in vars {
        command.env(name, value);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.main_headers()
}

fn get<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    let found: Vec<_> = headers
        .iter()
        .filter(|(existing, _)| existing == name)
        .collect();
    assert!(found.len() <= 1, "{name}: {headers:?}");
    found.first().map(|(_, value)| value.as_str())
}

/// 来源头的值来自环境与系统；`user-agent` 的版本与 `zcode --version` 相同；一个回合的两个请求共用 trace、回合与会话，
/// `x-request-id` 每个请求一个。
#[test]
fn a_real_request_carries_the_node_headers() {
    let requests = run(&[
        ("LANG", "zh_CN.UTF-8"),
        ("TZ", "Asia/Tokyo"),
        ("ZCODE_ENV", "test"),
    ]);
    assert_eq!(requests.len(), 2);
    let version = version();
    let platform = format!(
        "{}-{}",
        rust_cli_base::js::node_platform(),
        rust_cli_base::js::node_arch()
    );
    let category = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    for headers in &requests {
        for (name, value) in [
            ("http-referer", ZCODE_ORIGIN_TEST),
            ("user-agent", format!("ZCode/{version}").as_str()),
            ("x-zcode-app-version", version.as_str()),
            ("x-title", "Z Code@cli"),
            ("x-release-channel", "test"),
            ("x-client-language", "zh-CN"),
            ("x-client-timezone", "Asia/Tokyo"),
            ("x-zcode-agent", "glm"),
            ("x-platform", platform.as_str()),
            ("x-os-category", category),
            ("x-zcode-session-type", "main"),
            ("x-zcode-query-source", "main_turn"),
            ("x-session-id", SESSION),
        ] {
            assert_eq!(get(headers, name), Some(value), "{name}: {headers:?}");
        }
        assert!(get(headers, "x-os-version").is_some_and(|release| !release.is_empty()));
        for name in ["x-request-id", "x-zcode-trace-id", "x-query-id"] {
            assert!(
                get(headers, name).is_some_and(is_uuid),
                "{name}: {headers:?}"
            );
        }
    }
    let [first, second] = requests.as_slice() else {
        unreachable!()
    };
    for name in ["x-zcode-trace-id", "x-query-id"] {
        assert_eq!(get(first, name), get(second, name), "{name}");
    }
    assert_ne!(get(first, "x-request-id"), get(second, "x-request-id"));
}

/// `ZCODE_APP_VERSION` 设了就用它（Node 的 `??`：空值也算设了，版本写 `unknown`、不发版本头）；没设 `LANG` 时是 `en-US`。
#[test]
fn the_app_version_variable_comes_first() {
    let requests = run(&[("ZCODE_APP_VERSION", "9.9.9"), ("TZ", "UTC")]);
    let headers = &requests[0];
    assert_eq!(get(headers, "user-agent"), Some("ZCode/9.9.9"));
    assert_eq!(get(headers, "x-zcode-app-version"), Some("9.9.9"));
    assert_eq!(get(headers, "x-client-language"), Some("en-US"));
    assert_eq!(get(headers, "x-client-timezone"), Some("UTC"));
    assert_eq!(get(headers, "http-referer"), Some(ZCODE_ORIGIN));
    assert_eq!(get(headers, "x-release-channel"), Some("production"));
    let requests = run(&[("ZCODE_APP_VERSION", "")]);
    assert_eq!(get(&requests[0], "user-agent"), Some("ZCode/unknown"));
    assert_eq!(get(&requests[0], "x-zcode-app-version"), None);
}
