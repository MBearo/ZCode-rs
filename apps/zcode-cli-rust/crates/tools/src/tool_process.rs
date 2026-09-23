use super::tools::check_cancel;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
pub(super) const INLINE: usize = 24 * 1024;
const MAX_STREAM: u64 = 16 * 1024 * 1024;
struct Captured {
    text: String,
    bytes: u64,
    truncated: bool,
    path: PathBuf,
}
async fn capture(
    mut stream: impl AsyncRead + Unpin,
    path: PathBuf,
    combined: Arc<Mutex<tokio::fs::File>>,
    cancel: CancellationToken,
) -> Result<Captured> {
    let result = async {
        let mut file = tokio::fs::File::create(&path).await?;
        let mut preview = vec![];
        let mut bytes = 0u64;
        let mut buf = [0u8; 8192];
        loop {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let write = n.min(MAX_STREAM.saturating_sub(bytes) as usize);
            if write > 0 {
                file.write_all(&buf[..write]).await?;
                combined.lock().await.write_all(&buf[..write]).await?;
            }
            let take = n.min(INLINE.saturating_sub(preview.len()));
            preview.extend_from_slice(&buf[..take]);
            bytes += n as u64;
            if bytes > MAX_STREAM {
                cancel.cancel();
                break;
            }
        }
        file.flush().await?;
        Ok(Captured {
            text: String::from_utf8_lossy(&preview).into_owned(),
            bytes,
            truncated: bytes > preview.len() as u64,
            path,
        })
    }
    .await;
    // 输出落盘失败必须停止进程树，不能让已丢失输出的后台任务继续运行。
    if result.is_err() {
        cancel.cancel();
    }
    result
}
pub(super) async fn run(
    cwd: &Path,
    text: &str,
    path: &Path,
    combined: Arc<Mutex<tokio::fs::File>>,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
) -> Result<Value> {
    check_cancel(cancel)?;
    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        c.args(["/D", "/S", "/C", text]);
        c
    } else {
        let mut c = Command::new("/bin/bash");
        c.args(["-c", text]);
        c
    };
    command
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().context("Cannot start shell")?;
    let pid = child.id().context("Missing child pid")?;
    let overflow = CancellationToken::new();
    let mut out = tokio::spawn(capture(
        child.stdout.take().unwrap(),
        path.with_extension("stdout"),
        combined.clone(),
        overflow.clone(),
    ));
    let mut err = tokio::spawn(capture(
        child.stderr.take().unwrap(),
        path.with_extension("stderr"),
        combined.clone(),
        overflow.clone(),
    ));
    let timer = async {
        if let Some(t) = timeout {
            tokio::time::sleep(t).await
        } else {
            std::future::pending::<()>().await
        }
    };
    tokio::pin!(timer);
    let (status, mut reason) = tokio::select! {biased;
        _=cancel.cancelled()=>(None,"cancelled"),
        _=overflow.cancelled()=>(None,"failed"),
        _=&mut timer=>(None,"timed_out"),
        result=child.wait()=>(Some(result?),"completed"),
    };
    let captured = async {
        terminate(&mut child, pid, status.is_none()).await?;
        let streams = async { tokio::join!(&mut out, &mut err) };
        tokio::pin!(streams);
        if reason != "completed" {
            // 首次 wait 就被取消时也必须验证管道关闭，不能只在 leader 已退出的分支限时。
            return tokio::time::timeout(Duration::from_secs(1), &mut streams)
                .await
                .context("Bash descendants still hold output after termination");
        }
        // leader exit 不是整个 IO 生命周期结束；仍持有管道的后代不能绕过取消和超时。
        let ready = tokio::select! {biased;
            _=cancel.cancelled(), if reason=="completed"=>{reason="cancelled"; None},
            _=overflow.cancelled(), if reason=="completed"=>{reason="failed"; None},
            _=&mut timer, if reason=="completed"=>{reason="timed_out"; None},
            result=&mut streams=>Some(result),
        };
        if let Some(result) = ready {
            return Ok(result);
        }
        terminate(&mut child, pid, true).await?;
        // 期限只用于报告无法确认回收的错误，绝不把未关闭的 pipe 当作成功。
        tokio::time::timeout(Duration::from_secs(1), &mut streams)
            .await
            .context("Bash descendants still hold output after termination")
    }
    .await
    .context(crate::contract::ProcessCleanupFailure);
    if captured.is_err() {
        out.abort();
        err.abort();
        return Err(captured.err().unwrap());
    }
    let (out, err) = captured.unwrap();
    let out = out??;
    let err = err??;
    combined.lock().await.flush().await?;
    let reason = if reason == "completed" && !status.as_ref().unwrap().success() {
        "failed"
    } else {
        reason
    };
    let mut data = json!({"stdout":out.text,"stderr":err.text,"status":reason,"interrupted":reason=="cancelled"||reason=="timed_out","timedOut":reason=="timed_out","cancelled":reason=="cancelled","stdoutTruncated":out.truncated,"stderrTruncated":err.truncated,"stdoutBytes":out.bytes,"stderrBytes":err.bytes,"persistedOutputPath":path,"stdoutPersistedOutputPath":out.path,"stderrPersistedOutputPath":err.path,"persistedOutputSize":out.bytes.min(MAX_STREAM)+err.bytes.min(MAX_STREAM)});
    if let Some(code) = status.and_then(|s| s.code()) {
        data["exitCode"] = code.into();
    }
    if overflow.is_cancelled() {
        data["stderr"] = format!(
            "{}\nOutput limit exceeded (16 MiB per stream); process stopped",
            data["stderr"].as_str().unwrap()
        )
        .into();
    }
    Ok(data)
}

pub(super) async fn terminate(
    child: &mut tokio::process::Child,
    pid: u32,
    graceful: bool,
) -> Result<()> {
    #[cfg(unix)]
    {
        super::process_tree::terminate(child, pid, graceful).await
    }
    #[cfg(windows)]
    {
        let _ = graceful;
        let result = Command::new("taskkill")
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
}
