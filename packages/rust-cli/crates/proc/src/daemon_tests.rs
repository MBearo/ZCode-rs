//! 常驻进程自成会话：setsid 之后它是新会话与新进程组的首进程，进程组 ID 等于自己的 pid（macOS 的 ps 没有 sid，看 pgid）。

use std::process::Stdio;

use tokio::io::AsyncReadExt;

#[tokio::test]
async fn the_daemon_leads_its_own_session() {
    let mut command = crate::command("/bin/sh");
    command
        .args(["-c", "ps -o pgid= -p $$; echo $$"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    let mut child = super::spawn(&mut command).unwrap();
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .await
        .unwrap();
    child.wait().await.unwrap();
    let lines: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert_eq!(
        lines[0], lines[1],
        "process group (setsid makes it the session and group leader) is the daemon's own pid"
    );
}
