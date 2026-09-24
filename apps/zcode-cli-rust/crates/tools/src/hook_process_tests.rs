use super::*;
use serde_json::json;
use std::time::Instant;

fn request(program: Program, timeout_ms: u64, limit: usize) -> HookProcess {
    HookProcess {
        program,
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        env: vec![("ZCODE_SESSION_ID".into(), "s-1".into())],
        input: json!({"hookEventName":"UserPromptSubmit","prompt":"hi","sessionId":"s-1"}),
        timeout: Duration::from_millis(timeout_ms),
        max_output_bytes: limit,
    }
}

fn shell(command: &str) -> Program {
    Program::Command {
        command: command.into(),
        shell: Shell::Unset,
        background: false,
    }
}

fn base_env() -> Vec<(String, String)> {
    vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())]
}

#[tokio::test]
async fn runs_with_stdin_env_and_transcript() {
    let script = r#"read line; printf '%s|%s|' "$line" "$ZCODE_SESSION_ID"; p=$(printf '%s' "$line" | sed 's/.*"transcript_path":"\([^"]*\)".*/\1/'); cat "$p"; echo "$p" >&2; exit 3"#;
    let result = run(
        request(shell(script), 5000, 4096),
        &base_env(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, "failed");
    assert_eq!(result.exit_code, Some(3));
    let (line, rest) = result.stdout.split_once('|').unwrap();
    let stdin: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(stdin["hook_event_name"], "UserPromptSubmit");
    assert!(rest.starts_with("s-1|{\"message\":{\"content\":[{\"text\":\"hi\""));
    // 运行结束后临时目录已删除。
    let transcript = result.stderr.trim();
    assert!(transcript.ends_with("transcript.jsonl"));
    assert!(!std::path::Path::new(transcript).exists());
}

#[tokio::test]
async fn caps_each_stream_without_killing() {
    let script = "head -c 100000 /dev/zero | tr '\\0' a; head -c 10 /dev/zero | tr '\\0' b >&2";
    let result = run(
        request(shell(script), 5000, 64),
        &base_env(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, "completed");
    assert_eq!(result.stdout, "a".repeat(64));
    assert_eq!(result.stderr, "b".repeat(10));
}

#[tokio::test]
async fn times_out_and_cancels_the_tree() {
    let started = Instant::now();
    let result = run(
        request(shell("sleep 30"), 200, 64),
        &base_env(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, "timed_out");
    assert!(started.elapsed() < Duration::from_secs(5));
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });
    let result = run(request(shell("sleep 30"), 30_000, 64), &base_env(), &cancel)
        .await
        .unwrap();
    assert_eq!(result.status, "cancelled");
}

#[tokio::test]
async fn reclaims_descendants_holding_output() {
    let started = Instant::now();
    let result = run(
        request(shell("sleep 30 & echo done"), 30_000, 64),
        &base_env(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, "completed");
    assert_eq!(result.stdout, "done\n");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn reports_start_failures_like_node() {
    let program = Program::Process {
        command: "/nonexistent/hook".into(),
        args: vec![],
    };
    let result = run(
        request(program, 1000, 64),
        &base_env(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, "spawn_error");
    assert_eq!(
        result.error.as_deref(),
        Some("spawn /nonexistent/hook ENOENT")
    );
}
