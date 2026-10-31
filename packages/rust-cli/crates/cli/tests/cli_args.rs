//! 命令行参数的真实进程（docs/specs/rust-cli/cli-args.md §11）：临时 home 与工作区、本地假的 Anthropic 端点（记下
//! 请求体）。覆盖帮助、未知参数的报错、管道里的 prompt（Harbor 的写法）、system prompt 参数、`--session-id`、
//! `--add-dir` 与 `--settings` 的报错、`--permission-mode yolo` 与删掉的 `--dangerously-skip-permissions`（工作线 F63）。
//! root 下的 bypass 限制在单测里（`flags_tests.rs`）。
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

use rust_cli_base::models::GLM_5_3;
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

    /// 主请求（带 tools 的那些）。
    fn requests(&self) -> Vec<Value> {
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|body| body.get("tools").is_some())
            .cloned()
            .collect()
    }
}

/// 临时 home 与工作区。
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

    /// 跑一次 `zcode`；`stdin` 为 None 时标准输入是 /dev/null。
    fn run(&self, args: &[&str], server: Option<&Server>, stdin: Option<&str>) -> Output {
        self.run_with(args, server, stdin, &[])
    }

    /// 同 [`Sandbox::run`]，另设几个环境变量。
    fn run_with(
        &self,
        args: &[&str],
        server: Option<&Server>,
        stdin: Option<&str>,
        vars: &[(&str, &str)],
    ) -> Output {
        let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
        command
            .args(args)
            .current_dir(&self.work)
            .env_clear()
            .keep_system()
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("ZCODE_DATA_BASE_DIR", &self.home)
            .env("ZCODE_API_KEY", "test-key")
            .env("ZCODE_MODEL", GLM_5_3)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(server) = server {
            command.env("ZCODE_MODEL_BASE_URL", &server.url);
        }
        command.envs(vars.iter().copied());
        let mut child = command.spawn().unwrap();
        if let Some(text) = stdin {
            let mut pipe = child.stdin.take().unwrap();
            pipe.write_all(text.as_bytes()).unwrap();
        }
        child.wait_with_output().unwrap()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// 请求里全部文字（system 与 messages），用来查某段文字有没有发出去。
fn all_text(body: &Value) -> String {
    let mut out = String::new();
    collect(body, &mut out);
    out
}

fn collect(value: &Value, out: &mut String) {
    match value {
        Value::String(text) => {
            out.push_str(text);
            out.push('\n');
        }
        Value::Array(items) => items.iter().for_each(|item| collect(item, out)),
        Value::Object(map) => map
            .iter()
            .filter(|(key, _)| key.as_str() != "tools")
            .for_each(|(_, item)| collect(item, out)),
        _ => {}
    }
}

/// `--help` 写 stdout、退出码 0；在任何位置都是帮助（cli-args.md §3）。
#[test]
fn help_prints_the_page_and_exits_zero() {
    let sandbox = Sandbox::new();
    for args in [
        &["--help"][..],
        &["-h"][..],
        &["-p", "hi", "--nope", "-h"][..],
    ] {
        let output = sandbox.run(args, None, None);
        assert!(output.status.success(), "{args:?}: {output:?}");
        let page = text(&output.stdout);
        assert!(
            page.starts_with("Usage: zcode [options] [command] [prompt]\n"),
            "{page}"
        );
        assert!(page.contains(
            "\n  --append-system-prompt <prompt>       Append a system prompt to the default\n"
        ));
        assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
    }
}

/// 未知选项与缺值：commander 的原文、退出码 1（修复前是自拟的 `zcode: unsupported arguments: …`、退出码 2）。
#[test]
fn unknown_options_exit_one_with_the_commander_text() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["-p", "hi", "--nope"], None, None);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(text(&output.stderr), "error: unknown option '--nope'\n");
    assert!(output.stdout.is_empty());
    let output = sandbox.run(&["--model"], None, None);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: option '--model <model>' argument missing\n"
    );
}

/// Harbor 的写法：任务从管道进来，`--permission-mode=bypassPermissions --print`，没有 prompt 参数（cli-args.md §1.2）。
#[test]
fn the_prompt_comes_from_stdin_when_piped() {
    let sandbox = Sandbox::new();
    let server = Server::start();
    let output = sandbox.run(
        &["--permission-mode=bypassPermissions", "--print"],
        Some(&server),
        Some("fix the failing test"),
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(text(&output.stdout), "done\n");
    let requests = server.requests();
    assert!(all_text(&requests[0]).contains("fix the failing test"));
    // prompt 参数在前、管道的文字在后，换行连接。
    let server = Server::start();
    let output = sandbox.run(&["-p", "first"], Some(&server), Some("second"));
    assert!(output.status.success(), "{output:?}");
    assert!(all_text(&server.requests()[0]).contains("first\nsecond"));
}

#[test]
fn no_prompt_and_no_piped_input_is_an_error() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["-p"], None, Some(""));
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        text(&output.stderr),
        "Error: Input must be provided either through stdin or as a prompt argument when using --print\n"
    );
}

/// `--system-prompt-file` 与 `--append-system-prompt`（cli-args.md §4）：文件内容去掉结尾换行、空行、追加的文字。
#[test]
fn system_prompt_flags_reach_the_request() {
    let sandbox = Sandbox::new();
    let server = Server::start();
    std::fs::write(
        sandbox.work.join("system.md"),
        "You are a test assistant.\n",
    )
    .unwrap();
    let output = sandbox.run(
        &[
            "-p",
            "hi",
            "--system-prompt-file",
            "system.md",
            "--system-prompt",
            "Be brief.",
            "--append-system-prompt",
            "End with DONE.",
        ],
        Some(&server),
        None,
    );
    assert!(output.status.success(), "{output:?}");
    let system = &server.requests()[0]["system"];
    assert_eq!(system.as_array().unwrap().len(), 2, "{system}");
    assert_eq!(
        system[1]["text"],
        "You are a test assistant.\n\nBe brief.\n\nEnd with DONE."
    );
    let missing = sandbox.run(
        &["-p", "hi", "--append-system-prompt-file", "nope.md"],
        None,
        None,
    );
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    assert_eq!(
        text(&missing.stderr),
        format!(
            "Error: Append system prompt file not found: {}\n",
            sandbox.work.join("nope.md").display()
        )
    );
}

/// `--session-id`（cli-args.md §7）：会话 ID 是 `sess_<uuid>`；再用同一个时报错；`-r <uuid>` 续写它。
#[test]
fn a_session_id_names_the_session_once() {
    let sandbox = Sandbox::new();
    let server = Server::start();
    let uuid = "00000000-0000-4000-8000-0000000000f4";
    let first = sandbox.run(&["-p", "hi", "--session-id", uuid], Some(&server), None);
    assert!(first.status.success(), "{first:?}");
    let again = sandbox.run(&["-p", "hi", "--session-id", uuid], Some(&server), None);
    assert_eq!(again.status.code(), Some(1), "{again:?}");
    assert_eq!(
        text(&again.stderr),
        format!("Error: Session ID {uuid} is already in use.\n")
    );
    let resumed = sandbox.run(&["-p", "more", "-r", uuid], Some(&server), None);
    assert!(resumed.status.success(), "{resumed:?}");
    // 续写的请求带着第一次的输入。
    let last = server.requests().pop().unwrap();
    let text = all_text(&last);
    assert!(text.contains("hi") && text.contains("more"), "{text}");
    let invalid = sandbox.run(&["-p", "hi", "--session-id", "abc"], None, None);
    assert_eq!(
        String::from_utf8_lossy(&invalid.stderr),
        "Error: Invalid session ID. Must be a valid UUID.\n"
    );
}

/// `--add-dir`（cli-args.md §6）：环境段列出附加目录；不存在的静默丢掉，不是目录的给警告。
#[test]
fn added_directories_reach_the_environment_section() {
    let sandbox = Sandbox::new();
    let server = Server::start();
    let extra = sandbox.work.parent().unwrap().join("extra");
    std::fs::create_dir_all(&extra).unwrap();
    std::fs::write(sandbox.work.join("file.txt"), "x").unwrap();
    let output = sandbox.run(
        &[
            "-p",
            "hi",
            "--add-dir",
            "../extra",
            "/missing/dir",
            "file.txt",
        ],
        Some(&server),
        None,
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        text(&output.stderr),
        format!(
            "file.txt is not a directory. Did you mean to add the parent directory {}?\n",
            sandbox.work.display()
        )
    );
    let environment = all_text(&server.requests()[0]);
    assert!(
        environment.contains(&format!(
            " - Additional working directories:\n  - {}\n",
            extra.display()
        )),
        "{environment}"
    );
    assert!(!environment.contains("/missing/dir"));
}

/// `--settings`（cli-args.md §8）：`{…}` 不是 JSON、文件不存在时报错退出。
#[test]
fn bad_settings_stop_with_the_error_text() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["-p", "hi", "--settings", "{not json}"], None, None);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        text(&output.stderr),
        "Error: Invalid JSON provided to --settings\n"
    );
    let output = sandbox.run(&["-p", "hi", "--settings", "s.json"], None, None);
    assert_eq!(
        text(&output.stderr),
        format!(
            "Error: Settings file not found: {}\n",
            sandbox.work.join("s.json").display()
        )
    );
}

/// 修复线 F43（shell-permissions.md §4.9）：启动时是 bypass 类别（`--permission-mode bypassPermissions` / `yolo`，或配置里
/// Node 的 `guarded`）时，第一个请求宣布可以用 Bash 读写文件，Bash 的描述没有“不要用 cat”一行；默认模式两处都照旧。
/// 工作线 F63 删掉了 `--dangerously-skip-permissions`，这里原来用它。
#[test]
fn a_bypass_launch_steers_toward_bash_and_trims_its_description() {
    const STEER: &str = "While bypass permissions mode is active:\n\nYou can do much of your work through the Bash tool when it is the simpler route:";
    const AVOID: &str = "- IMPORTANT: Avoid using this tool to run `cat`";
    let bash_description = |body: &Value| {
        body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "Bash")
            .map(|tool| tool["description"].as_str().unwrap().to_owned())
            .unwrap()
    };
    let guarded = r#"{"permission":{"mode":"guarded"}}"#;
    let runs: [(&[&str], bool); 4] = [
        (
            &["-p", "hi", "--permission-mode", "bypassPermissions"],
            true,
        ),
        (&["-p", "hi", "--permission-mode", "yolo"], true),
        (&["-p", "hi", "--settings", guarded], true),
        (&["-p", "hi", "--permission-mode", "default"], false),
    ];
    for (args, bypass) in runs {
        let sandbox = Sandbox::new();
        let server = Server::start();
        let output = sandbox.run(args, Some(&server), None);
        assert!(output.status.success(), "{args:?}: {output:?}");
        let request = &server.requests()[0];
        assert_eq!(all_text(request).contains(STEER), bypass, "{args:?}");
        assert_eq!(
            !bash_description(request).contains(AVOID),
            bypass,
            "{args:?}"
        );
    }
}

/// 工作线 F63（cli-args.md §5.3）：`--permission-mode yolo` 发出的请求与 `bypassPermissions` 相同（只去掉带会话 ID 的
/// `metadata`）；`--dangerously-skip-permissions` 按 commander 的未知选项报错，退出码 1，不发请求。
#[test]
fn yolo_launches_in_bypass_and_the_old_flag_is_an_unknown_option() {
    let sandbox = Sandbox::new();
    let first_request = |mode: &str| {
        let server = Server::start();
        let output = sandbox.run(
            &["-p", "hi", "--permission-mode", mode],
            Some(&server),
            None,
        );
        assert!(output.status.success(), "{mode}: {output:?}");
        let mut body = server.requests().remove(0);
        body.as_object_mut().unwrap().remove("metadata");
        body
    };
    assert_eq!(first_request("yolo"), first_request("bypassPermissions"));
    let server = Server::start();
    let output = sandbox.run(
        &["-p", "hi", "--dangerously-skip-permissions"],
        Some(&server),
        None,
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        text(&output.stderr),
        "error: unknown option '--dangerously-skip-permissions'\n"
    );
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(server.bodies.lock().unwrap().is_empty());
}

/// 工作线 F69（cli-args.md §1.4）：`--effort` 是新会话的档位，主请求与对话中间 system 消息的 `output_config.effort`
/// 都是它；不认识的值写一行警告、按缺省档位，退出码 0。端点按官方 API 处理（只给测试用的声明），模型有卡片的形状
/// （GLM 按 opus，effort 按卡片写），否则不发 effort。
#[test]
fn effort_sets_the_request_effort() {
    let sandbox = Sandbox::new();
    let first_party = [
        ("_ZCODE_ASSUME_FIRST_PARTY_BASE_URL", "1"),
        ("ZCODE_MODEL", GLM_5_3),
    ];
    let efforts = |args: &[&str]| {
        let server = Server::start();
        let output = sandbox.run_with(args, Some(&server), None, &first_party);
        assert!(output.status.success(), "{args:?}: {output:?}");
        let body = server.requests().remove(0);
        let turn = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|message| message.get("output_config"))
            .cloned();
        (
            body["output_config"]["effort"].clone(),
            turn,
            text(&output.stderr),
        )
    };
    let (top, turn, stderr) = efforts(&["-p", "hi", "--effort", "high"]);
    assert_eq!(top, "high");
    assert_eq!(turn, Some(serde_json::json!({"effort": "high"})));
    assert_eq!(stderr, "");
    let (top, _, stderr) = efforts(&["-p", "hi", "--effort=Turbo"]);
    assert_eq!(top, efforts(&["-p", "hi"]).0);
    assert_eq!(
        stderr,
        "Warning: Unknown --effort value 'Turbo' \u{2014} ignoring it and using the default effort. Valid values: low, medium, high, xhigh, max.\n"
    );
}
