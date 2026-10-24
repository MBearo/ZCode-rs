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
