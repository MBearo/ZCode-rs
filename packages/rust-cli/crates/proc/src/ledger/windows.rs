// 复制自 rust/crates/proc/src/ledger/windows.rs（feat/rs 79b6691c55）。
//! Windows：工具进程树放进设置了 `KILL_ON_JOB_CLOSE` 的 Job，句柄由本进程持有，本进程退出时
//! （正常退出或崩溃）系统关闭句柄，整棵树一起结束（docs/specs/rust-tool-process-reaping.md 2.3）。

use std::sync::OnceLock;

use anyhow::{Context, Result};
use tokio::process::Child;

use super::{Containment, Purpose};
use crate::job::Job;

/// 把刚启动的 `child` 放进 Job。返回只属于它的 Job（[`Containment::OwnJob`]）；其余情况放进
/// 进程级 Job，返回 `None`，结束时用 taskkill。加入失败只记日志：进程照常运行，只是不随本进程结束。
pub(super) fn contain(
    child: &Child,
    (pid, purpose): (u32, Purpose),
    containment: Containment,
) -> Option<Job> {
    let purpose = purpose.as_str();
    match containment {
        Containment::OwnJob => match Job::new().and_then(|job| job.assign(child).map(|()| job)) {
            Ok(job) => return Some(job),
            Err(error) => tracing::warn!(
                pid,
                purpose,
                %error,
                "cannot contain the process in its own Job; using the process Job and taskkill"
            ),
        },
        Containment::ProcessJob => {}
    }
    if let Some(job) = process_job()
        && let Err(error) = job.assign(child)
    {
        tracing::warn!(
            pid,
            purpose,
            %error,
            "cannot add the process to the process Job; it will outlive this process"
        );
    }
    None
}

/// 进程级 Job：首次使用时创建，存放在静态量里，进程退出前不关闭。
///
/// hooks 与 MCP stdio 服务结束时不结束整个 Job（hook 正常结束后有意留下的后台进程继续运行）；
/// 只有本进程退出时，系统关闭句柄，其中的进程才一起结束。
fn process_job() -> Option<&'static Job> {
    static JOB: OnceLock<Option<Job>> = OnceLock::new();
    JOB.get_or_init(|| match Job::new() {
        Ok(job) => Some(job),
        Err(error) => {
            tracing::warn!(%error, "cannot create the process Job");
            None
        }
    })
    .as_ref()
}

/// 结束整棵树并回收 `child`；没有 TERM 阶段。有自己的 Job 时结束 Job（根进程退出后 taskkill /T
/// 已无法沿父进程找到后代；Job 不依赖父子关系），否则 `taskkill /T /F`。
pub(super) async fn terminate(child: &mut Child, pid: u32, job: Option<&Job>) -> Result<()> {
    if let Some(job) = job {
        job.terminate()
            .context("Cannot terminate the process job")?;
        if child.try_wait()?.is_none() {
            child.wait().await?;
        }
        return Ok(());
    }
    let result = crate::command("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status()
        .await;
    if child.try_wait()?.is_none() {
        anyhow::ensure!(result?.success(), "Bash taskkill failed");
        child.wait().await?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
