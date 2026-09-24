//! Hook processes (Node `ExecutionPort.run` for a hook request): transcript
//! file, stdin, per-stream capped output and process-tree cleanup.
use crate::contract::{HookProcess, ProcessCleanupFailure};
use crate::domain::hooks::{Program, Shell, input, output::Exec};
use anyhow::{Context, Result, bail};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

/// Node `mkdtemp(tmpdir()/zcode-hook-)`, removed after the run.
struct TempDir(Option<PathBuf>);

impl TempDir {
    async fn new() -> Result<Self> {
        let base = std::env::temp_dir();
        for _ in 0..8 {
            let path = base.join(format!("zcode-hook-{}", crate::id()));
            match tokio::fs::create_dir(&path).await {
                Ok(()) => return Ok(Self(Some(path))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("Cannot create hook temp directory"),
            }
        }
        bail!("Cannot create hook temp directory")
    }
    fn path(&self) -> &std::path::Path {
        self.0.as_deref().unwrap()
    }
    async fn remove(mut self) {
        if let Some(path) = self.0.take() {
            let _ = tokio::fs::remove_dir_all(path).await;
        }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // run 被取消（future 被丢弃）时仍要删除临时目录；只在异步清理未执行时兜底。
        if let Some(path) = self.0.take()
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn(async move {
                let _ = tokio::fs::remove_dir_all(path).await;
            });
        }
    }
}

pub(super) async fn run(
    request: HookProcess,
    env: &[(String, String)],
    cancel: &CancellationToken,
) -> Result<Exec> {
    let dir = TempDir::new().await?;
    let transcript = dir.path().join("transcript.jsonl");
    tokio::fs::write(&transcript, input::transcript(&request.input))
        .await
        .context("Cannot write hook transcript")?;
    let stdin = input::stdin(&request.input, &transcript.to_string_lossy());
    let result = execute(&request, env, stdin, cancel).await;
    dir.remove().await;
    result
}

#[cfg(unix)]
fn shell_command(command: &str, shell: &Shell) -> Command {
    // Node `spawn(cmd, [], {shell})`：`shell: true` 在 POSIX 上为 /bin/sh。
    let program = match shell {
        Shell::Path(path) => path.as_str(),
        _ => "/bin/sh",
    };
    let mut c = Command::new(program);
    c.args(["-c", command]);
    c
}

#[cfg(windows)]
fn shell_command(command: &str, shell: &Shell) -> Command {
    let program = match shell {
        Shell::Path(path) => path.clone(),
        _ => std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into()),
    };
    let mut c = Command::new(&program);
    let lower = program.to_ascii_lowercase();
    if lower.ends_with("cmd.exe") || lower == "cmd" {
        c.raw_arg(format!("/d /s /c \"{command}\""));
    } else {
        c.args(["-c", command]);
    }
    c
}

fn command(program: &Program) -> Command {
    match program {
        Program::Command { command, shell, .. } => shell_command(command, shell),
        Program::Process { command, args } => {
            let mut c = Command::new(command);
            c.args(args);
            c
        }
    }
}

/// Node `spawn <file> <CODE>` start failure text.
fn spawn_error(program: &Program, error: &std::io::Error) -> String {
    let file = match program {
        Program::Command { .. } => "shell",
        Program::Process { command, .. } => command.as_str(),
    };
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT".to_owned(),
        std::io::ErrorKind::PermissionDenied => "EACCES".to_owned(),
        _ => error.to_string(),
    };
    format!("spawn {file} {code}")
}

/// Keeps the first `limit` bytes; the rest is read and dropped so the child
/// never blocks on a full pipe (Node output collector, no kill).
async fn capture(mut stream: impl AsyncRead + Unpin, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut kept = vec![];
    let mut buf = [0u8; 8192];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(kept);
        }
        let room = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(room)]);
    }
}

fn exec(status: &str, error: Option<String>) -> Exec {
    Exec {
        status: status.into(),
        error,
        ..Exec::default()
    }
}

async fn execute(
    request: &HookProcess,
    env: &[(String, String)],
    stdin: String,
    cancel: &CancellationToken,
) -> Result<Exec> {
    if cancel.is_cancelled() {
        return Ok(exec("cancelled", None));
    }
    let mut command = command(&request.program);
    command
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .envs(request.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Ok(exec(
                "spawn_error",
                Some(spawn_error(&request.program, &error)),
            ));
        }
    };
    let pid = child.id().context("Missing hook pid")?;
    let mut pipe = child.stdin.take().context("Missing hook stdin")?;
    let writer = tokio::spawn(async move {
        let written = pipe.write_all(stdin.as_bytes()).await;
        drop(pipe);
        written
    });
    let limit = request.max_output_bytes;
    let mut out = tokio::spawn(capture(child.stdout.take().unwrap(), limit));
    let mut err = tokio::spawn(capture(child.stderr.take().unwrap(), limit));
    let timer = tokio::time::sleep(request.timeout);
    tokio::pin!(timer);
    let (exit, mut reason) = tokio::select! {biased;
        _=cancel.cancelled()=>(None,"cancelled"),
        _=&mut timer=>(None,"timed_out"),
        status=child.wait()=>(Some(status?),"completed"),
    };
    let drained = async {
        let streams = async { tokio::join!(&mut out, &mut err) };
        tokio::pin!(streams);
        if reason == "completed" {
            // 根进程退出后最多再等 1 s 管道关闭（Node process.ts），仍被后代占用则回收进程树。
            let ready = tokio::select! {biased;
                _=cancel.cancelled()=>{reason="cancelled"; None},
                _=&mut timer=>{reason="timed_out"; None},
                _=tokio::time::sleep(Duration::from_secs(1))=>None,
                result=&mut streams=>Some(result),
            };
            if let Some(result) = ready {
                return Ok(result);
            }
        }
        super::tool_process::terminate(&mut child, pid, true).await?;
        tokio::time::timeout(Duration::from_secs(1), &mut streams)
            .await
            .context("Hook descendants still hold output after termination")
    }
    .await
    .context(ProcessCleanupFailure);
    writer.abort();
    let (out, err) = match drained {
        Ok(streams) => streams,
        Err(error) => {
            out.abort();
            err.abort();
            return Err(error);
        }
    };
    let text = |captured: std::result::Result<std::io::Result<Vec<u8>>, _>| -> String {
        match captured {
            Ok(Ok(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
            _ => String::new(),
        }
    };
    let exit_code = exit.and_then(|status| status.code());
    let mut status = match (reason, exit_code) {
        ("completed", Some(0)) => "completed",
        ("completed", _) => "failed",
        (other, _) => other,
    };
    let mut error = None;
    if status == "completed"
        && let Ok(Err(failure)) = writer.await
        && failure.kind() != std::io::ErrorKind::BrokenPipe
    {
        // 与 Node 一致：子进程提前关闭 stdin（EPIPE）不算失败，其他写入错误才算。
        status = "failed";
        error = Some(failure.to_string());
    }
    Ok(Exec {
        status: status.into(),
        exit_code: if reason == "completed" {
            exit_code
        } else {
            None
        },
        stdout: text(out),
        stderr: text(err),
        error,
    })
}

#[cfg(all(test, unix))]
#[path = "hook_process_tests.rs"]
mod tests;
