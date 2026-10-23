// 复制自 rust/crates/proc/src/ledger/windows_tests.rs（feat/rs 79b6691c55）。
//! Windows：工具进程树在 Job 中（docs/specs/rust-tool-process-reaping.md 2.3）。只在 Windows 上运行
//! （CI 的 Windows job）。

use std::process::Stdio;
use std::time::Duration;

use super::super::{Containment, Isolation, Ledger, Purpose};
use super::*;
use crate::tree::Shutdown;

/// cmd.exe 及其子进程 ping.exe：30 秒内不会自行结束。
fn script() -> tokio::process::Command {
    let mut command = crate::shell::script("ping -n 30 127.0.0.1 > nul");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

#[expect(
    clippy::disallowed_methods,
    reason = "测试：账本路径放在系统临时目录（不会写入）"
)]
async fn spawn(containment: Containment) -> super::super::Tracked {
    let ledger = Ledger::open(std::env::temp_dir().join("zcode-proc-unused-ledger"));
    ledger
        .spawn(
            &mut script(),
            Purpose::Bash,
            Isolation::Session,
            containment,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn an_own_job_ends_the_whole_tree() {
    let mut tracked = spawn(Containment::OwnJob).await;
    assert!(tracked.job.as_ref().unwrap().holds(&tracked.child));
    tracked.terminate(Shutdown::Graceful).await.unwrap();
    let status = tokio::time::timeout(Duration::from_secs(10), tracked.child().wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}

#[tokio::test]
async fn hooks_and_mcp_servers_join_the_process_job() {
    let mut tracked = spawn(Containment::ProcessJob).await;
    assert!(tracked.job.is_none());
    // 进程级 Job 只创建一次；进程退出时系统关闭它的句柄，其中的进程一起结束（与 `Job` 被 drop 相同，
    // 见 job_tests.rs 的 dropping_the_job_ends_the_tree）。
    let job = process_job().unwrap();
    assert!(std::ptr::eq(job, process_job().unwrap()));
    assert!(job.holds(&tracked.child));
    // 结束方式不变：taskkill /T /F。
    tracked.terminate(Shutdown::Graceful).await.unwrap();
    assert!(tracked.child().try_wait().unwrap().is_some());
}
