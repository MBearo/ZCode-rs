// 复制自 rust/crates/proc/src/group_tests.rs（feat/rs 79b6691c55）。
use super::*;

#[tokio::test]
#[expect(unsafe_code, reason = "getsid / getpgid 只查询子进程的会话与进程组")]
async fn isolated_children_lead_a_new_session_and_group() {
    let mut command = crate::command("sleep");
    command.arg("30").kill_on_drop(true);
    isolate(&mut command);
    let mut child = command.spawn().unwrap();
    let pid = libc::pid_t::try_from(child.id().unwrap()).unwrap();
    // SAFETY: getsid 与 getpgid 只读取内核中的进程信息；pid 是尚未回收的子进程。
    let (session, group) = unsafe { (libc::getsid(pid), libc::getpgid(pid)) };
    child.kill().await.unwrap();
    // 新会话的首进程没有控制终端：读取 /dev/tty 的提示（ssh、sudo、git 凭据）立即失败，不会挂起。
    assert_eq!((session, group), (pid, pid));
}
