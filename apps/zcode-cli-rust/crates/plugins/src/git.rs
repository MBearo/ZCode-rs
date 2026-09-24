//! System Git for plugin and marketplace sources (Node `marketplace.ts`
//! `clonePluginSource`, `cloneMarketplaceSource`, `execGitCloneWithRetry`,
//! `execGitCommand`). Spec rust-m10-4-plugin-sources §6.
use crate::store;
use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(90);
const CLONE_MAX_ATTEMPTS: u64 = 3;
const CLONE_RETRY_DELAY_MS: u64 = 1_000;
const MAX_OUTPUT_BYTES: u64 = 10 * 1024 * 1024;
/// Node child_process `AbortError`'s message.
const ABORTED: &str = "The operation was aborted";
const RETRYABLE: [&str; 11] = [
    "rpc failed",
    "operation timed out",
    "recv failure",
    "expected flush",
    "early eof",
    "remote end hung up",
    "http/2 stream",
    "connection reset",
    "etimedout",
    "econnreset",
    "network timeout",
];

/// The Git executable and its environment (Node `buildMarketplaceGitEnv`).
pub struct Git {
    pub binary: String,
    pub env: Vec<(String, String)>,
}

/// A failed Git command with the output Node's retry check reads.
#[derive(Debug)]
struct CommandFailure {
    message: String,
    stdout: String,
}

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandFailure {}

/// Node `isRetryableGitCloneError`.
fn retryable(error: &anyhow::Error) -> bool {
    let output = match error.downcast_ref::<CommandFailure>() {
        Some(failure) => format!("{}\n{}", failure.message, failure.stdout),
        None => error.to_string(),
    }
    .to_lowercase();
    RETRYABLE.iter().any(|needle| output.contains(needle))
}

async fn read_bounded(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> String {
    let mut bytes = vec![];
    if let Some(pipe) = pipe {
        let _ = pipe.take(MAX_OUTPUT_BYTES).read_to_end(&mut bytes).await;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(unix)]
fn terminate(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id().and_then(|p| libc::pid_t::try_from(p).ok()) {
        // 与 Node execFile 的 killSignal: "SIGTERM" 一致。
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

#[cfg(windows)]
fn terminate(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
}

/// Node `execGitCommand`.
async fn run(git: &Git, args: &[String], cancel: &CancellationToken) -> Result<()> {
    crate::failure::check(cancel)?;
    let spawned = tokio::process::Command::new(&git.binary)
        .args(args)
        .env_clear()
        .envs(git.env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let source = args
                .iter()
                .find(|a| a.contains("://") || a.starts_with("git@") || a.starts_with("git+"))
                .or(args.last())
                .map_or("Git operation", String::as_str);
            return Err(crate::failure::git_unavailable(source));
        }
        Err(error) => return Err(error.into()),
    };
    let stdout = tokio::spawn(read_bounded(child.stdout.take()));
    let stderr = tokio::spawn(read_bounded(child.stderr.take()));
    let mut aborted = false;
    let finished = tokio::select! {
        status = child.wait() => Some(status?),
        _ = tokio::time::sleep(COMMAND_TIMEOUT) => None,
        _ = cancel.cancelled() => { aborted = true; None }
    };
    let status = match finished {
        Some(status) => status,
        None => {
            terminate(&mut child);
            child.wait().await?
        }
    };
    let (stdout, stderr) = (stdout.await?, stderr.await?);
    if aborted {
        return Err(anyhow!(ABORTED));
    }
    if !status.success() {
        let command = std::iter::once(git.binary.as_str())
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        return Err(CommandFailure {
            message: format!("Command failed: {command}\n{stderr}"),
            stdout,
        }
        .into());
    }
    crate::failure::check(cancel)
}

/// Node `execGitCloneWithRetry`: only network-shaped clone failures retry.
async fn clone_with_retry(
    git: &Git,
    args: &[String],
    dir: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut attempt = 1;
    loop {
        let result = async {
            crate::failure::check(cancel)?;
            if attempt > 1 {
                store::remove_all(dir).await?;
                tokio::fs::create_dir_all(dir).await?;
            }
            run(git, args, cancel).await
        }
        .await;
        let Err(error) = result else {
            return Ok(());
        };
        if attempt >= CLONE_MAX_ATTEMPTS || !retryable(&error) {
            return Err(error);
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(CLONE_RETRY_DELAY_MS * attempt)) => {}
            _ = cancel.cancelled() => return Err(crate::failure::cancelled()),
        }
        attempt += 1;
    }
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Node `clonePluginSource`: a pinned commit needs full history.
pub async fn clone_plugin(
    git: &Git,
    url: &str,
    reference: Option<&str>,
    pin: Option<&str>,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    let dir = store::temp_dir("zcode-plugin-src-").await?;
    let mut args = vec!["clone".to_owned()];
    if pin.is_none() {
        args.extend(["--depth".into(), "1".into()]);
    }
    if let Some(reference) = reference {
        args.extend(["--branch".into(), reference.into()]);
    }
    args.extend([url.to_owned(), path_arg(&dir)]);
    let result = async {
        clone_with_retry(git, &args, &dir, cancel).await?;
        if let Some(pin) = pin {
            let checkout = ["-C".into(), path_arg(&dir), "checkout".into(), pin.into()];
            run(git, &checkout, cancel).await?;
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => Ok(dir),
        Err(error) => {
            let cleanup = store::cleanup(Some(&dir)).await;
            Err(crate::failure::append_cleanup(error, cleanup))
        }
    }
}

/// Node `cloneMarketplaceSource`: shallow, optionally sparse.
pub async fn clone_marketplace(
    git: &Git,
    url: &str,
    reference: Option<&str>,
    sparse: &[String],
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    let dir = store::temp_dir("zcode-marketplace-src-").await?;
    let mut args: Vec<String> = vec!["clone".into(), "--depth".into(), "1".into()];
    if let Some(reference) = reference {
        args.extend(["--branch".into(), reference.into()]);
    }
    if !sparse.is_empty() {
        args.extend(["--filter=blob:none".into(), "--sparse".into()]);
    }
    args.extend([url.to_owned(), path_arg(&dir)]);
    let result = async {
        clone_with_retry(git, &args, &dir, cancel).await?;
        if !sparse.is_empty() {
            let mut set = vec![
                "-C".into(),
                path_arg(&dir),
                "sparse-checkout".into(),
                "set".into(),
            ];
            set.extend(sparse.iter().cloned());
            run(git, &set, cancel).await?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        store::remove_all(&dir).await?;
        return Err(error);
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_network_failures_retry() {
        let failure = |stderr: &str| -> anyhow::Error {
            CommandFailure {
                message: format!("Command failed: git clone\n{stderr}"),
                stdout: String::new(),
            }
            .into()
        };
        assert!(retryable(&failure(
            "error: RPC failed; curl 56 Recv failure"
        )));
        assert!(retryable(&failure(
            "fatal: the remote end hung up unexpectedly"
        )));
        assert!(!retryable(&failure("fatal: repository 'x' not found")));
        assert!(!retryable(&anyhow!(ABORTED)));
    }

    #[tokio::test]
    async fn a_missing_git_binary_is_reported_with_a_redacted_source() {
        let git = Git {
            binary: "/nonexistent/zcode-git".into(),
            env: vec![],
        };
        let error = clone_plugin(
            &git,
            "https://u:p@example.test/r.git",
            None,
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            crate::failure::classify(&error),
            Some(crate::failure::Failure::Source {
                code: "plugin_git_unavailable",
                ..
            })
        ));
        assert!(
            error
                .to_string()
                .contains("plugin source https://example.test/r.git,")
        );
    }
}
