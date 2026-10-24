//! 经 ssh 在远端跑一条命令（探测、上传）：ssh 进程经 proc 起，stdin 交给它写入的内容，收 stdout、stderr 与退出码。

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::target::SshTarget;

/// 一次远端命令的结果。
pub(crate) struct Output {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }

    /// stderr 的最后几行（错误文字）。
    pub fn tail(&self) -> String {
        let lines: Vec<&str> = self.stderr.lines().collect();
        lines[lines.len().saturating_sub(5)..].join("\n")
    }
}

/// 跑 `remote_command`，把 `input` 写进 ssh 的 stdin（写完关掉）。`limit`：存活的上限（探测给，上传不给）。
pub(crate) async fn run(
    target: &SshTarget,
    remote_command: &str,
    mut input: Box<dyn AsyncRead + Send + Unpin>,
    limit: Option<Duration>,
) -> Result<Output, String> {
    let args = target.args(remote_command)?;
    let mut command = rust_cli_proc::command(&target.program);
    command
        .args(&args)
        .env_clear()
        .envs(target.env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| format!("{}: {error}", target.program.display()))?;
    let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err("ssh pipes are missing".to_owned());
    };
    let feed = rust_cli_base::owned(async move {
        let copied = tokio::io::copy(&mut input, &mut stdin).await;
        let _ = stdin.shutdown().await;
        copied.map(|_| ()).map_err(|error| error.to_string())
    });
    let collect = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (read_out, read_err) =
            tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        read_out.map_err(|error| error.to_string())?;
        read_err.map_err(|error| error.to_string())?;
        let status = child.wait().await.map_err(|error| error.to_string())?;
        Ok::<_, String>(Output {
            status: status.code(),
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
        })
    };
    let output = match limit {
        Some(limit) => tokio::time::timeout(limit, collect).await.map_err(|_| {
            format!(
                "{} did not answer in {} s",
                target.describe(),
                limit.as_secs()
            )
        })??,
        None => collect.await?,
    };
    // 写 stdin 失败（远端提前退出）时，退出码与 stderr 更能说明原因：只在命令本身成功时才报它。
    if let Ok(Err(error)) = feed.await
        && output.success()
    {
        return Err(format!("writing to {}: {error}", target.describe()));
    }
    Ok(output)
}
