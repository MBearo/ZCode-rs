use crate::{
    contract::ContextPort,
    domain::prompt::{InstructionSource, PromptSnapshot},
};
use anyhow::Result;
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

pub struct WorkspaceContext {
    cwd: PathBuf,
    home: PathBuf,
    desktop: bool,
}
impl WorkspaceContext {
    pub fn new(cwd: PathBuf, home: PathBuf, desktop: bool) -> Self {
        Self { cwd, home, desktop }
    }
}
#[async_trait::async_trait]
impl ContextPort for WorkspaceContext {
    fn desktop(&self) -> bool {
        self.desktop
    }
    async fn snapshot(&self, cancel: &CancellationToken) -> Result<PromptSnapshot> {
        let platform = match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            other => other,
        };
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "x64",
            other => other,
        };
        let (git, release) = tokio::join!(
            super::context_git::snapshot(&self.cwd, cancel),
            super::context_git::os_release(&self.cwd, cancel),
        );
        super::tools::check_cancel(cancel)?;
        Ok(PromptSnapshot {
            cwd: self.cwd.to_string_lossy().into_owned(),
            platform: platform.into(),
            shell: if cfg!(windows) { "cmd.exe" } else { "bash" }.into(),
            os_version: format!("{platform} {} {arch}", release.trim()),
            current_date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            git,
        })
    }
    async fn instructions(&self, cancel: &CancellationToken) -> Result<Vec<InstructionSource>> {
        tokio::select! { biased;
            _=cancel.cancelled()=>anyhow::bail!("Cancelled"),
            sources=read_instructions(&self.cwd, &self.home)=>Ok(sources),
        }
    }
}
async fn read_instructions(cwd: &Path, home: &Path) -> Vec<InstructionSource> {
    // TS 只选择最近文件，并在最近 Git 根停止；不能把父目录或隐藏候选全量拼接进请求。
    let mut candidates = vec![(home.join(".zcode/AGENTS.md"), true)];
    for directory in cwd.ancestors() {
        let candidate = directory.join("AGENTS.md");
        if tokio::fs::metadata(&candidate)
            .await
            .is_ok_and(|m| m.is_file())
        {
            if candidate != candidates[0].0 {
                candidates.push((candidate, false));
            }
            break;
        }
        if tokio::fs::metadata(directory.join(".git")).await.is_ok() {
            break;
        }
    }
    let mut sources = vec![];
    for (path, user) in candidates {
        if let Ok(source) = read_source(&path, user).await {
            sources.push(source);
        }
    }
    sources
}
async fn read_source(path: &Path, user: bool) -> Result<InstructionSource> {
    const MAX_BYTES: u64 = 100 * 1024;
    let meta = tokio::fs::metadata(path).await?;
    anyhow::ensure!(meta.is_file(), "Instructions must be a regular file");
    let file = tokio::fs::File::open(path).await?;
    let mut bytes = Vec::with_capacity(meta.len().min(MAX_BYTES) as usize);
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).await?;
    let truncated = meta.len() > MAX_BYTES || bytes.len() > MAX_BYTES as usize;
    bytes.truncate(MAX_BYTES as usize);
    Ok(InstructionSource {
        path: path.to_string_lossy().into_owned(),
        user,
        content: String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn same_user_and_workspace_path_is_injected_once_and_non_files_are_skipped() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join(".zcode");
        tokio::fs::create_dir_all(cwd.join(".git")).await.unwrap();
        tokio::fs::write(cwd.join("AGENTS.md"), "one source")
            .await
            .unwrap();
        let sources = read_instructions(&cwd, root.path()).await;
        assert_eq!(sources.len(), 1);
        assert!(sources[0].user);
        assert_eq!(sources[0].content, "one source");
        tokio::fs::remove_file(cwd.join("AGENTS.md")).await.unwrap();
        tokio::fs::create_dir(cwd.join("AGENTS.md")).await.unwrap();
        tokio::fs::write(root.path().join("AGENTS.md"), "outside git boundary")
            .await
            .unwrap();
        assert!(read_instructions(&cwd, root.path()).await.is_empty());
    }
    #[tokio::test]
    async fn cancelled_context_initialization_does_not_return_a_snapshot_or_sources() {
        let root = tempfile::tempdir().unwrap();
        let source = WorkspaceContext::new(root.path().into(), root.path().into(), false);
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(source.snapshot(&cancel).await.is_err());
        assert!(source.instructions(&cancel).await.is_err());
    }
}
