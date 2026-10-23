// 复制自 rust/crates/proc/src/job_tests.rs（feat/rs 79b6691c55）。
use std::process::Stdio;
use std::time::Duration;

use windows_sys::Win32::System::JobObjects::{
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
    QueryInformationJobObject,
};

use super::*;

/// cmd.exe 及其子进程 ping.exe：一棵两层的进程树，30 秒内不会自行结束。
fn spawn_tree(job: &Job) -> Child {
    let mut command = crate::shell::script("ping -n 30 127.0.0.1 > nul");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = command.spawn().unwrap();
    job.assign(&child).unwrap();
    child
}

#[expect(unsafe_code, reason = "测试查询 Job 中的进程数")]
fn active_processes(job: &Job) -> u32 {
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    let size = u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>()).unwrap();
    // SAFETY: 句柄有效；输出缓冲区与信息类别匹配，长度一致。
    let queried = unsafe {
        QueryInformationJobObject(
            job.handle.as_raw_handle(),
            JobObjectBasicAccountingInformation,
            (&raw mut info).cast(),
            size,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(queried, 0, "{}", io::Error::last_os_error());
    info.ActiveProcesses
}

async fn until(condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn terminate_ends_every_process_in_the_tree() {
    let job = Job::new().unwrap();
    let mut child = spawn_tree(&job);
    // cmd.exe 与它启动的 ping.exe 都在 Job 中。
    until(|| active_processes(&job) >= 2).await;
    job.terminate().unwrap();
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(1));
    until(|| active_processes(&job) == 0).await;
    // 已经没有进程时再次结束同样成功。
    job.terminate().unwrap();
}

#[tokio::test]
async fn dropping_the_job_ends_the_tree() {
    let job = Job::new().unwrap();
    let mut child = spawn_tree(&job);
    until(|| active_processes(&job) >= 2).await;
    drop(job);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
}

#[tokio::test]
async fn assigning_a_reaped_child_fails() {
    let job = Job::new().unwrap();
    let mut child = crate::shell::script("exit 0").spawn().unwrap();
    child.wait().await.unwrap();
    assert_eq!(
        job.assign(&child).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}
