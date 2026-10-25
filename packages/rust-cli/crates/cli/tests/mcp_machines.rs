//! 远端 MCP 的会话级真实进程测试（docs/specs/rust-cli/remote-exec.md §22.10）：`zcode -p` 连本地 SSE 夹具，debug 构建的
//! `_ZCODE_ASSUME_LOOPBACK_MACHINES` 把 `machines.json` 里连着的 dev-a 经 loopback 连上（一个执行器子进程，工作目录与本机
//! 不同）。`mcp-machines.json` 把一个 stdio 服务器（仓库里的 bash 夹具）绑到 dev-a：它由那台机器的执行器起，`${VAR}` 在
//! 执行器展开；同一份配置不绑定时照旧在本机、原样不展开（Node 配置的规则）。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录与配置、起本地 HTTP 夹具、读回运行日志、检查进程"
)]

mod sse;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use serde_json::{Value, json};

struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
    remote: PathBuf,
}

fn fixture_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-shell.sh")
}

/// 临时 HOME、本机工作区、dev-a 的工作目录；两个同样的服务器，`bindings` 决定哪些绑到哪台机器。
fn sandbox(bindings: Value) -> Sandbox {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work, remote) = (base.join("home"), base.join("work"), base.join("remote"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&remote).unwrap();
    std::fs::create_dir_all(home.join(".zcode/rust-cli")).unwrap();
    std::fs::create_dir_all(home.join(".zcode/cli")).unwrap();
    std::fs::write(work.join("notes.txt"), "local notes\n").unwrap();
    std::fs::write(remote.join("notes.txt"), "remote notes\n").unwrap();
    let machines = json!({"version": 1, "machines": [
        {"name": "dev-a", "ssh": "dev-a", "workingDirectory": remote.display().to_string(), "connected": true}
    ]});
    std::fs::write(
        home.join(".zcode/rust-cli/machines.json"),
        machines.to_string(),
    )
    .unwrap();
    std::fs::write(
        home.join(".zcode/rust-cli/mcp-machines.json"),
        json!({"version": 1, "servers": bindings}).to_string(),
    )
    .unwrap();
    let server = json!({
        "type": "stdio",
        "command": "/bin/bash",
        "args": [fixture_script().display().to_string()],
        "env": {"MCP_PROBE": "${HOME}"},
        "alwaysLoad": true
    });
    let config = json!({"mcp": {"servers": {"remote-fx": server, "local-fx": server}}});
    std::fs::write(home.join(".zcode/cli/config.json"), config.to_string()).unwrap();
    Sandbox {
        _dir: dir,
        home,
        work,
        remote,
    }
}

fn run(sandbox: &Sandbox, fixture: &sse::Fixture) -> Output {
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", "go", "--permission-mode", "bypassPermissions"])
        .current_dir(&sandbox.work)
        .env_clear()
        .env("HOME", &sandbox.home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &sandbox.home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "glm-5")
        .env("ZCODE_RUNTIME_ENV", "test")
        .env("_ZCODE_ASSUME_LOOPBACK_MACHINES", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn tool(id: &str, name: &str) -> String {
    sse::tools_reply(&[sse::tool_block(0, id, name, "{}")])
}

/// 请求最后一条 user 消息里的工具结果文字。
fn last_result(request: &Value) -> String {
    let messages = request["messages"].as_array().unwrap();
    messages.last().unwrap()["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["type"] == "tool_result")
        .map(|block| match &block["content"] {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn field<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in {text}"))
}

fn tool_names(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// 等一个进程不在了（测试里的等待，有上限）。
///
/// 缺陷修复（已知不稳定，F96 记下的 `the local server is left`）：`-p` 退出时本机 MCP 服务器的进程组收到 SIGKILL（proc 的
/// `Tracked` 在丢弃时发），信号的投递与退出后由 init 回收都是异步的；回收之前 `kill -0` 仍然成功。以前在 `-p` 返回后马上
/// 查一次，负载下偶尔查到还没回收的进程。进程真的留下来（没发信号）时这里照样失败。
fn gone(pid: &str) -> bool {
    for _ in 0..200 {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

fn run_log(home: &Path) -> String {
    let log = home.join(".zcode/rust-cli/log");
    std::fs::read_dir(&log)
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn a_bound_server_runs_on_its_machine_and_its_tool_answers() {
    let sandbox = sandbox(json!({"remote-fx": {"machine": "dev-a"}}));
    let fixture = sse::serve(vec![
        tool("toolu_remote", "mcp__remote-fx__where"),
        tool("toolu_local", "mcp__local-fx__where"),
        sse::text_reply("done"),
    ]);
    run(&sandbox, &fixture);
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 3);
    // 模型看到的不变：两个服务器的工具同名规则，MCP 工具没有 machine 参数。
    let names = tool_names(&requests[0]);
    assert!(
        names.contains(&"mcp__remote-fx__where".to_owned()),
        "{names:?}"
    );
    assert!(
        names.contains(&"mcp__local-fx__where".to_owned()),
        "{names:?}"
    );
    let schema = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "mcp__remote-fx__where")
        .unwrap();
    assert!(
        schema["input_schema"]["properties"]
            .get("machine")
            .is_none()
    );
    // 绑定的服务器在 dev-a 的工作目录里起；`${HOME}` 由执行器展开（loopback 的执行器继承同一个 HOME）。
    let remote = last_result(&requests[1]);
    assert_eq!(field(&remote, "cwd"), sandbox.remote.display().to_string());
    assert_eq!(field(&remote, "notes"), "remote notes");
    assert_eq!(field(&remote, "probe"), sandbox.home.display().to_string());
    // 同一份配置不绑定：在本机的工作区起，Node 配置原样使用、不展开（ext.md §3.1）。
    let local = last_result(&requests[2]);
    assert_eq!(field(&local, "cwd"), sandbox.work.display().to_string());
    assert_eq!(field(&local, "notes"), "local notes");
    assert_eq!(field(&local, "probe"), "${HOME}");
    // 收尾：`-p` 结束后执行器与它起的服务器进程都不在了。
    assert!(gone(field(&remote, "pid")), "the remote server is left");
    assert!(gone(field(&local, "pid")), "the local server is left");
}

#[tokio::test]
async fn a_server_bound_to_a_machine_that_is_not_connected_fails() {
    let sandbox = sandbox(json!({"remote-fx": {"machine": "gpu-1"}}));
    let fixture = sse::serve(vec![sse::text_reply("done")]);
    run(&sandbox, &fixture);
    let requests = fixture.main_requests();
    let names = tool_names(&requests[0]);
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with("mcp__remote-fx__")),
        "{names:?}"
    );
    assert!(
        names.contains(&"mcp__local-fx__where".to_owned()),
        "{names:?}"
    );
    let log = run_log(&sandbox.home);
    assert!(
        log.contains("Machine \\\"gpu-1\\\" is not connected.")
            || log.contains("Machine \"gpu-1\" is not connected."),
        "{log}"
    );
}
