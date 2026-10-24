//! git 根与规范根（docs/specs/rust-cli/workspace-trust.md §2.1）。只经执行环境的 `stat` 与 `read_file` 判断，不跑 git。

use std::path::{Path, PathBuf};

// 按段化简 `.` 与 `..`（纯词法）：唯一定义在 base（F58：以前这里、cli 的插件目录与 platform 各写一份）。
use rust_cli_base::path::normalize;

use rust_cli_kernel_api::env::{EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_files::{PathStyle, StatFile};
use rust_cli_kernel_api::env_git::{gitdir_pointer, has_git_marker};
use rust_cli_kernel_api::machine::Files;

use super::key::Key;

/// 读 `.git` 等小文件时的路径操作都在本机路径上做（cli 只组装本机执行环境）。
pub(super) struct Probe<'a> {
    pub files: &'a dyn Files,
    pub workspace: &'a WorkspaceRef,
    pub style: PathStyle,
}

/// `.git` 内容给出的路径是 UNC（`\\server\…`、`//server/…`）时不读：Windows 上读网络路径会泄露凭据。
fn remote(path: &str) -> bool {
    path.starts_with("\\\\") || path.starts_with("//")
}

impl Probe<'_> {
    fn env_path(path: &Path) -> EnvPath {
        EnvPath::new(path.to_string_lossy())
    }

    async fn exists(&self, path: &Path) -> bool {
        let request = StatFile {
            path: Self::env_path(path),
        };
        matches!(self.files.stat(self.workspace, request).await, Ok(Some(_)))
    }

    /// 文件的文字（去掉首尾空白）；读不到（包括它是目录）时为 None。
    async fn text(&self, path: &Path) -> Option<String> {
        let request = ReadFile {
            path: Self::env_path(path),
        };
        let content = self.files.read_file(self.workspace, request).await.ok()?;
        Some(String::from_utf8_lossy(&content.bytes).trim().to_owned())
    }

    fn same(&self, a: &Path, b: &Path) -> bool {
        let key = |path: &Path| Key::parse(&path.to_string_lossy(), self.style);
        key(a) == key(b)
    }

    /// 从 `start` 向上第一个含 `.git`（目录或文件）的目录。
    ///
    /// 缺陷修复（F58）：以前 `.git` 是任何种类都算；正确的是只认目录与文件，FIFO 等别的种类继续
    /// 往上。判定改用 kernel-api 的 `has_git_marker`（唯一定义）。
    pub(super) async fn git_root(&self, start: &Path) -> Option<PathBuf> {
        for dir in start.ancestors() {
            let dot_git = dir.join(".git");
            if has_git_marker(self.files, self.workspace, &dot_git.to_string_lossy()).await {
                return Some(dir.to_path_buf());
            }
        }
        None
    }

    /// git 根是 linked worktree 时它的主仓库根，否则 git 根本身。任何一步不符合都用 git 根。
    pub(super) async fn canonical_root(&self, root: &Path) -> PathBuf {
        self.worktree_main(root)
            .await
            .unwrap_or_else(|| root.to_path_buf())
    }

    async fn worktree_main(&self, root: &Path) -> Option<PathBuf> {
        let dot_git = root.join(".git");
        let text = self.text(&dot_git).await?;
        // `gitdir:` 的解析与 UNC 的拒绝在 kernel-api（F58：以前 context、cli、runtime 各写一份）。
        let gitdir = normalize(&root.join(gitdir_pointer(&text)?));
        let common_text = self.text(&gitdir.join("commondir")).await?;
        if remote(&common_text) {
            return None;
        }
        let common = normalize(&gitdir.join(&common_text));
        if !self.same(gitdir.parent()?, &common.join("worktrees")) {
            return None;
        }
        // `<gitdir>/gitdir` 要指回这个 worktree 的 `.git`（按规范化后的路径比较，不用 realpath，D4）。
        let back = self.text(&gitdir.join("gitdir")).await?;
        if remote(&back) || !self.same(&normalize(&gitdir.join(&back)), &dot_git) {
            return None;
        }
        if common.file_name().is_some_and(|name| name == ".git") {
            return common.parent().map(Path::to_path_buf);
        }
        // bare 仓库：公共目录本身是仓库；它下面还有 `.git` 时认不出，用 git 根。
        if self.exists(&common.join(".git")).await {
            return None;
        }
        Some(common)
    }
}
