//! 内存文件系统的执行环境替身：规则文件、条件规则与 git 状态的测试用它。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};

use crate::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, FileVersion, OperationContext, Outcome,
    ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use crate::env_files::{
    DirEntry, FileKind, FileList, FileStat, FileWritten, ListDir, ListFiles, PathFacts, PathStyle,
    PutFile, ReadText, StatFile, StatMany, TextRead, TextSlice,
};
use crate::env_git::{GitStatus, GitStatusRequest};
use crate::env_rewind::{RewindPlan, RewindPrepare, RewindStatus};
use crate::env_shell::ShellState;
use crate::ids::{OperationId, SessionId};
use crate::local_services::{Fetch, Git};
use crate::machine::{Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell};
use crate::testing::lock;
use crate::testing::memory_rewind::{self, Transactions};

/// 文件表里的路径就是全部文件；某个路径加分隔符是其他文件的前缀时，它是目录。分隔符看路径本身：
/// 含 `\` 的按 Windows，否则按 `/`。`git` 为 None 时 `git_status` 返回 `Unsupported`。
pub struct MemoryEnvironment {
    pub facts: EnvFacts,
    files: Mutex<BTreeMap<String, Vec<u8>>>,
    /// 既不是文件也不是目录的路径（FIFO、socket、设备）：`stat` 给 `FileKind::Other`，`read_file` 失败。
    specials: Mutex<BTreeSet<String>>,
    git: Option<GitStatus>,
    /// `git_diff` 的结果（`/diff`）；None 时返回 `Unsupported`。
    diff: Option<crate::env_git::GitDiff>,
    reads: AtomicUsize,
    /// `stat` 与 `stat_many` 的调用次数（R3：远端每步只发一次批量 stat，remote-exec.md §20.7.4）。
    stats: AtomicUsize,
    stat_batches: AtomicUsize,
    /// shell 的当前目录（`shell_state`）；没有设置时是工作区根。
    shell_cwd: Mutex<Option<EnvPath>>,
    /// 代码回退事务（与文件表分开加锁，不跨 await 持有）。
    rewinds: Mutex<Transactions>,
    /// 下一次 `rewind_apply` 失败（R3：跨机器回退时一台 apply 中途失败）。
    fail_apply: std::sync::atomic::AtomicBool,
    /// `rewind_finish` 收到的 token，按顺序。
    finished: Mutex<Vec<OperationId>>,
    /// `background_foreground` 收到的原因，按顺序（修复线 F105：低延迟投递转不转后台）。没有前台命令，转了 0 个。
    moved: Mutex<Vec<crate::env_command::BackgroundReason>>,
}

fn separator(path: &str) -> char {
    if path.contains('\\') { '\\' } else { '/' }
}

fn child_prefix(dir: &str) -> String {
    let sep = separator(dir);
    let trimmed = dir.trim_end_matches(sep);
    format!("{trimmed}{sep}")
}

fn missing(path: &str) -> EnvError {
    EnvError::NotFound {
        path: path.to_owned(),
    }
}

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "memory environment".to_owned(),
    }
}

impl MemoryEnvironment {
    pub fn new(facts: EnvFacts) -> Self {
        Self {
            facts,
            files: Mutex::default(),
            specials: Mutex::default(),
            git: None,
            diff: None,
            reads: AtomicUsize::new(0),
            stats: AtomicUsize::new(0),
            stat_batches: AtomicUsize::new(0),
            fail_apply: std::sync::atomic::AtomicBool::new(false),
            finished: Mutex::default(),
            shell_cwd: Mutex::default(),
            rewinds: Mutex::default(),
            moved: Mutex::default(),
        }
    }

    /// `background_foreground` 收到的原因，按顺序。
    pub fn moved(&self) -> Vec<crate::env_command::BackgroundReason> {
        lock(&self.moved).clone()
    }

    #[must_use]
    pub fn with_file(self, path: &str, text: &str) -> Self {
        self.set_file(path, text);
        self
    }

    /// 一个既不是文件也不是目录的路径（FIFO、socket、设备）：只影响 `stat`（`FileKind::Other`）与 `read_file`
    /// （失败），不让上层成为目录，也不出现在 `list_dir` 里。
    #[must_use]
    pub fn with_special(self, path: &str) -> Self {
        lock(&self.specials).insert(path.to_owned());
        self
    }

    #[must_use]
    pub fn with_git(mut self, status: GitStatus) -> Self {
        self.git = Some(status);
        self
    }

    /// `/diff` 的未提交改动（docs/specs/rust-cli/tui-manage.md §4）。
    #[must_use]
    pub fn with_git_diff(mut self, diff: crate::env_git::GitDiff) -> Self {
        self.diff = Some(diff);
        self
    }

    /// Bash `cd` 之后 shell 的当前目录。
    pub fn set_shell_cwd(&self, cwd: &str) {
        *lock(&self.shell_cwd) = Some(EnvPath::new(cwd));
    }

    /// 会话中途改写或新建文件。
    /// 下一次 `rewind_apply` 返回失败（不改任何文件）。
    pub fn fail_next_rewind_apply(&self) {
        self.fail_apply.store(true, Ordering::Relaxed);
    }

    /// `rewind_finish` 收到的 token，按顺序。
    pub fn rewinds_finished(&self) -> Vec<OperationId> {
        lock(&self.finished).clone()
    }

    /// `stat` 与 `stat_many` 各被调用了几次（`stat_many` 里的逐项不算进 `stat`）。
    pub fn stat_calls(&self) -> (usize, usize) {
        (
            self.stats.load(Ordering::Relaxed),
            self.stat_batches.load(Ordering::Relaxed),
        )
    }

    pub fn set_file(&self, path: &str, text: &str) {
        lock(&self.files).insert(path.to_owned(), text.as_bytes().to_vec());
    }

    pub fn remove_file(&self, path: &str) {
        lock(&self.files).remove(path);
    }

    /// `read_file` 被调用的次数。
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }

    fn is_dir(&self, path: &str) -> bool {
        let prefix = child_prefix(path);
        lock(&self.files).keys().any(|key| key.starts_with(&prefix))
    }
}

#[async_trait]
impl EnvInfo for MemoryEnvironment {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Ok(self.facts.clone())
    }

    /// home 取自环境事实，路径风格按 platform（`win32` 为 Windows）。
    async fn path_facts(&self, _workspace: &WorkspaceRef) -> Result<PathFacts, EnvError> {
        let style = if self.facts.platform == "win32" {
            PathStyle::Windows
        } else {
            PathStyle::Posix
        };
        Ok(PathFacts {
            home: self.facts.home.clone(),
            style,
        })
    }
}

#[async_trait]
impl Files for MemoryEnvironment {
    async fn read_file(
        &self,
        _workspace: &WorkspaceRef,
        request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let path = request.path.as_str();
        if lock(&self.specials).contains(path) {
            return Err(EnvError::Io {
                message: format!("{path} is not a regular file"),
            });
        }
        let bytes = lock(&self.files).get(path).cloned();
        match bytes {
            Some(bytes) => Ok(FileContent {
                version: memory_rewind::version(&bytes),
                bytes,
            }),
            None if self.is_dir(path) => Err(EnvError::Io {
                message: format!("{path} is a directory"),
            }),
            None => Err(missing(path)),
        }
    }

    async fn write_file(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        Err(unused())
    }

    async fn stat_many(
        &self,
        workspace: &WorkspaceRef,
        request: StatMany,
    ) -> Result<Vec<Result<Option<FileStat>, EnvError>>, EnvError> {
        self.stat_batches.fetch_add(1, Ordering::Relaxed);
        let mut out = Vec::new();
        for path in request.paths {
            out.push(self.stat_one(workspace, StatFile { path }));
        }
        Ok(out)
    }

    async fn stat(
        &self,
        workspace: &WorkspaceRef,
        request: StatFile,
    ) -> Result<Option<FileStat>, EnvError> {
        self.stats.fetch_add(1, Ordering::Relaxed);
        self.stat_one(workspace, request)
    }
    async fn list_dir(
        &self,
        _workspace: &WorkspaceRef,
        request: ListDir,
    ) -> Result<Vec<DirEntry>, EnvError> {
        let path = request.path.as_str();
        let prefix = child_prefix(path);
        let sep = separator(path);
        let mut entries: Vec<DirEntry> = Vec::new();
        for key in lock(&self.files).keys() {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            let (name, kind) = match rest.split_once(sep) {
                Some((dir, _)) => (dir, FileKind::Directory),
                None => (rest, FileKind::File),
            };
            if entries.last().is_none_or(|entry| entry.name != name) {
                entries.push(DirEntry {
                    name: name.to_owned(),
                    kind,
                });
            }
        }
        if entries.is_empty() {
            return Err(missing(path));
        }
        Ok(entries)
    }

    /// 按行读文本（Read 工具用）：只做 UTF-8 解码与按 `\n` 切行，不处理 BOM、编码与 CRLF。
    async fn read_text(
        &self,
        _workspace: &WorkspaceRef,
        request: ReadText,
    ) -> Result<TextRead, EnvError> {
        let path = request.path.as_str();
        let Some(bytes) = lock(&self.files).get(path).cloned() else {
            return if self.is_dir(path) {
                Ok(TextRead::Directory)
            } else {
                Err(missing(path))
            };
        };
        let size = bytes.len() as u64;
        if request.max_bytes.is_some_and(|max| size > max) {
            return Ok(TextRead::TooLarge { size });
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let lines: Vec<&str> = text.split('\n').collect();
        let start = usize::try_from(request.start_line).unwrap_or(usize::MAX);
        let take = request
            .max_lines
            .map_or(usize::MAX, |max| usize::try_from(max).unwrap_or(usize::MAX));
        let selected: Vec<&str> = lines.iter().skip(start).take(take).copied().collect();
        Ok(TextRead::Text(TextSlice {
            content: selected.join("\n"),
            line_count: selected.len() as u64,
            total_lines: lines.len() as u64,
            total_bytes: size,
            read_bytes: size,
            mtime_ms: 0,
            version: memory_rewind::version(&bytes),
        }))
    }

    async fn put_file(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        request: PutFile,
    ) -> Result<FileWritten, EnvError> {
        memory_rewind::put(&mut lock(&self.files), request)
    }
}

#[async_trait]
impl Search for MemoryEnvironment {
    /// 工作区文件清单（`@` 补全）：`cwd` 下的全部文件，相对路径，按路径排序。
    async fn list_files(
        &self,
        _workspace: &WorkspaceRef,
        request: ListFiles,
    ) -> Result<FileList, EnvError> {
        let prefix = child_prefix(request.cwd.as_str());
        let files = lock(&self.files)
            .keys()
            .filter_map(|key| key.strip_prefix(&prefix).map(str::to_owned))
            .collect();
        Ok(FileList { files })
    }
}

#[async_trait]
impl Commands for MemoryEnvironment {
    async fn start_command(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: StartCommand,
    ) -> Result<(), EnvError> {
        Err(unused())
    }

    fn watch_command(
        &self,
        _operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        stream::iter([Err(unused())]).boxed()
    }

    async fn cancel(&self, _operation: OperationId) -> Result<(), EnvError> {
        Err(unused())
    }

    async fn operation_status(&self, _operation: OperationId) -> Result<Outcome, EnvError> {
        Err(unused())
    }

    async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

#[async_trait]
impl Shell for MemoryEnvironment {
    async fn shell_state(
        &self,
        workspace: &WorkspaceRef,
        _session: &SessionId,
    ) -> Result<ShellState, EnvError> {
        Ok(ShellState {
            cwd: lock(&self.shell_cwd)
                .clone()
                .unwrap_or_else(|| workspace.root.clone()),
            outputs: None,
            background: Vec::new(),
        })
    }

    /// 会话换工作目录（EnterWorktree、`/cd`）：之后的 `shell_state` 给出新目录。
    async fn set_shell_cwd(&self, _session: &SessionId, cwd: EnvPath) -> Result<(), EnvError> {
        *lock(&self.shell_cwd) = Some(cwd);
        Ok(())
    }
}

#[async_trait]
impl Checkpoints for MemoryEnvironment {
    async fn rewind_prepare(
        &self,
        _workspace: &WorkspaceRef,
        context: OperationContext,
        request: RewindPrepare,
    ) -> Result<RewindPlan, EnvError> {
        let files = lock(&self.files).clone();
        Ok(lock(&self.rewinds).prepare(&files, context.operation, request.files))
    }

    async fn rewind_apply(&self, token: &OperationId) -> Result<RewindStatus, EnvError> {
        if self.fail_apply.swap(false, Ordering::Relaxed) {
            return Ok(RewindStatus::Failed {
                message: "disk full".to_owned(),
                conflicts: Vec::new(),
            });
        }
        let mut files = lock(&self.files);
        Ok(lock(&self.rewinds).apply(&mut files, token))
    }

    async fn rewind_status(&self, token: &OperationId) -> Result<RewindStatus, EnvError> {
        Ok(lock(&self.rewinds).status(token))
    }

    async fn rewind_finish(&self, token: &OperationId) -> Result<(), EnvError> {
        lock(&self.finished).push(token.clone());
        lock(&self.rewinds).finish(token);
        Ok(())
    }
}

#[async_trait]
impl Git for MemoryEnvironment {
    async fn git_status(
        &self,
        _workspace: &WorkspaceRef,
        _request: GitStatusRequest,
    ) -> Result<GitStatus, EnvError> {
        self.git.clone().ok_or_else(unused)
    }

    async fn git_diff(
        &self,
        _workspace: &WorkspaceRef,
        _request: crate::env_git::GitDiffRequest,
    ) -> Result<crate::env_git::GitDiff, EnvError> {
        self.diff.clone().ok_or_else(unused)
    }
}

#[async_trait]
impl Background for MemoryEnvironment {
    async fn background_foreground(
        &self,
        _session: &SessionId,
        reason: crate::env_command::BackgroundReason,
    ) -> Result<Vec<crate::ids::ShellTaskId>, EnvError> {
        lock(&self.moved).push(reason);
        Ok(Vec::new())
    }
}
impl Pdf for MemoryEnvironment {}
impl Fetch for MemoryEnvironment {}

impl MemoryEnvironment {
    fn stat_one(
        &self,
        _workspace: &WorkspaceRef,
        request: StatFile,
    ) -> Result<Option<FileStat>, EnvError> {
        let path = request.path.as_str();
        let file = lock(&self.files)
            .get(path)
            .map(|bytes| (bytes.len(), memory_rewind::version(bytes)));
        let stat = |kind, size: usize, version| FileStat {
            kind,
            size: size as u64,
            mtime_ms: 0,
            version,
        };
        let special = lock(&self.specials).contains(path);
        Ok(match file {
            None if special => Some(stat(FileKind::Other, 0, FileVersion::new("special"))),
            Some((size, version)) => Some(stat(FileKind::File, size, version)),
            None if self.is_dir(path) => {
                Some(stat(FileKind::Directory, 0, FileVersion::new("dir")))
            }
            None => None,
        })
    }
}
