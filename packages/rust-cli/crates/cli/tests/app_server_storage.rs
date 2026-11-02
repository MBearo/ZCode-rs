//! app-server 的会话库 ID（`startup/storageState` 的 `databaseId`，docs/specs/rust-cli/app-protocol.md §4.2）：真实进程
//! `zcode app-server --stdio`，临时 home。ID 对应实际打开的库：配置改了 `storage.sessionDbPath` 时不同于缺省位置的 ID，
//! 环境变量指向同一个库时相同；同一次启动的 `checking` 与 `ready` 是同一个 ID（Host 丢掉 ID 变了的帧）。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起真实进程、读它的标准输出"
)]

mod system_env;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;
use system_env::KeepSystem as _;

use serde_json::Value;

/// 等启动帧的上限（存活的上限：正常不到 1 s）。
const FRAME_WAIT: Duration = Duration::from_secs(60);

/// 起一次 app-server，读到 `ready` 或 `failed` 为止：每个 `startup/storageState` 帧的（阶段，库 ID）。
fn storage_states(home: &Path, work: &Path, extra: &[(&str, &Path)]) -> Vec<(String, String)> {
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(["app-server", "--stdio"])
        .current_dir(work)
        .env_clear()
        .keep_system()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", home);
    for (name, value) in extra {
        command.env(name, value);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (lines, frames) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut states = Vec::new();
    loop {
        let line = frames.recv_timeout(FRAME_WAIT).unwrap();
        let frame: Value = serde_json::from_str(&line).unwrap();
        if frame["method"] != "startup/storageState" {
            continue;
        }
        let params = &frame["params"];
        let phase = params["phase"].as_str().unwrap().to_owned();
        states.push((
            phase.clone(),
            params["databaseId"].as_str().unwrap().to_owned(),
        ));
        if phase == "ready" || phase == "failed" {
            break;
        }
    }
    // 关掉输入：app-server 读到结尾就退出。
    drop(child.stdin.take());
    child.wait().unwrap();
    states
}

/// 一次启动的库 ID：`checking` → `ready`，两帧同一个 ID。
fn database_id(home: &Path, work: &Path, extra: &[(&str, &Path)]) -> String {
    let states = storage_states(home, work, extra);
    let phases: Vec<&str> = states.iter().map(|(phase, _)| phase.as_str()).collect();
    assert_eq!(phases, ["checking", "ready"], "{states:?}");
    assert_eq!(states[0].1, states[1].1, "{states:?}");
    states[1].1.clone()
}

#[test]
fn the_database_id_names_the_database_that_is_opened() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(home.join(".zcode/cli")).unwrap();
    let default = database_id(&home, &work, &[]);
    assert!(home.join(".zcode/cli/db/db.sqlite").exists());

    // 配置文件把库移到别处：ID 跟着变。
    let moved = base.join("config.sqlite");
    let config = serde_json::json!({"storage": {"sessionDbPath": moved}});
    std::fs::write(home.join(".zcode/cli/config.json"), config.to_string()).unwrap();
    let configured = database_id(&home, &work, &[]);
    assert!(moved.exists());
    assert_ne!(
        configured, default,
        "the id still names the default database"
    );

    // 环境变量覆盖配置文件、指向同一个库：ID 相同（ID 只看实际打开的库，不看位置从哪里来）。
    let config = serde_json::json!({"storage": {"sessionDbPath": base.join("other.sqlite")}});
    std::fs::write(home.join(".zcode/cli/config.json"), config.to_string()).unwrap();
    let from_env = database_id(&home, &work, &[("ZCODE_SESSION_DB_PATH", &moved)]);
    assert_eq!(from_env, configured);
    assert!(!base.join("other.sqlite").exists());
}
