#![allow(
    clippy::disallowed_methods,
    reason = "测试准备与读回 telemetry-state.json、起两个子进程同时首次生成"
)]
//! 设备身份 deviceMid（platform.md §10，工作线 F52）：与 Node CLI `ensureCliDeviceMid` 同一个文件、字段与锁。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rust_cli_fs::PidLockOptions;
use rust_cli_paths::{StoragePaths, StorageSettings};

use super::DeviceMid;

fn paths(home: &Path) -> StoragePaths {
    StoragePaths::resolve(&StorageSettings::default(), home, home)
}

/// 记下调用次数的 UUID 来源：第 n 次是 `uuid-<n>`。
fn counting() -> (super::NewUuid, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let uuid: super::NewUuid =
        Arc::new(move || format!("uuid-{}", counter.fetch_add(1, Ordering::SeqCst) + 1));
    (uuid, calls)
}

fn state_file(home: &Path) -> PathBuf {
    home.join(".zcode/v2/telemetry-state.json")
}

fn quick() -> PidLockOptions {
    PidLockOptions {
        retry: Duration::from_millis(5),
        attempts: 20,
        stale: Duration::from_secs(5 * 60),
    }
}

/// App / Node 已经写了：直接用，不写文件，不生成。
#[tokio::test]
async fn an_existing_device_mid_is_used_as_is() {
    let home = tempfile::tempdir().unwrap();
    let file = state_file(home.path());
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let text = "{\"deviceMid\":\"7f1431f0-53e8-41c1-9cf9-22d11aa51d6a\",\"other\":1}";
    std::fs::write(&file, text).unwrap();
    let (uuid, calls) = counting();
    let device = DeviceMid::new(&paths(home.path()), uuid);
    assert_eq!(device.get().await, "7f1431f0-53e8-41c1-9cf9-22d11aa51d6a");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// 第一次：生成、按 `JSON.stringify(state, null, 2)` 写入，保留其他字段与顺序，`deviceMid` 在最后；之后同一个值，
/// 不再读写。锁用完删掉。
#[tokio::test]
async fn the_first_request_generates_and_keeps_the_other_fields() {
    let home = tempfile::tempdir().unwrap();
    let file = state_file(home.path());
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        "{\"lastDailyActiveDate\":\"2026-10-21\",\"dailyActiveInFlight\":false,\"deviceMid\":\"\"}",
    )
    .unwrap();
    let (uuid, calls) = counting();
    let device = DeviceMid::new(&paths(home.path()), uuid);
    assert_eq!(device.get().await, "uuid-1");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "{\n  \"lastDailyActiveDate\": \"2026-10-21\",\n  \"dailyActiveInFlight\": false,\n  \"deviceMid\": \"uuid-1\"\n}"
    );
    std::fs::remove_file(&file).unwrap();
    assert_eq!(device.get().await, "uuid-1");
    assert!(!file.exists(), "the value is cached in the process");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!home.path().join(".zcode/v2/telemetry-state.lock").exists());
}

/// 文件不存在（目录也没有）或损坏时当作空对象。
#[tokio::test]
async fn a_missing_or_broken_file_is_an_empty_state() {
    for broken in [
        None,
        Some("not json"),
        Some("[1,2]"),
        Some("{\"deviceMid\":7}"),
    ] {
        let home = tempfile::tempdir().unwrap();
        let file = state_file(home.path());
        if let Some(text) = broken {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, text).unwrap();
        }
        let (uuid, _) = counting();
        let device = DeviceMid::new(&paths(home.path()), uuid);
        assert_eq!(device.get().await, "uuid-1", "{broken:?}");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            written,
            serde_json::json!({"deviceMid": "uuid-1"}),
            "{broken:?}"
        );
    }
}

/// 拿到锁之后再读一次：等锁期间别的进程写好了，就用它的值，不生成、不覆盖。
#[tokio::test]
async fn the_value_written_while_waiting_for_the_lock_wins() {
    let home = tempfile::tempdir().unwrap();
    let file = state_file(home.path());
    let lock = home.path().join(".zcode/v2/telemetry-state.lock");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    // 另一个“进程”（这里是本进程的 pid，活着）持锁。
    std::fs::write(
        &lock,
        format!("{{\"createdAt\":1,\"pid\":{}}}", std::process::id()),
    )
    .unwrap();
    let (uuid, calls) = counting();
    let device = DeviceMid::new(&paths(home.path()), uuid);
    let writer = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::write(&file, "{\"deviceMid\":\"from-the-other-process\"}").unwrap();
        std::fs::remove_file(&lock).unwrap();
    };
    let (value, ()) = tokio::join!(device.get(), writer);
    assert_eq!(value, "from-the-other-process");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "{\"deviceMid\":\"from-the-other-process\"}"
    );
}

/// 出错不挡请求：配置根不是目录（写不了）、锁一直被活着的进程占着（超时）时，用本进程生成的同一个 UUID。
#[tokio::test]
async fn file_system_errors_fall_back_to_one_generated_id() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".zcode")).unwrap();
    std::fs::write(home.path().join(".zcode/v2"), "a file, not a directory").unwrap();
    let (uuid, calls) = counting();
    let device = DeviceMid::new(&paths(home.path()), uuid);
    assert_eq!(device.get().await, "uuid-1");
    assert_eq!(device.get().await, "uuid-1");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let home = tempfile::tempdir().unwrap();
    let lock = home.path().join(".zcode/v2/telemetry-state.lock");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(
        &lock,
        format!("{{\"createdAt\":1,\"pid\":{}}}", std::process::id()),
    )
    .unwrap();
    let (uuid, calls) = counting();
    let device = DeviceMid::new(&paths(home.path()), uuid).with_lock_options(quick());
    assert_eq!(device.get().await, "uuid-1");
    assert_eq!(device.get().await, "uuid-1");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!state_file(home.path()).exists());
}

/// 同一个进程里并发的第一次请求：只解析一次。
#[tokio::test]
async fn concurrent_first_requests_resolve_once() {
    let home = tempfile::tempdir().unwrap();
    let (uuid, calls) = counting();
    let device = DeviceMid::new(&paths(home.path()), uuid);
    let (a, b, c) = tokio::join!(device.get(), device.get(), device.get());
    assert_eq!(
        (a.as_str(), b.as_str(), c.as_str()),
        ("uuid-1", "uuid-1", "uuid-1")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// ---- 两个进程：测试二进制把自己作为子进程启动（同 fs `lock_tests.rs` 的写法） ----

const HOME: &str = "RUST_CLI_DEVICE_TEST_HOME";
const OUT: &str = "RUST_CLI_DEVICE_TEST_OUT";

fn child(home: &Path, out: &Path) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "device::tests::child_process",
            "--ignored",
            "--nocapture",
        ])
        .env(HOME, home)
        .env(OUT, out)
        .spawn()
        .unwrap()
}

/// 子进程的入口：用自己的 pid 生成候选值，把拿到的设备身份写进 `OUT`。
#[tokio::test]
#[ignore = "被两个进程的测试作为子进程启动"]
async fn child_process() {
    let (Some(home), Some(out)) = (std::env::var_os(HOME), std::env::var_os(OUT)) else {
        return;
    };
    let pid = std::process::id();
    let device = DeviceMid::new(
        &paths(Path::new(&home)),
        Arc::new(move || format!("pid-{pid}")),
    );
    std::fs::write(out, device.get().await).unwrap();
}

/// 两个进程同时第一次生成：文件里只有一个值，两个进程用的都是它（platform.md §10 的时序图）。
#[tokio::test]
async fn two_processes_generating_at_once_keep_one_value() {
    for _ in 0..3 {
        let home = tempfile::tempdir().unwrap();
        let outs = [home.path().join("a.out"), home.path().join("b.out")];
        let mut children = [child(home.path(), &outs[0]), child(home.path(), &outs[1])];
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        let file: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(state_file(home.path())).unwrap())
                .unwrap();
        let stored = file["deviceMid"].as_str().unwrap();
        assert!(stored.starts_with("pid-"), "{stored}");
        for out in &outs {
            assert_eq!(std::fs::read_to_string(out).unwrap(), stored);
        }
        assert!(!home.path().join(".zcode/v2/telemetry-state.lock").exists());
    }
}
