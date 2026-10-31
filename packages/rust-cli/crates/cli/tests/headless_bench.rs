//! 跑分框架的命令行（docs/specs/rust-cli/headless.md §2，修复线 F39）：真实的 `zcode` 进程、本地的 SSE 模型夹具，
//! 按 Harbor（现行与 2026-02 的两种）、terminal-bench-1、Inspect-SWE、aws-samples SWE 与 stream-json 输入的写法调用，
//! 按框架取结果的方式核对输出与退出码。参数的切分与 stdin 的 prompt 是 F40（cli-args.md），两条线合入后补上 Harbor 的写法。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、跑子进程"
)]

mod system_env;
use system_env::KeepSystem as _;
mod sse;

use std::io::Write;
use std::process::{Output, Stdio};

use rust_cli_base::models::GLM_5_3;
use serde_json::Value;

/// 在临时 HOME 与工作区里跑 zcode；`stdin` 有值时写进去再关上。
fn zcode(fixture: &sse::Fixture, args: &[&str], stdin: Option<&str>) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(work.join("a.txt"), "alpha\n").unwrap();
    let mut child = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(args)
        .current_dir(&work)
        .env_clear()
        .keep_system()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", GLM_5_3)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(text) = stdin {
        let mut input = child.stdin.take().unwrap();
        input.write_all(text.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn lines(output: &Output) -> Vec<Value> {
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            let kind = event["type"].as_str().unwrap_or_default();
            match event["subtype"].as_str() {
                Some(subtype) => format!("{kind}/{subtype}"),
                None => kind.to_owned(),
            }
        })
        .collect()
}

fn read_then_answer() -> sse::Fixture {
    let input = r#"{"file_path":"a.txt"}"#;
    sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(0, "toolu_read", "Read", input)]),
        sse::text_reply("All done."),
    ])
}

/// terminal-bench-1：`zcode --verbose --output-format stream-json -p '<instr>' --allowedTools Bash Edit …`（tmux 里跑，
/// 不解析输出，只等命令结束；`--allowedTools` 是空格分隔的可变参数，含本工程没有的旧名）。
#[test]
fn terminal_bench_command_line_streams_events() {
    let fixture = read_then_answer();
    let output = zcode(
        &fixture,
        &[
            "--verbose",
            "--output-format",
            "stream-json",
            "-p",
            "Read a.txt and finish.",
            "--allowedTools",
            "Bash",
            "Edit",
            "Write",
            "Read",
            "Glob",
            "Grep",
            "LS",
            "WebFetch",
            "NotebookEdit",
            "NotebookRead",
            "TodoRead",
            "TodoWrite",
            "Agent",
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let events = lines(&output);
    assert_eq!(
        kinds(&events),
        [
            "system/init",
            "assistant",
            "user",
            "assistant",
            "result/success"
        ]
    );
    let init = &events[0];
    assert!(init["session_id"].as_str().unwrap().starts_with("sess_"));
    assert_eq!(init["model"], GLM_5_3);
    // key 来自 `ZCODE_API_KEY`（修复线 F42，config.md §2.5）。
    assert_eq!(init["apiKeySource"], "ZCODE_API_KEY");
    assert!(
        init["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool == "Task")
    );
    assert_eq!(events[1]["message"]["content"][0]["name"], "Read");
    assert_eq!(
        events[2]["message"]["content"][0]["tool_use_id"],
        "toolu_read"
    );
    let result = &events[4];
    assert_eq!(result["result"], "All done.");
    assert_eq!(result["num_turns"], 2);
    assert_eq!(result["is_error"], false);
    assert_eq!(result["session_id"], init["session_id"]);
}

/// aws-samples SWE（json）：`json.loads(整个 stdout)`，读 `modelUsage[*]` 的 token、`num_turns`、`total_cost_usd`、
/// `is_error`、`subtype`、`result`。
#[test]
fn aws_swe_json_command_line_prints_one_result_object() {
    let fixture = read_then_answer();
    let output = zcode(
        &fixture,
        &[
            "-p",
            "Fix the bug.",
            "--model",
            GLM_5_3,
            "--output-format",
            "json",
            "--permission-mode",
            "bypassPermissions",
            "--allowedTools",
            "Read,Glob,Grep,Write,Edit",
            "--max-turns",
            "250",
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    let result: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["is_error"], false);
    assert_eq!(result["result"], "All done.");
    assert_eq!(result["num_turns"], 2);
    assert_eq!(result["total_cost_usd"], 0);
    let usage = &result["modelUsage"][GLM_5_3];
    // 夹具的每次回复：输入 1、输出 1。
    assert_eq!(usage["inputTokens"], 2);
    assert_eq!(usage["outputTokens"], 2);
    assert_eq!(result["usage"]["input_tokens"], 2);
}

/// aws-samples SWE（stream-json，只在这时加 `--verbose`）：取最后一个 result。轮数用尽时 `error_max_turns`、退出码 1。
#[test]
fn aws_swe_stream_json_reports_max_turns_with_exit_1() {
    let fixture = read_then_answer();
    let output = zcode(
        &fixture,
        &[
            "-p",
            "Fix the bug.",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
            "--max-turns",
            "1",
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    // 结果已在 stdout 上，stderr 不另写。
    assert!(output.stderr.is_empty(), "{output:?}");
    let events = lines(&output);
    let result = events.last().unwrap();
    assert_eq!(result["subtype"], "error_max_turns");
    assert_eq!(result["is_error"], true);
    assert_eq!(result["num_turns"], 2);
    assert_eq!(result["errors"][0], "Reached maximum number of turns (1)");
}

/// stream-json 输入：两行 user 消息、两个回合；每个回合前一个 init、后一个 result（`result_index` 递增）；stdin 结束
/// 后退出码 0。
#[test]
fn stream_json_input_runs_one_turn_per_line() {
    let fixture = sse::serve(vec![sse::text_reply("First."), sse::text_reply("Second.")]);
    let stdin = concat!(
        r#"{"type":"user","message":{"role":"user","content":"Say hello."}}"#,
        "\n",
        r#"{"type":"control_request","request_id":"r1","request":{"subtype":"interrupt"}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Again."}]}}"#,
        "\n"
    );
    let output = zcode(
        &fixture,
        &[
            "-p",
            "--input-format",
            "stream-json",
            "--output-format=stream-json",
            "--verbose",
        ],
        Some(stdin),
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let events = lines(&output);
    assert_eq!(
        kinds(&events),
        [
            "system/init",
            "assistant",
            "result/success",
            "system/init",
            "assistant",
            "result/success"
        ]
    );
    assert_eq!(events[2]["result"], "First.");
    assert_eq!(events[2]["result_index"], 0);
    assert_eq!(events[5]["result"], "Second.");
    assert_eq!(events[5]["result_index"], 1);
    // 同一个会话：第二个请求里有第一轮的问答。
    let second = &fixture.main_requests()[1];
    let text = second["messages"].to_string();
    assert!(
        text.contains("Say hello.") && text.contains("Again."),
        "{text}"
    );
}

/// 校验与行错误：固定的文字写 stderr，退出码 1，stdout 为空。
#[test]
fn format_errors_use_the_fixed_words() {
    let fixture = sse::serve(Vec::new());
    let cases: [(&[&str], Option<&str>, &str); 3] = [
        (
            &["-p", "hi", "--output-format", "stream-json"],
            None,
            "Error: When using --print, --output-format=stream-json requires --verbose\n",
        ),
        (
            &["-p", "--input-format", "stream-json"],
            None,
            "Error: --input-format=stream-json requires output-format=stream-json.\n",
        ),
        (
            &[
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
            ],
            Some("not json\n"),
            "Error parsing streaming input line (type=unknown, 8 chars): SyntaxError\n",
        ),
    ];
    for (args, stdin, expected) in cases {
        let output = zcode(&fixture, args, stdin);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            expected,
            "{args:?}"
        );
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
    }
    assert!(fixture.main_requests().is_empty());
}

/// Harbor 现行（2026-07 起）：`printf "%s" "$instr" | zcode --verbose --output-format=stream-json
/// --permission-mode=bypassPermissions --print 2>&1 | tee …`：任务从管道进来（末尾没有换行），取第一个 result 的
/// `total_cost_usd`，退出码非 0 时看 `is_error` / `subtype` 分类。
#[test]
fn harbor_command_line_reads_the_task_from_stdin() {
    let fixture = read_then_answer();
    let output = zcode(
        &fixture,
        &[
            "--verbose",
            "--output-format=stream-json",
            "--permission-mode=bypassPermissions",
            "--print",
        ],
        Some("Read a.txt and finish."),
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let events = lines(&output);
    assert_eq!(events[0]["permissionMode"], "bypassPermissions");
    let result = events
        .iter()
        .find(|event| event["type"] == "result")
        .unwrap();
    assert_eq!(result["total_cost_usd"], 0);
    assert_eq!(result["is_error"], false);
    assert_eq!(result["subtype"], "success");
    let first = fixture.main_requests()[0]["messages"].to_string();
    assert!(first.contains("Read a.txt and finish."), "{first}");
}

/// Harbor 2026-02 到 2026-07 与 Inspect-SWE：`--print -- '<instr>'`（任务以 `-` 开头也行），stdin 是 /dev/null；
/// Inspect-SWE 另带 `--session-id <uuid> --dangerously-skip-permissions --model X --output-format stream-json --verbose`。
/// 工作线 F63 删掉了 `--dangerously-skip-permissions`（cli-args.md §1.2、§5.3）：原样的写法报未知选项、不发请求；
/// 适配器换成 `--permission-mode bypassPermissions` 后照常跑，本工程的 `yolo` 也一样，init 的 `permissionMode` 都是
/// `bypassPermissions`。
#[test]
fn harbor_and_inspect_swe_pass_the_task_after_double_dash() {
    let fixture = read_then_answer();
    let output = zcode(
        &fixture,
        &[
            "--verbose",
            "--output-format=stream-json",
            "--permission-mode=bypassPermissions",
            "--print",
            "--",
            "-Read a.txt and finish.",
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(lines(&output).last().unwrap()["result"], "All done.");
    let inspect = |fixture: &sse::Fixture, permission: &[&str]| {
        let mut line = vec!["--session-id", "0b6c5f6e-2f7a-4c1e-9d2b-3a4e5f607182"];
        line.extend_from_slice(permission);
        line.extend([
            "--model",
            GLM_5_3,
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--",
            "Fix the bug.",
        ]);
        zcode(fixture, &line, None)
    };
    let fixture = read_then_answer();
    let output = inspect(&fixture, &["--dangerously-skip-permissions"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: unknown option '--dangerously-skip-permissions'\n"
    );
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(fixture.main_requests().is_empty());
    for mode in ["bypassPermissions", "yolo"] {
        let fixture = read_then_answer();
        let output = inspect(&fixture, &["--permission-mode", mode]);
        assert_eq!(output.status.code(), Some(0), "{mode}: {output:?}");
        let events = lines(&output);
        assert_eq!(events[0]["permissionMode"], "bypassPermissions", "{mode}");
        assert_eq!(events.last().unwrap()["subtype"], "success", "{mode}");
    }
}
