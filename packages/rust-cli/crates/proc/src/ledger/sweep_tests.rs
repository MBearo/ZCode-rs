// 复制自 rust/crates/proc/src/ledger/sweep_tests.rs（feat/rs 79b6691c55）。
#![allow(clippy::disallowed_methods, reason = "测试直接准备文件系统夹具")]
//! 账本与清扫（docs/specs/rust-tool-process-reaping.md 4），用真实的进程树。测试只向自己启动的
//! 进程发信号。

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};

use tokio::process::{Child, Command};

use super::super::identity::{Probe, Process, Start, probe, this_process};
use super::super::record::{self, Record};
use super::super::{Containment, Isolation, Ledger, Purpose};
use crate::tree::Shutdown;

/// 自成会话的两层进程树 `sh -c 'sleep 30 & echo $!; wait'`：根进程等待它的后台子进程。
struct Tree {
    pid: i32,
    background: i32,
}

/// 结束时回收本测试创建的进程组，失败的用例也不遗留 30 秒的 sleep。
struct Cleanup(Vec<i32>);

impl Drop for Cleanup {
    #[expect(unsafe_code, reason = "测试清理：kill(2)")]
    fn drop(&mut self) {
        for group in &self.0 {
            // SAFETY: kill 只投递信号；目标是本测试创建的进程组。
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }
}

const SCRIPT: &str = "sleep 30 & echo $!; wait";

fn sh() -> Command {
    let mut command = crate::command("/bin/sh");
    command
        .args(["-c", SCRIPT])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // 模拟所有者被强杀：丢弃句柄不能顺带结束根进程。
        .kill_on_drop(false);
    command
}

async fn background_of(child: &mut Child) -> i32 {
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .await
        .unwrap();
    line.trim().parse().unwrap()
}

/// 不经账本启动的树（记录由测试按需要的所有者写入）。句柄随即丢弃：与被过继的树一样，根进程
/// 结束后由别人回收（这里是 tokio），不会一直是僵尸进程。
async fn tree() -> Tree {
    let mut command = sh();
    crate::group::isolate(&mut command);
    let mut root = command.spawn().unwrap();
    let pid = i32::try_from(root.id().unwrap()).unwrap();
    let background = background_of(&mut root).await;
    Tree { pid, background }
}

async fn identity(pid: i32) -> Process {
    let Probe::Running(start) = probe(pid).await else {
        panic!("process {pid} is not running");
    };
    Process { pid, start }
}

/// 一个已经结束的进程（模拟被强杀的所有者）。
async fn ended_process() -> Process {
    let mut child = crate::command("sleep").arg("30").spawn().unwrap();
    let process = identity(i32::try_from(child.id().unwrap()).unwrap()).await;
    child.kill().await.unwrap();
    process
}

async fn running(pid: i32) -> bool {
    matches!(probe(pid).await, Probe::Running(_))
}

/// 等 `pid` 结束（僵尸进程算已结束）。
async fn ended(pid: i32) -> bool {
    tokio::time::timeout(Duration::from_secs(5), async {
        while running(pid).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok()
}

fn records(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut names: Vec<_> = entries
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

async fn write(dir: &Path, root: Process, owner: Process, instance: u64) -> String {
    let name = Record {
        purpose: Purpose::Bash,
        root,
        owner,
        instance,
    }
    .name();
    record::create(dir, &name).await.unwrap();
    name
}

#[tokio::test]
async fn a_tree_whose_owner_process_is_gone_is_ended_and_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let tree = tree().await;
    let _cleanup = Cleanup(vec![tree.pid]);
    write(
        dir.path(),
        identity(tree.pid).await,
        ended_process().await,
        1,
    )
    .await;

    Ledger::open(dir.path().into()).sweep().await;

    assert_eq!(
        (ended(tree.pid).await, ended(tree.background).await),
        (true, true)
    );
    assert_eq!(records(dir.path()), Vec::<String>::new());
}

/// 缺陷 R2-1：账本目录原来在共享的临时目录里，别人可以放一条伪造的记录（根进程写受害者自己的进程、
/// 所有者写一个已结束的进程），让受害者的清扫结束自己的进程树。现在清扫只认本用户的普通文件：这里用
/// 符号链接代表“不是本用户写下的记录”（非 root 的测试造不出属主是别人的文件）。
#[tokio::test]
async fn records_that_are_not_our_regular_files_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let tree = tree().await;
    let _cleanup = Cleanup(vec![tree.pid]);
    let elsewhere = tempfile::tempdir().unwrap();
    let name = write(
        elsewhere.path(),
        identity(tree.pid).await,
        ended_process().await,
        1,
    )
    .await;
    std::os::unix::fs::symlink(elsewhere.path().join(&name), dir.path().join(&name)).unwrap();

    Ledger::open(dir.path().into()).sweep().await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        (running(tree.pid).await, running(tree.background).await),
        (true, true)
    );
}

#[tokio::test]
async fn trees_of_a_running_owner_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    // 另一个仍在运行的所有者进程。
    let mut owner = crate::command("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let foreign = tree().await;
    let foreign_owner = identity(i32::try_from(owner.id().unwrap()).unwrap()).await;
    let foreign_record = write(dir.path(), identity(foreign.pid).await, foreign_owner, 1).await;
    // 本进程里仍在运行的实例。
    let running_instance = Ledger::open(dir.path().into());
    let mut command = sh();
    let mut own = running_instance
        .spawn(
            &mut command,
            Purpose::Bash,
            Isolation::Session,
            Containment::OwnJob,
        )
        .await
        .unwrap();
    let own_background = background_of(own.child()).await;
    let own_pid = i32::try_from(own.pid()).unwrap();
    let _cleanup = Cleanup(vec![foreign.pid, own_pid]);
    let expected = records(dir.path());
    assert_eq!(expected.len(), 2);
    assert!(expected.contains(&foreign_record));

    Ledger::open(dir.path().into()).sweep().await;

    let alive = [foreign.pid, foreign.background, own_pid, own_background];
    let mut states = vec![];
    for pid in alive {
        states.push(running(pid).await);
    }
    assert_eq!(states, vec![true; 4]);
    assert_eq!(records(dir.path()), expected);
    own.terminate(Shutdown::Graceful).await.unwrap();
    owner.kill().await.unwrap();
}

#[tokio::test]
async fn a_reused_pid_is_never_signalled() {
    let dir = tempfile::tempdir().unwrap();
    let tree = tree().await;
    let _cleanup = Cleanup(vec![tree.pid]);
    // 同一个 pid、不同的启动时间：记录里的进程已经结束，这个 pid 现在属于别的进程。
    let reused = Process {
        pid: tree.pid,
        start: Start::parse("1").unwrap(),
    };
    write(dir.path(), reused, ended_process().await, 1).await;

    Ledger::open(dir.path().into()).sweep().await;

    assert_eq!(
        (running(tree.pid).await, running(tree.background).await),
        (true, true)
    );
    assert_eq!(records(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn trees_that_end_normally_leave_no_record() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(dir.path().into());
    // 正常停止：terminate 确认整棵树结束。
    let mut command = sh();
    let mut stopped = ledger
        .spawn(
            &mut command,
            Purpose::Bash,
            Isolation::Session,
            Containment::OwnJob,
        )
        .await
        .unwrap();
    let background = background_of(stopped.child()).await;
    let _cleanup = Cleanup(vec![i32::try_from(stopped.pid()).unwrap()]);
    assert_eq!(records(dir.path()).len(), 1);
    stopped.terminate(Shutdown::Graceful).await.unwrap();
    assert!(ended(background).await);
    // 根进程自行退出、其余进程有意保留（hook）：release 删除记录。
    let mut command = crate::command("/bin/sh");
    command.args(["-c", "sleep 0.2"]).kill_on_drop(true);
    let mut exited = ledger
        .spawn(
            &mut command,
            Purpose::Hook,
            Isolation::Session,
            Containment::ProcessJob,
        )
        .await
        .unwrap();
    assert_eq!(records(dir.path()).len(), 1);
    exited.child().wait().await.unwrap();
    exited.release().await;
    // 进程组（MCP stdio）同样记账。
    let mut command = crate::command("sleep");
    command.arg("30").kill_on_drop(true);
    let mut server = ledger
        .spawn(
            &mut command,
            Purpose::Mcp,
            Isolation::Group,
            Containment::ProcessJob,
        )
        .await
        .unwrap();
    assert_eq!(records(dir.path()).len(), 1);
    server.terminate(Shutdown::Graceful).await.unwrap();

    assert_eq!(records(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn a_tree_left_by_an_ended_instance_of_this_process_is_reaped() {
    let dir = tempfile::tempdir().unwrap();
    // 进程内的 runtime 实例结束时没有收口：记录属于本进程里一个已经注销的实例。
    let tree = tree().await;
    let _cleanup = Cleanup(vec![tree.pid]);
    let owner = this_process().await.unwrap().clone();
    write(dir.path(), identity(tree.pid).await, owner, u64::MAX).await;

    // 同一进程里的下一个实例。
    Ledger::open(dir.path().into()).sweep().await;

    assert_eq!(
        (ended(tree.pid).await, ended(tree.background).await),
        (true, true)
    );
    assert_eq!(records(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn a_tracked_tree_keeps_its_instance_registered() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(dir.path().into());
    let mut command = sh();
    let mut tracked = ledger
        .spawn(
            &mut command,
            Purpose::Bash,
            Isolation::Session,
            Containment::OwnJob,
        )
        .await
        .unwrap();
    let (pid, background) = (
        i32::try_from(tracked.pid()).unwrap(),
        background_of(tracked.child()).await,
    );
    let _cleanup = Cleanup(vec![pid]);
    // 账本的其他克隆都已释放，树仍由 Tracked 持有：它仍属于一个运行中的实例。
    drop(ledger);

    Ledger::open(dir.path().into()).sweep().await;

    assert_eq!(
        (running(pid).await, running(background).await),
        (true, true)
    );
    assert_eq!(records(dir.path()).len(), 1);
    tracked.terminate(Shutdown::Graceful).await.unwrap();
    assert_eq!(records(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn concurrent_sweeps_of_one_directory_agree() {
    let dir = tempfile::tempdir().unwrap();
    let (first, second) = (tree().await, tree().await);
    let _cleanup = Cleanup(vec![first.pid, second.pid]);
    for tree in [&first, &second] {
        write(
            dir.path(),
            identity(tree.pid).await,
            ended_process().await,
            1,
        )
        .await;
    }

    let (a, b) = (
        Ledger::open(dir.path().into()),
        Ledger::open(dir.path().into()),
    );
    tokio::join!(a.sweep(), b.sweep());

    let mut states = vec![];
    for pid in [first.pid, first.background, second.pid, second.background] {
        states.push(ended(pid).await);
    }
    assert_eq!(states, vec![true; 4]);
    assert_eq!(records(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn records_name_their_fields_and_reject_other_names() {
    let this = this_process().await.unwrap().clone();
    let record = Record {
        purpose: Purpose::Mcp,
        root: Process {
            pid: 4242,
            start: Start::parse("19a2b.3c").unwrap(),
        },
        owner: this,
        instance: 7,
    };
    assert_eq!(Record::parse(&record.name()), Some(record.clone()));
    let name = format!(
        "mcp-4242-19a2b.3c-{}-{}-7",
        record.owner.pid,
        record.owner.start.as_str()
    );
    assert_eq!(record.name(), name);
    for other in [
        "tool-4242-1-4243-2-7",
        "mcp-1-1-4243-2-7",
        "mcp-4242-1-4243-2-7-8",
        "mcp-+4242-1-4243-2-7",
        "mcp-04242-1-4243-2-7",
        "mcp-4242-x-4243-2-7",
        ".mcp-4242-1-4243-2-7.tmp",
    ] {
        assert_eq!(Record::parse(other), None, "{other}");
    }
}

#[path = "drop_tests.rs"]
mod drop_tests;
