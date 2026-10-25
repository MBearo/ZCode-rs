// 复制自 rust/crates/proc/src/tree/posix_tests.rs（feat/rs 79b6691c55）。
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::anyhow;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::*;

/// 隔离启动 `sh -c <script>`；脚本第一行输出后台子进程的 pid。
async fn spawn(script: &str) -> (Child, u32, i32) {
    let mut command = crate::command("/bin/sh");
    command
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    crate::group::isolate(&mut command);
    let mut child = command.spawn().unwrap();
    let pid = child.id().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .await
        .unwrap();
    (child, pid, line.trim().parse().unwrap())
}

/// 失败用例也回收自己创建的进程组，不遗留 30 秒的 sleep。
struct Cleanup(i32);
impl Drop for Cleanup {
    #[expect(unsafe_code, reason = "测试清理：kill(2)")]
    fn drop(&mut self) {
        // SAFETY: kill 只投递信号；目标是本测试创建的进程组。
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

/// 等 `pid` 消失（僵尸由 init 回收前 kill(pid, 0) 仍会成功）。
async fn gone(pid: i32) -> bool {
    tokio::time::timeout(Duration::from_secs(3), async {
        while alive(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

async fn failing() -> Result<Table> {
    Err(anyhow!("injected process lookup failure"))
}

#[tokio::test]
async fn snapshot_failure_still_terminates_the_leader_and_its_descendants() {
    for shutdown in [Shutdown::Graceful, Shutdown::Immediate] {
        let (mut child, pid, background) = spawn("sleep 30 & echo $!; sleep 30").await;
        let _cleanup = Cleanup(pid as i32);
        terminate_with(Root::Child(&mut child), pid, shutdown, &failing)
            .await
            .unwrap();
        assert!(child.try_wait().unwrap().is_some(), "{shutdown:?}: leader");
        assert!(gone(background).await, "{shutdown:?}: background child");
    }
}

#[tokio::test]
async fn snapshot_failure_inside_the_kill_loop_falls_back_to_the_root_group() {
    // TERM 被忽略，宽限期结束后进入 KILL 循环；第一次快照成功，之后的快照全部失败。
    let (mut child, pid, background) = spawn("trap '' TERM; sleep 30 & echo $!; wait").await;
    let _cleanup = Cleanup(pid as i32);
    let calls = AtomicUsize::new(0);
    let flaky = async || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            table::snapshot().await
        } else {
            failing().await
        }
    };
    terminate_with(Root::Child(&mut child), pid, Shutdown::Graceful, &flaky)
        .await
        .unwrap();
    assert!(calls.load(Ordering::SeqCst) >= 2);
    assert!(child.try_wait().unwrap().is_some());
    assert!(gone(background).await);
}

#[tokio::test]
async fn terminates_the_tree_found_in_the_process_table() {
    let (mut child, pid, background) = spawn("sleep 30 & echo $!; wait").await;
    let _cleanup = Cleanup(pid as i32);
    terminate(&mut child, pid, Shutdown::Graceful)
        .await
        .unwrap();
    assert!(child.try_wait().unwrap().is_some());
    assert!(gone(background).await);
}

/// F98（remote-exec.md §25.3）：自己另起会话（进程组）的后代只能经进程表找到：对根进程组发信号够不着它。没有终端时
/// `set -m` 不起作用，所以用 `setsid`（busybox、util-linux 都有；macOS 没有，用 perl 的 `setpgrp`）。旧代码在 alpine（busybox
/// 的 `ps` 不认 `lstart`）与 debian-slim（没有 `/bin/ps`）上取不到进程表，退回对根进程组发信号，这个后代留下来；这台
/// macOS 上 `ps` 可用，旧代码也通过（Docker 里的验证见 remote-exec.md §25.10）。
#[tokio::test]
async fn a_descendant_in_its_own_session_is_terminated_too() {
    let (mut child, pid, background) = spawn(
        "if command -v setsid >/dev/null 2>&1; then setsid sleep 30 & else perl -e 'setpgrp(0, 0); exec @ARGV' sleep 30 & fi; echo $!; wait",
    )
    .await;
    let _cleanup = Cleanup(pid as i32);
    let _escaped = Cleanup(background);
    terminate(&mut child, pid, Shutdown::Graceful)
        .await
        .unwrap();
    assert!(child.try_wait().unwrap().is_some());
    assert!(gone(background).await, "the escaped background job is left");
}
