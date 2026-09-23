use crate::domain::prompt::GitSnapshot;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

// 只用于环境探测，输出与进程时间都有界；drop 也回收探测进程组，避免 Stop 留下 fsmonitor 等子进程。
#[cfg(unix)]
struct Group(u32);
#[cfg(unix)]
impl Drop for Group {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}
async fn command(
    cwd: &Path,
    program: &str,
    args: &[&str],
    cancel: &CancellationToken,
) -> Option<String> {
    if cancel.is_cancelled() {
        return None;
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().ok()?;
    #[cfg(unix)]
    let _group = Group(child.id()?);
    #[cfg(windows)]
    let pid = child.id()?;
    let mut stdout = child.stdout.take()?.take(1024 * 1024 + 1);
    let read = async {
        let mut bytes = vec![];
        stdout.read_to_end(&mut bytes).await.ok()?;
        if bytes.len() > 1024 * 1024 || !child.wait().await.ok()?.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    };
    let result = tokio::select! { biased;
        _=cancel.cancelled()=>None,
        _=tokio::time::sleep(Duration::from_secs(3))=>None,
        result=read=>result,
    };
    #[cfg(windows)]
    if child.try_wait().ok().flatten().is_none() {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await;
        let _ = child.kill().await;
    }
    result
}
async fn git(cwd: &Path, args: &[&str], cancel: &CancellationToken) -> String {
    command(cwd, "git", args, cancel)
        .await
        .unwrap_or_default()
        .trim()
        .into()
}
async fn may_have_git(cwd: &Path, cancel: &CancellationToken) -> bool {
    if std::env::var_os("GIT_DIR").is_some() || std::env::var_os("GIT_WORK_TREE").is_some() {
        return true;
    }
    let cwd = match tokio::fs::canonicalize(cwd).await {
        Ok(path) => path,
        Err(_) => return true,
    };
    for dir in cwd.ancestors() {
        if cancel.is_cancelled() {
            return false;
        }
        match tokio::fs::symlink_metadata(dir.join(".git")).await {
            Ok(_) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return true,
        }
    }
    false
}
pub(super) async fn snapshot(cwd: &Path, cancel: &CancellationToken) -> Option<GitSnapshot> {
    // macOS 的系统 git 启动成本显著；不存在仓库标记时无需启动探测进程。显式 Git 环境仍交给 Git。
    if !may_have_git(cwd, cancel).await {
        return None;
    }
    if git(cwd, &["rev-parse", "--is-inside-work-tree"], cancel).await != "true" {
        return None;
    }
    let (branch, main_branch, user, status, recent) = tokio::join!(
        git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"], cancel),
        main_branch(cwd, cancel),
        git(cwd, &["config", "user.name"], cancel),
        git(cwd, &["--no-optional-locks", "status", "--short"], cancel),
        git(
            cwd,
            &["--no-optional-locks", "log", "--oneline", "-n", "5"],
            cancel
        ),
    );
    let status = if status.encode_utf16().count() > 2000 {
        let units: Vec<_> = status.encode_utf16().take(2000).collect();
        format!(
            "{}\n... (truncated because it exceeds 2k characters. If you need more information, run \"git status\" using {})",
            String::from_utf16_lossy(&units),
            if cfg!(windows) { "PowerShell" } else { "Bash" }
        )
    } else {
        status
    };
    Some(GitSnapshot {
        branch: if branch.is_empty() {
            "HEAD".into()
        } else {
            branch
        },
        main_branch,
        user,
        status: status.replace("\r\n", "\n"),
        recent_commits: recent
            .lines()
            .filter(|line| !line.is_empty())
            .take(5)
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n"),
    })
}
async fn main_branch(cwd: &Path, cancel: &CancellationToken) -> String {
    let remote = git(
        cwd,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        cancel,
    )
    .await;
    for name in [
        remote.strip_prefix("origin/").unwrap_or(&remote),
        "main",
        "master",
    ] {
        if !name.is_empty()
            && command(
                cwd,
                "git",
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/remotes/origin/{name}"),
                ],
                cancel,
            )
            .await
            .is_some()
        {
            return name.into();
        }
    }
    "main".into()
}
pub(super) async fn os_release(cwd: &Path, cancel: &CancellationToken) -> String {
    #[cfg(not(windows))]
    let result = command(cwd, "uname", &["-r"], cancel).await;
    #[cfg(windows)]
    let result = command(
        cwd,
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::OSVersion.Version.ToString()",
        ],
        cancel,
    )
    .await;
    result.unwrap_or_else(|| "unknown".into())
}
