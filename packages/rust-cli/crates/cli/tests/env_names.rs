//! 环境变量的名字与跑分用的三个开关（修复线 F42，docs/specs/rust-cli/config.md §2.5、cli-args.md §9）的真实进程：
//! 临时 home 与工作区、本地假的 Anthropic 端点（记下请求体）。覆盖显式覆盖只认 `ZCODE_*`、Anthropic SDK 的变量不生效、
//! 有 key 没有模型时取个人 Provider 配置的 `defaultModelSelection`、`ZCODE_MAX_OUTPUT_TOKENS`、`MAX_THINKING_TOKENS`
//! 与 `BASH_*_TIMEOUT_MS`。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、检查写出的文件"
)]

mod system_env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use system_env::KeepSystem as _;

use rust_cli_base::models::{DEEPSEEK_V4_PRO, GLM_5_3, OPUS_5_5};
use serde_json::Value;

const REPLY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",",
    "\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

/// 本地假端点：每个连接回同一段 SSE 文本回复，记下 `/v1/messages` 的请求体。
struct Server {
    url: String,
    bodies: Arc<Mutex<Vec<Value>>>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = bodies.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut length, mut messages) = (0, false);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    messages |= line.starts_with("POST /v1/messages");
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                if messages && let Ok(value) = serde_json::from_slice::<Value>(&body) {
                    seen.lock().unwrap().push(value);
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    REPLY.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(REPLY.as_bytes());
            }
        });
        Self {
            url: format!("http://{address}"),
            bodies,
        }
    }

    /// 主请求（带 tools 的那些；旁路请求不算）。
    fn requests(&self) -> Vec<Value> {
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|body| body.get("tools").is_some())
            .cloned()
            .collect()
    }

    fn all(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
    }
}

/// 临时 home 与工作区；`ZCODE_DATA_BASE_DIR` 指向 home（个人 Provider 配置在 `<home>/.zcode/v2`）。
struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let (home, work) = (base.join("home"), base.join("work"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        Self {
            _dir: dir,
            home,
            work,
        }
    }

    /// 个人 Provider 配置里只有新会话的缺省选择（Node `saveConfiguredDefault` 写的形状）。
    fn default_model_selection(&self, model: &str) {
        let dir = self.home.join(".zcode/v2");
        std::fs::create_dir_all(&dir).unwrap();
        let config = serde_json::json!({
            "schemaVersion": 1,
            "config": {
                "defaultModelSelection": {
                    "providerId": "zai",
                    "modelId": model,
                    "options": {"reasoningLevel": "high"}
                }
            }
        });
        std::fs::write(dir.join("provider_config.json"), config.to_string()).unwrap();
    }

    /// `zcode -p hi`，只带 `vars` 与最少的系统变量（不继承父进程的环境）。
    fn run(&self, vars: &[(&str, &str)], extra: &[&str]) -> Output {
        let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
        command
            .args(["-p", "hi"])
            .args(extra)
            .current_dir(&self.work)
            .env_clear()
            .keep_system()
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("ZCODE_DATA_BASE_DIR", &self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in vars {
            command.env(name, value);
        }
        command.output().unwrap()
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// 显式覆盖的三个变量是 `ZCODE_API_KEY` / `ZCODE_MODEL_BASE_URL` / `ZCODE_MODEL`（config.md §2.5）。
#[test]
fn the_zcode_names_configure_the_explicit_endpoint() {
    let sandbox = Sandbox::new();
    let server = Server::start();
    let output = sandbox.run(
        &[
            ("ZCODE_API_KEY", "test-key"),
            ("ZCODE_MODEL_BASE_URL", &server.url),
            ("ZCODE_MODEL", GLM_5_3),
        ],
        &[],
    );
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "done");
    let requests = server.requests();
    assert!(!requests.is_empty());
    assert_eq!(requests[0]["model"], GLM_5_3);
}

/// Anthropic SDK 的变量（`ANTHROPIC_*`）不读：给全了也不发请求，提示里是本工程的名字。
#[test]
fn anthropic_sdk_variables_are_not_read() {
    let sandbox = Sandbox::new();
    let server = Server::start();
    let output = sandbox.run(
        &[
            ("ANTHROPIC_API_KEY", "test-key"),
            ("ANTHROPIC_BASE_URL", &server.url),
            ("ANTHROPIC_MODEL", GLM_5_3),
        ],
        &[],
    );
    assert!(!output.status.success(), "{}", text(&output));
    assert!(server.all().is_empty(), "{:?}", server.all());
    let shown = text(&output);
    assert!(shown.contains("set ZCODE_API_KEY"), "{shown}");
    assert!(!shown.contains("ANTHROPIC"), "{shown}");
}

/// 有 key 没有模型：用个人 Provider 配置的 `defaultModelSelection`（用户决定 2026-10-20）；也没有时报错提示
/// `--model`，不发请求。
#[test]
fn an_api_key_without_a_model_uses_the_default_model_selection() {
    let sandbox = Sandbox::new();
    let server = Server::start();
    let vars = [
        ("ZCODE_API_KEY", "test-key"),
        ("ZCODE_MODEL_BASE_URL", server.url.as_str()),
    ];
    let output = sandbox.run(&vars, &[]);
    assert!(!output.status.success(), "{}", text(&output));
    assert!(server.all().is_empty());
    let shown = text(&output);
    assert!(
        shown.contains("ZCODE_API_KEY is set: pass --model or set ZCODE_MODEL"),
        "{shown}"
    );
    sandbox.default_model_selection(GLM_5_3);
    let output = sandbox.run(&vars, &[]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(server.requests()[0]["model"], GLM_5_3);
    // `ZCODE_MODEL` 与 `--model` 优先。
    let mut with_model = vars.to_vec();
    with_model.push(("ZCODE_MODEL", "from-env"));
    let output = sandbox.run(&with_model, &[]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(server.requests().last().unwrap()["model"], "from-env");
    let output = sandbox.run(&with_model, &["--model", "from-flag"]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(server.requests().last().unwrap()["model"], "from-flag");
}

/// 主请求的 `max_tokens`（cli-args.md §9.1）：设的值不超过上限（有卡片的模型按卡片 128000；按 opus 处理的 GLM 按目录，
/// 内置规则给 glm-5.3 的是 128000；没有卡片 128000、缺省 32000）；无效值用缺省。卡片的上限只给有卡片的模型，所以这里
/// 用 Anthropic 的有卡片的模型；“没有卡片”用 DeepSeek。
#[test]
fn zcode_max_output_tokens_sets_the_output_limit() {
    let sandbox = Sandbox::new();
    let server = Server::start();
    let max_tokens = |model: &str, var: (&str, &str)| {
        let output = sandbox.run(
            &[
                ("ZCODE_API_KEY", "test-key"),
                ("ZCODE_MODEL_BASE_URL", &server.url),
                ("ZCODE_MODEL", model),
                var,
            ],
            &[],
        );
        assert!(output.status.success(), "{}", text(&output));
        server.requests().last().unwrap()["max_tokens"].clone()
    };
    assert_eq!(
        max_tokens(OPUS_5_5, ("ZCODE_MAX_OUTPUT_TOKENS", "4096")),
        4096
    );
    assert_eq!(
        max_tokens(OPUS_5_5, ("ZCODE_MAX_OUTPUT_TOKENS", "1e6")),
        128_000
    );
    let uncarded = DEEPSEEK_V4_PRO;
    assert_eq!(
        max_tokens(uncarded, ("ZCODE_MAX_OUTPUT_TOKENS", "64,000")),
        64_000
    );
    assert_eq!(
        max_tokens(uncarded, ("ZCODE_MAX_OUTPUT_TOKENS", "999999")),
        128_000
    );
    assert_eq!(
        max_tokens(uncarded, ("ZCODE_MAX_OUTPUT_TOKENS", "lots")),
        32_000
    );
    // GLM 的缺省是目录的输出上限（工作线 F121），不是没有卡片时的 32000。
    assert_eq!(
        max_tokens(GLM_5_3, ("ZCODE_MAX_OUTPUT_TOKENS", "lots")),
        128_000
    );
}

/// `MAX_THINKING_TOKENS`（cli-args.md §9.2）：0 时有卡片形状的模型（GLM 按 opus）不发 thinking 与 clear_thinking；大于 0
/// 时照旧。
/// 本地端点按官方 API 处理（只给测试的声明，model.md §6）：其他 Anthropic 兼容的方案本来就不发这些字段。
#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "测试声明只在 debug 构建生效（工作线 F52b）"
)]
fn max_thinking_tokens_zero_turns_thinking_off() {
    let sandbox = Sandbox::new();
    let server = Server::start();
    let body = |value: &str| {
        let output = sandbox.run(
            &[
                ("ZCODE_API_KEY", "test-key"),
                ("ZCODE_MODEL_BASE_URL", &server.url),
                ("ZCODE_MODEL", GLM_5_3),
                ("_ZCODE_ASSUME_FIRST_PARTY_BASE_URL", "1"),
                ("MAX_THINKING_TOKENS", value),
            ],
            &[],
        );
        assert!(output.status.success(), "{}", text(&output));
        server.requests().last().unwrap().clone()
    };
    let off = body("0");
    assert!(off.get("thinking").is_none(), "{off}");
    assert!(off.get("context_management").is_none(), "{off}");
    assert_eq!(off["output_config"]["effort"], "medium");
    let on = body("8000");
    assert_eq!(on["thinking"]["type"], "adaptive");
    assert!(on.get("context_management").is_some(), "{on}");
}

/// `BASH_DEFAULT_TIMEOUT_MS` / `BASH_MAX_TIMEOUT_MS`（cli-args.md §9.3）：Bash 的描述与 schema 里的数字跟着变。
#[test]
fn bash_timeouts_change_the_tool_text() {
    let sandbox = Sandbox::new();
    let server = Server::start();
    let output = sandbox.run(
        &[
            ("ZCODE_API_KEY", "test-key"),
            ("ZCODE_MODEL_BASE_URL", &server.url),
            ("ZCODE_MODEL", GLM_5_3),
            ("BASH_DEFAULT_TIMEOUT_MS", "900000"),
            ("BASH_MAX_TIMEOUT_MS", "8000000"),
        ],
        &[],
    );
    assert!(output.status.success(), "{}", text(&output));
    let requests = server.requests();
    let bash = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "Bash")
        .unwrap()
        .clone();
    let description = bash["description"].as_str().unwrap();
    assert!(
        description.contains(
            "- `timeout` is in milliseconds: default 900000, max 8000000 for a foreground command."
        ),
        "{description}"
    );
    assert!(
        description.contains("(default 900000, max 8000000); at that limit"),
        "{description}"
    );
    let schema = &bash["input_schema"]["properties"];
    assert_eq!(
        schema["timeout"]["description"],
        "Optional timeout in milliseconds (max 8000000 for a foreground command)"
    );
    let background = schema["run_in_background"]["description"].as_str().unwrap();
    assert!(
        background.contains("(default 900000 ms, max 8000000 ms)"),
        "{background}"
    );
}

/// 记忆段的开关（`ZCODE_DISABLE_AUTO_MEMORY`）：为真时关掉记忆段，为假时不关。
#[test]
fn the_auto_memory_switch_takes_effect() {
    let sandbox = Sandbox::new();
    let server = Server::start();
    let system = |var: (&str, &str)| {
        let output = sandbox.run(
            &[
                ("ZCODE_API_KEY", "test-key"),
                ("ZCODE_MODEL_BASE_URL", &server.url),
                ("ZCODE_MODEL", GLM_5_3),
                var,
            ],
            &[],
        );
        assert!(output.status.success(), "{}", text(&output));
        server.requests().last().unwrap()["system"].to_string()
    };
    const MEMORY: &str = "You have a persistent file-based memory at";
    assert!(!system(("ZCODE_DISABLE_AUTO_MEMORY", "1")).contains(MEMORY));
    assert!(system(("ZCODE_DISABLE_AUTO_MEMORY", "0")).contains(MEMORY));
}
