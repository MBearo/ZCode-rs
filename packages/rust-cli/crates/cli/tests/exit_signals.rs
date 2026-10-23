//! R5-2 的真实进程（docs/specs/rust-cli/session-lifecycle.md §3.5）：`zcode -p` 的 Bash 正在 `sleep` 时给 zcode
//! 发 SIGINT / SIGTERM / SIGHUP。优雅退出：中止回合，Bash 的进程树随之结束，SessionEnd hook
//! 运行，日志写完 `process.exit`，退出码 0 / 143 / 129。修复前进程按信号的默认动作直接结束：没有退出码，
//! `sleep` 留成孤儿，SessionEnd 与日志的收尾都被跳过。
//!
//! 超时只用于测试判定，不参与被测的同步。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、发信号、检查写出的文件"
)]

mod sse;

use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(20);

/// 等到 `check` 为真；超过期限时失败。
fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let started = Instant::now();
    while !check() {
        assert!(started.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn kill(signal: &str, pid: u32) -> bool {
    rust_cli_proc::std_command("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    let mut status = None;
    wait_for("zcode to exit", || {
        status = child.try_wait().unwrap();
        status.is_some()
    });
    status.unwrap()
}

/// 跑到 Bash 的 sleep 开始后发信号，核对收尾；返回 zcode 的退出码。
fn run_and_signal(signal: &str) -> Option<i32> {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(home.join(".zcode/cli")).unwrap();
    let ended = base.join("SESSION_END");
    // Node 配置文件的形状（config.md §2）：`hooks.enabled` 与 `hooks.events`。
    let config = serde_json::json!({"hooks": {"enabled": true, "events": {"SessionEnd": [{"hooks": [
        {"type": "command", "command": format!("echo end > {}", ended.display())}
    ]}]}}});
    std::fs::write(home.join(".zcode/cli/config.json"), config.to_string()).unwrap();
    let pid_file = base.join("SLEEP_PID");
    let bash = serde_json::json!({
        "command": format!("sh -c 'echo $$ > {}; exec sleep 30'", pid_file.display()),
        "description": "wait",
    })
    .to_string();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(0, "toolu_sleep", "Bash", &bash)]),
        sse::text_reply("done"),
    ]);
    let mut child = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", "go", "--permission-mode", "bypassPermissions"])
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "claude-opus-5-5")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut sleep_pid = None;
    wait_for("the Bash tool to start sleep", || {
        sleep_pid = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok());
        sleep_pid.is_some()
    });
    let sleep_pid = sleep_pid.unwrap();
    assert!(kill(signal, child.id()));
    let status = wait_exit(&mut child);
    // 进程树已结束：`kill -0` 找不到 sleep（它的父进程已回收它，或它被 init 收养后回收）。
    let orphan = Instant::now();
    while kill("0", sleep_pid) && orphan.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let left = kill("0", sleep_pid);
    if left {
        kill("KILL", sleep_pid);
    }
    assert!(!left, "sleep {sleep_pid} outlived zcode after SIG{signal}");
    assert!(ended.exists(), "SessionEnd did not run after SIG{signal}");
    let log = home.join(".zcode/rust-cli/log");
    let text: String = std::fs::read_dir(&log)
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert!(
        text.contains("\"process.signal\"") && text.contains("\"process.exit\""),
        "{text}"
    );
    status.code()
}

#[test]
fn sigint_aborts_the_turn_and_exits_0() {
    assert_eq!(run_and_signal("INT"), Some(0));
}

#[test]
fn sigterm_aborts_the_turn_and_exits_143() {
    assert_eq!(run_and_signal("TERM"), Some(143));
}

#[test]
fn sighup_aborts_the_turn_and_exits_129() {
    assert_eq!(run_and_signal("HUP"), Some(129));
}

/// 修复线 F38（session-lifecycle.md §3.5 D4）：UserPromptSubmit hook 还在跑（输入还没提交）时收到退出信号。
/// SIGTERM：直接退出，transcript 里什么都没有；以 `Shutdown` 中止（hook 的进程照样回收），
/// 库里没有这条输入，退出码 143。SIGINT：`-p` 以 user-cancel 中止，输入与中断标记落盘，退出码 0（F37）。
/// F37 时 SIGTERM 同 SIGINT（输入与标记落库）。
fn signal_during_the_prompt_hook(signal: &str) -> (Option<i32>, String) {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(home.join(".zcode/cli")).unwrap();
    let pid_file = base.join("HOOK_PID");
    let hook = format!("echo $$ > {}; exec sleep 30", pid_file.display());
    let config = serde_json::json!({"hooks": {"enabled": true, "events": {"UserPromptSubmit": [{"hooks": [
        {"type": "command", "command": hook}
    ]}]}}});
    std::fs::write(home.join(".zcode/cli/config.json"), config.to_string()).unwrap();
    let fixture = sse::serve(vec![sse::text_reply("never sent")]);
    let mut child = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", "gap prompt", "--permission-mode", "bypassPermissions"])
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "claude-opus-5-5")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut hook_pid = None;
    wait_for("the prompt hook to start", || {
        hook_pid = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok());
        hook_pid.is_some()
    });
    let hook_pid = hook_pid.unwrap();
    assert!(kill(signal, child.id()));
    let status = wait_exit(&mut child);
    let orphan = Instant::now();
    while kill("0", hook_pid) && orphan.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let left = kill("0", hook_pid);
    if left {
        kill("KILL", hook_pid);
    }
    assert!(
        !left,
        "the hook {hook_pid} outlived zcode after SIG{signal}"
    );
    assert!(fixture.main_requests().is_empty(), "no model request");
    (status.code(), stored_messages(&home, &work))
}

/// 库里（Node 的 db.sqlite，经 NodeStore 冷读取）这个工作区唯一会话的消息（JSON）；没有会话时为空。
fn stored_messages(home: &std::path::Path, work: &std::path::Path) -> String {
    use rust_cli_kernel_api::store::SessionStore;
    let paths = rust_cli_paths::StoragePaths::resolve(
        &rust_cli_paths::StorageSettings::default(),
        home,
        work,
    );
    let ids = rust_cli_node_store::NodeIds::new(std::sync::Arc::new(
        rust_cli_node_store::SystemClock::utc(),
    ));
    let store = rust_cli_node_store::NodeStore::new(paths, std::sync::Arc::new(ids));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let filter = rust_cli_kernel_api::store::SessionFilter {
            workspace: rust_cli_kernel_api::env::WorkspaceRef {
                environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
                root: rust_cli_kernel_api::env::EnvPath::new(work.to_string_lossy()),
            },
            limit: 1,
        };
        let Some(session) = store.list(filter).await.unwrap().into_iter().next() else {
            return String::new();
        };
        let loaded = store.load(&session.id).await.unwrap();
        serde_json::to_string(&loaded.messages).unwrap()
    })
}

#[test]
fn sigterm_while_the_prompt_hook_runs_records_nothing_and_exits_143() {
    let (code, messages) = signal_during_the_prompt_hook("TERM");
    assert_eq!(code, Some(143));
    assert!(!messages.contains("gap prompt"), "{messages}");
    assert!(
        !messages.contains("[Request interrupted by user]"),
        "{messages}"
    );
}

#[test]
fn sigint_while_the_prompt_hook_runs_keeps_the_input_with_the_marker() {
    let (code, messages) = signal_during_the_prompt_hook("INT");
    assert_eq!(code, Some(0));
    let input = messages.find("gap prompt").unwrap_or(usize::MAX);
    let marker = messages
        .find("[Request interrupted by user]")
        .unwrap_or(usize::MAX);
    assert!(input < marker && marker != usize::MAX, "{messages}");
}

/// stream-json 输出时收到退出信号（headless.md §3.2，修复线 F39）：返回 stdout 与退出码。
fn stream_json_and_signal(signal: &str) -> (Option<i32>, String) {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let pid_file = base.join("SLEEP_PID");
    let bash = serde_json::json!({
        "command": format!("sh -c 'echo $$ > {}; exec sleep 30'", pid_file.display()),
        "description": "wait",
    })
    .to_string();
    let fixture = sse::serve(vec![sse::tools_reply(&[sse::tool_block(
        0,
        "toolu_sleep",
        "Bash",
        &bash,
    )])]);
    let child = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args([
            "-p",
            "go",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
        ])
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "claude-opus-5-5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for("the Bash tool to start sleep", || pid_file.exists());
    assert!(kill(signal, child.id()));
    let output = child.wait_with_output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

/// SIGINT：写到这个回合为止的事件与一个 `error_during_execution` 的 result（`terminal_reason: aborted_streaming`），
/// 退出码仍按信号是 0。
#[test]
fn sigint_in_stream_json_writes_the_interrupted_result() {
    let (code, text) = stream_json_and_signal("INT");
    assert_eq!(code, Some(0));
    let last: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(last["type"], "result", "{text}");
    assert_eq!(last["subtype"], "error_during_execution");
    assert_eq!(last["terminal_reason"], "aborted_streaming");
    assert_eq!(last["is_error"], true);
}

/// SIGTERM（修复线 F38：直接退出）：没有这个回合的 result，退出码 143。
#[test]
fn sigterm_in_stream_json_writes_no_result() {
    let (code, text) = stream_json_and_signal("TERM");
    assert_eq!(code, Some(143));
    assert!(!text.contains("\"type\":\"result\""), "{text}");
    assert!(text.starts_with("{\"type\":\"system\""), "{text}");
}
