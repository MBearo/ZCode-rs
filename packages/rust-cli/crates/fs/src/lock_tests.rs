#![allow(
    clippy::disallowed_methods,
    reason = "测试起子进程争锁、读子进程的环境变量（fs 只在 crate 级放开了文件类）"
)]
//! 目录锁（platform.md §5.5）：形状与 Node 相同，任务之间、进程之间都互斥。

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::*;

fn fast() -> LockOptions {
    LockOptions {
        retry_delays: vec![Duration::from_millis(10)],
        ownerless_grace: Duration::from_millis(50),
        max_wait: Duration::from_millis(400),
    }
}

#[tokio::test]
async fn owner_file_matches_node_format_and_is_removed_on_release() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credentials.json");
    let lock = acquire_file_lock(&file, &fast()).await.unwrap();
    let lock_dir = dir.path().join("credentials.json.lock");
    let owners = list_dir(&lock_dir).await.unwrap();
    assert_eq!(owners.len(), 1);
    let raw = std::fs::read_to_string(lock_dir.join(&owners[0])).unwrap();
    assert!(raw.ends_with('\n'));
    let value: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["pid"], std::process::id());
    let token = value["token"].as_str().unwrap();
    assert_eq!(owners[0], format!("owner-{token}.json"));
    assert!(token.starts_with(&format!("{}-", std::process::id())));
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["pid", "createdAt", "token"]);
    lock.release().await;
    assert!(!lock_dir.exists());
}

#[tokio::test]
async fn a_live_owner_blocks_until_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.json");
    let held = acquire_file_lock(&file, &fast()).await.unwrap();
    let error = acquire_file_lock(&file, &fast()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(error.to_string().starts_with(FILE_LOCK_TIMEOUT_CODE));
    held.release().await;
    acquire_file_lock(&file, &fast())
        .await
        .unwrap()
        .release()
        .await;
}

#[tokio::test]
async fn dead_owners_empty_directories_and_legacy_files_are_reclaimed() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("b.json");
    let lock_dir = dir.path().join("b.json.lock");
    // 不存在的进程（PID 上限之外）持有的锁可以回收。
    std::fs::create_dir(&lock_dir).unwrap();
    std::fs::write(
        lock_dir.join("owner-dead.json"),
        r#"{"pid":2147483000,"createdAt":1,"token":"dead"}"#,
    )
    .unwrap();
    acquire_file_lock(&file, &fast())
        .await
        .unwrap()
        .release()
        .await;
    // 空锁目录（mkdir 后崩溃）超过 grace 后回收。
    std::fs::create_dir(&lock_dir).unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    acquire_file_lock(&file, &fast())
        .await
        .unwrap()
        .release()
        .await;
    // 升级前的单文件锁：没有 PID、超过 grace。
    std::fs::write(&lock_dir, r#"{"createdAt":1}"#).unwrap();
    acquire_file_lock(&file, &fast())
        .await
        .unwrap()
        .release()
        .await;
    assert!(!lock_dir.exists());
}

#[tokio::test]
async fn with_file_lock_creates_the_parent_and_releases_on_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("nested/v2/provider_config.json");
    let failed: Result<(), io::Error> =
        with_file_lock(&file, &fast(), async || Err(io::Error::other("boom"))).await;
    assert_eq!(failed.unwrap_err().to_string(), "boom");
    assert!(file.parent().unwrap().is_dir());
    assert!(!lock_dir_of(&file).exists());
}

/// 修 bug（F17）：建出的锁目录在拿到锁之前被别的等待者回收时，建 owner 文件在 Linux 上报 NotFound、在 macOS
/// 上报 InvalidInput（EINVAL），两种都是“输掉了”、重来；别的错误照常报出。
#[test]
fn a_lost_directory_is_retried_on_every_platform() {
    for kind in [ErrorKind::NotFound, ErrorKind::InvalidInput] {
        assert!(lost_created(true, &io::Error::from(kind)), "{kind:?}");
        assert!(!lost_created(false, &io::Error::from(kind)), "{kind:?}");
    }
    assert!(!lost_created(
        true,
        &io::Error::from(ErrorKind::PermissionDenied)
    ));
}

/// 空目录不等 grace 就被回收：别的等待者常在 mkdir 与建 owner 文件之间删掉目录。16 个任务分 25 轮、每轮各取一次锁，
/// 一次错误都不能有（F17 修前 macOS 负载下约 1/5 的运行出现 `Invalid argument (os error 22)`）。
///
/// 修复线 F35：原来 16 个任务各自连取 25 次。锁不保证先来先得（platform.md §5.5），刚释放的任务马上又拿到，退避中的
/// 等待者要等别的任务都取完（插桩：最长的一次等待约等于整个测试的时长），负载下整个测试超过 20 s 时这一次就超时，
/// 测试靠真实时间的余量。现在每轮等 16 个任务都到齐再一起取（同时起跑，回收撞上建锁的机会更多），一次等待最多等过
/// 同一轮的另外 15 次，与轮数、总时长无关；重试间隔 1 ms，一轮在几十毫秒内取完。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_directory_reclaimed_mid_acquire_makes_the_creator_retry() {
    let dir = tempfile::tempdir().unwrap();
    let file = Arc::new(dir.path().join("credentials.json"));
    let options = Arc::new(LockOptions {
        retry_delays: vec![Duration::from_millis(1)],
        ownerless_grace: Duration::ZERO,
        max_wait: Duration::from_secs(20),
    });
    let round = Arc::new(tokio::sync::Barrier::new(16));
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let (file, options, round) = (file.clone(), options.clone(), round.clone());
        tasks.push(tokio::spawn(async move {
            let mut errors = Vec::new();
            for _ in 0..25 {
                round.wait().await;
                let held: io::Result<()> = with_file_lock(&file, &options, async || Ok(())).await;
                if let Err(error) = held {
                    errors.push(format!("{:?}: {error}", error.kind()));
                }
            }
            errors
        }));
    }
    let mut errors = Vec::new();
    for task in tasks {
        errors.extend(task.await.unwrap());
    }
    assert!(errors.is_empty(), "{errors:#?}");
}

/// 两个任务交替读改写同一个文件：一次都不丢。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tasks_serialize_read_modify_write() {
    let dir = tempfile::tempdir().unwrap();
    let file = Arc::new(dir.path().join("counter.json"));
    std::fs::write(file.as_path(), "0").unwrap();
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let file = file.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..10 {
                increment(&file).await;
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(std::fs::read_to_string(file.as_path()).unwrap(), "40");
}

async fn increment(file: &Path) {
    let options = LockOptions {
        max_wait: Duration::from_secs(20),
        ..LockOptions::default()
    };
    with_file_lock(file, &options, async || {
        let value: u32 = tokio::fs::read_to_string(file)
            .await?
            .trim()
            .parse()
            .map_err(io::Error::other)?;
        tokio::task::yield_now().await;
        tokio::fs::write(file, (value + 1).to_string()).await
    })
    .await
    .unwrap();
}

// ---- 两个进程：测试二进制把自己作为子进程启动，子进程按环境变量扮演锁的另一方 ----

const ROLE: &str = "RUST_CLI_LOCK_TEST_ROLE";
const TARGET: &str = "RUST_CLI_LOCK_TEST_FILE";

fn child(role: &str, file: &Path) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "lock::tests::child_process",
            "--ignored",
            "--nocapture",
        ])
        .env(ROLE, role)
        .env(TARGET, file)
        .spawn()
        .unwrap()
}

/// 子进程的入口（只被上面的父进程测试启动）：`increment` 读改写 25 次；`hold` 持锁并留下标记后一直等。
#[tokio::test]
#[ignore = "被两个进程的测试作为子进程启动"]
async fn child_process() {
    let (Ok(role), Some(file)) = (std::env::var(ROLE), std::env::var_os(TARGET)) else {
        return;
    };
    let file = PathBuf::from(file);
    match role.as_str() {
        "increment" => {
            for _ in 0..25 {
                increment(&file).await;
            }
        }
        "hold" => {
            let _lock = acquire_file_lock(&file, &LockOptions::default())
                .await
                .unwrap();
            std::fs::write(marker(&file), "held").unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        other => panic!("unknown role {other}"),
    }
}

fn marker(file: &Path) -> PathBuf {
    file.with_extension("held")
}

#[tokio::test]
async fn processes_serialize_read_modify_write() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("counter.json");
    std::fs::write(&file, "0").unwrap();
    let mut children = [child("increment", &file), child("increment", &file)];
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "50");
    assert!(!lock_dir_of(&file).exists());
}

#[tokio::test]
async fn a_live_process_holds_the_lock_and_its_death_releases_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credentials.json");
    let mut holder = child("hold", &file);
    let started = Instant::now();
    while !marker(&file).exists() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "holder never took the lock"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // 持锁的进程活着：等待者超时，不会删它的锁。
    let error = acquire_file_lock(&file, &fast()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(lock_dir_of(&file).exists());
    // 进程被杀，锁目录留在原地：owner 已退出，等待者回收它。
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert!(lock_dir_of(&file).exists());
    acquire_file_lock(&file, &fast())
        .await
        .unwrap()
        .release()
        .await;
}
