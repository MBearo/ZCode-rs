//! stdio 传输：子进程的 stdin / stdout 当连接；关闭后等子进程退出；异常退出时带上 stderr 的最后几行。
#![cfg(unix)]
#![allow(clippy::disallowed_methods, reason = "测试直接准备进程与文件夹具")]

use std::path::PathBuf;

use bytes::Bytes;

use super::{ChildCommand, StdioConnector};
use crate::link::{Connector, LinkError};

fn connector(script: &str) -> StdioConnector {
    StdioConnector::new(ChildCommand {
        program: PathBuf::from("/bin/sh"),
        args: vec!["-c".to_owned(), script.to_owned()],
        env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
        cwd: None,
        kill_on_drop: false,
    })
}

#[tokio::test]
async fn a_child_echoing_its_stdin_returns_the_same_frames() {
    let link = connector("exec cat").connect().await.unwrap();
    assert!(link.peer().starts_with("stdio:"));
    let (mut tx, mut rx) = link.split();
    tx.send(Bytes::from_static(b"frame one")).await.unwrap();
    assert_eq!(
        rx.recv().await.unwrap(),
        Some(Bytes::from_static(b"frame one"))
    );
    // 关掉 stdin：cat 退出，接收方向在子进程退出后报告结束。
    tx.close().await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), None);
}

#[tokio::test]
async fn a_failing_child_reports_its_stderr() {
    let link = connector("echo 'executor: cannot start' >&2; exit 3")
        .connect()
        .await
        .unwrap();
    let (_tx, mut rx) = link.split();
    let Err(LinkError::Io(message)) = rx.recv().await else {
        panic!("expected an error");
    };
    assert!(message.contains("executor: cannot start"), "{message}");
}

/// 子进程写完 stderr 并退出时，转发 stderr 的任务可能一次都还没读到；接收方向报告的仍是带那一行的错误
/// （remote-exec.md §18.6 的收尾顺序，工作线 F94）。原来子进程一退出就取转发任务攒下的尾巴，尾巴是空的，异常
/// 退出被报告成正常结束：负载下上一个测试不稳定就是这个原因。这里把那个时序固定下来：单线程的运行时被阻塞到
/// 子进程成了僵尸（已经退出、stdout 与 stderr 都已关闭、还没被回收），再收；驱动转一轮之后 `block_on` 的主
/// future 先于派生的转发任务被调度，所以收的一方总是先看到子进程退出。
#[tokio::test]
async fn stderr_is_read_to_its_end_before_the_link_ends() {
    let pid_file = std::env::temp_dir().join(format!("zcode-stdio-tail-{}", std::process::id()));
    let _ = std::fs::remove_file(&pid_file);
    let script = format!(
        "echo $$ > '{}'; echo 'executor: cannot start' >&2; exit 3",
        pid_file.display()
    );
    let link = connector(&script).connect().await.unwrap();
    let (_tx, mut rx) = link.split();
    block_until_exited(&pid_file);
    let _ = std::fs::remove_file(&pid_file);
    let Err(LinkError::Io(message)) = rx.recv().await else {
        panic!("expected an error");
    };
    assert!(message.contains("executor: cannot start"), "{message}");
}

/// 阻塞当前线程（不让出运行时）直到 pid 文件里的进程成了僵尸（`ps` 的状态以 `Z` 开头）。
fn block_until_exited(pid_file: &std::path::Path) {
    for _ in 0..2000 {
        let pid = std::fs::read_to_string(pid_file).unwrap_or_default();
        if pid.ends_with('\n') {
            let state = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", pid.trim()])
                .output()
                .unwrap();
            if String::from_utf8_lossy(&state.stdout)
                .trim_start()
                .starts_with('Z')
            {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the child did not exit");
}

#[tokio::test]
async fn a_missing_program_cannot_connect() {
    let missing = StdioConnector::new(ChildCommand {
        program: PathBuf::from("/nonexistent/zcode-executor"),
        args: Vec::new(),
        env: Vec::new(),
        cwd: None,
        kill_on_drop: false,
    });
    assert!(matches!(
        missing.connect().await.map(|_| ()),
        Err(LinkError::Connect(_))
    ));
}

/// ssh 的连接（`kill_on_drop`）：丢掉连接时子进程被结束，不会一直留着（remote-exec.md §19.2）。
#[tokio::test]
async fn a_dropped_link_kills_the_child_when_asked() {
    let dir = std::env::temp_dir().join(format!("zcode-stdio-{}", std::process::id()));
    let _ = std::fs::remove_file(&dir);
    let script = format!("echo $$ > '{}'; exec sleep 30", dir.display());
    let mut command = ChildCommand {
        program: PathBuf::from("/bin/sh"),
        args: vec!["-c".to_owned(), script],
        env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
        cwd: None,
        kill_on_drop: true,
    };
    command.env.push(("LC_ALL".to_owned(), "C".to_owned()));
    let link = StdioConnector::new(command).connect().await.unwrap();
    let mut pid = String::new();
    for _ in 0..200 {
        pid = std::fs::read_to_string(&dir).unwrap_or_default();
        if pid.ends_with('\n') {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let pid = pid.trim().to_owned();
    let alive = |pid: &str| {
        std::process::Command::new("/bin/kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(&pid));
    drop(link);
    for _ in 0..200 {
        if !alive(&pid) {
            let _ = std::fs::remove_file(&dir);
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the child outlived its link");
}
