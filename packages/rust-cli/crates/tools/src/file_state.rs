//! readFileState：每个会话“读过哪些文件、读到的是哪个版本”（docs/specs/rust-cli/tools-files.md §4）。
//!
//! 用途：
//! - Read 写入选中的内容与 offset / limit；Write / Edit / NotebookEdit 写入写后的全文；
//! - Edit / Write 据此判断“必须先读”和“读后被改过”，Read 据此给出重复读取提示；
//! - 每个会话最多 100 条、内容合计 25MB，超出时淘汰最久未用的条目。
//!
//! 归属：状态以会话为键，放在这个 store 里；store 属于 runtime 组装的那一套内置工具（每个
//! `LocalKernel` 一个），不是进程级全局变量。“读后被改过”用执行环境返回的 [`FileVersion`] 判断，
//! 不比本机 mtime。会话关闭时由会话的 owner 调 [`ReadFileState::forget`]；store 只保留最近用过的
//! [`MAX_SESSIONS`] 个会话，淘汰旧会话只会让模型多读一次文件，不会放过修改。
//! 冷恢复时用 [`ReadFileState::restore`] 从会话历史重建；
//! 压缩时经 [`ReadFileStates::clear`] 清空（compact.md §3.4）。

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rust_cli_kernel_api::env::FileVersion;
use rust_cli_kernel_api::env_registry::LOCAL_MACHINE;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::tool::ReadFileStates;

const MAX_ENTRIES: usize = 100;
const MAX_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_SESSIONS: usize = 16;

/// 怎样判断这条记录之后文件有没有被改过。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Baseline {
    /// 执行侧在读或写时给出的版本；当前版本不同即为改过。
    Version(FileVersion),
    /// 从会话历史重建的记录没有版本：拿当前内容与记录的内容比（比内容，不比消息时间戳与 mtime）。
    Content,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileState {
    /// 模型看到的内容：Read 选中的行（未加行号），或 Write / Edit 写后的全文。
    pub content: String,
    pub baseline: Baseline,
    /// Read 的 offset（1 起）；Write / Edit / 重建的记录为 None。
    pub offset: Option<u64>,
    pub limit: Option<u64>,
    /// 只看到了一部分。
    pub partial: bool,
    /// 写入的内容不在模型上下文里。
    pub not_in_context: bool,
}

impl FileState {
    /// 从头读完的完整视图。
    pub fn is_full_view(&self) -> bool {
        if self.offset.unwrap_or(1) > 1 || self.partial {
            return false;
        }
        match self.limit {
            None => true,
            Some(limit) => {
                !self.content.is_empty() && (self.content.matches('\n').count() as u64) + 1 < limit
            }
        }
    }

    /// 记录之后文件是否被改过。`current` 是当前内容（去 BOM、换行归一），只在需要比内容时才算。
    ///
    /// 版本不同但这是完整视图、内容又没变时不算改过（容忍 Windows 时间戳、云同步造成的版本变化）。
    pub fn changed(&self, version: &FileVersion, current: impl FnOnce() -> Option<String>) -> bool {
        match &self.baseline {
            Baseline::Version(recorded) if recorded == version => false,
            Baseline::Version(_) => {
                !(self.is_full_view() && current().is_some_and(|text| text == self.content))
            }
            Baseline::Content => current().is_none_or(|text| text != self.content),
        }
    }

    /// 完整视图且内容就在上下文里。
    pub fn is_in_context(&self) -> bool {
        self.is_full_view() && !self.not_in_context
    }
}

/// 一条记录的键（R3，remote-exec.md §20.7.3）：哪台机器上的哪个路径。远端读过的文件不能拿去改本机的同名文件。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileKey {
    pub machine: String,
    pub path: String,
}

impl FileKey {
    pub fn new(machine: &str, path: &str) -> Self {
        Self {
            machine: machine.to_owned(),
            path: path.to_owned(),
        }
    }

    fn local(path: &str) -> Self {
        Self::new(LOCAL_MACHINE, path)
    }

    pub fn is_local(&self) -> bool {
        self.machine == LOCAL_MACHINE
    }
}

#[derive(Default)]
struct SessionFiles {
    entries: HashMap<FileKey, FileState>,
    /// 最久未用的在前。
    order: VecDeque<FileKey>,
    bytes: usize,
}

impl SessionFiles {
    fn touch(&mut self, key: &FileKey) {
        if let Some(index) = self.order.iter().position(|item| item == key)
            && let Some(item) = self.order.remove(index)
        {
            self.order.push_back(item);
        }
    }

    fn remove(&mut self, key: &FileKey) {
        if let Some(old) = self.entries.remove(key) {
            self.bytes -= old.content.len();
            self.order.retain(|item| item != key);
        }
    }

    /// 上限按会话合计（全部机器一起，暂定，remote-exec.md §20.7.3）。
    fn insert(&mut self, key: FileKey, state: FileState) {
        self.remove(&key);
        self.bytes += state.content.len();
        self.order.push_back(key.clone());
        self.entries.insert(key, state);
        while self.entries.len() > MAX_ENTRIES || (self.bytes > MAX_BYTES && self.order.len() > 1) {
            match self.order.front().cloned() {
                Some(oldest) => self.remove(&oldest),
                None => break,
            }
        }
    }
}

#[derive(Default)]
struct Sessions {
    files: HashMap<SessionId, SessionFiles>,
    order: VecDeque<SessionId>,
}

impl Sessions {
    fn session(&mut self, session: &SessionId) -> &mut SessionFiles {
        self.order.retain(|item| item != session);
        self.order.push_back(session.clone());
        while self.order.len() > MAX_SESSIONS {
            if let Some(oldest) = self.order.pop_front() {
                self.files.remove(&oldest);
            }
        }
        self.files.entry(session.clone()).or_default()
    }

    fn entry(&mut self, session: &SessionId, key: &FileKey) -> Option<&mut FileState> {
        self.files
            .get_mut(session)
            .and_then(|files| files.entries.get_mut(key))
    }
}

/// 全部会话的 readFileState。方法都是同步的，锁不跨 await。`*_at` 带机器名（R3）；不带的是本机。
#[derive(Default)]
pub struct ReadFileState(Mutex<Sessions>);

impl ReadFileState {
    fn sessions(&self) -> MutexGuard<'_, Sessions> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn get(&self, session: &SessionId, path: &str) -> Option<FileState> {
        self.get_at(session, LOCAL_MACHINE, path)
    }

    pub fn get_at(&self, session: &SessionId, machine: &str, path: &str) -> Option<FileState> {
        let key = FileKey::new(machine, path);
        let mut sessions = self.sessions();
        let files = sessions.session(session);
        let found = files.entries.get(&key).cloned();
        if found.is_some() {
            files.touch(&key);
        }
        found
    }

    pub fn set(&self, session: &SessionId, path: &str, state: FileState) {
        self.set_at(session, LOCAL_MACHINE, path, state);
    }

    pub fn set_at(&self, session: &SessionId, machine: &str, path: &str, state: FileState) {
        self.sessions()
            .session(session)
            .insert(FileKey::new(machine, path), state);
    }

    /// 会话关闭：释放它的全部记录。
    pub fn forget(&self, session: &SessionId) {
        let mut sessions = self.sessions();
        sessions.files.remove(session);
        sessions.order.retain(|item| item != session);
    }

    /// fork 出的会话继承父会话读过的文件。
    pub fn fork(&self, from: &SessionId, to: &SessionId) {
        let mut sessions = self.sessions();
        let copied: Vec<(FileKey, FileState)> = sessions
            .files
            .get(from)
            .map(|files| {
                files
                    .order
                    .iter()
                    .filter_map(|key| Some((key.clone(), files.entries.get(key)?.clone())))
                    .collect()
            })
            .unwrap_or_default();
        let target = sessions.session(to);
        for (key, state) in copied {
            target.insert(key, state);
        }
    }

    /// 本机上没有 offset、也没有 limit 的记录（读过的文件在磁盘上被改动时要查的那些，tools-files.md §4.1），最近用过的
    /// 在前。不算一次使用。
    pub fn whole_files(&self, session: &SessionId) -> Vec<(String, FileState)> {
        self.whole_files_everywhere(session)
            .into_iter()
            .filter(|(key, _)| key.is_local())
            .map(|(key, state)| (key.path, state))
            .collect()
    }

    /// 同 [`ReadFileState::whole_files`]，全部机器（R3，remote-exec.md §20.7.4）。
    pub fn whole_files_everywhere(&self, session: &SessionId) -> Vec<(FileKey, FileState)> {
        let sessions = self.sessions();
        let Some(files) = sessions.files.get(session) else {
            return Vec::new();
        };
        files
            .order
            .iter()
            .rev()
            .filter_map(|key| {
                let state = files.entries.get(key)?;
                (state.offset.is_none() && state.limit.is_none())
                    .then(|| (key.clone(), state.clone()))
            })
            .collect()
    }

    /// 去掉一条记录（文件已经不在了）。
    pub fn remove(&self, session: &SessionId, path: &str) {
        self.remove_at(session, &FileKey::local(path));
    }

    pub fn remove_at(&self, session: &SessionId, key: &FileKey) {
        if let Some(files) = self.sessions().files.get_mut(session) {
            files.remove(key);
        }
    }

    /// 内容就是记录的内容，记下执行侧现在的版本；不改顺序与形状。
    pub fn rebase(&self, session: &SessionId, path: &str, version: FileVersion) {
        self.rebase_at(session, &FileKey::local(path), version);
    }

    pub fn rebase_at(&self, session: &SessionId, key: &FileKey, version: FileVersion) {
        if let Some(state) = self.sessions().entry(session, key) {
            state.baseline = Baseline::Version(version);
        }
    }

    /// 记录的内容不在模型的上下文里（只告诉了模型文件改了，没给内容）。不改顺序。
    pub fn mark_not_in_context(&self, session: &SessionId, path: &str) {
        self.mark_not_in_context_at(session, &FileKey::local(path));
    }

    pub fn mark_not_in_context_at(&self, session: &SessionId, key: &FileKey) {
        if let Some(state) = self.sessions().entry(session, key) {
            state.not_in_context = true;
        }
    }

    /// 冷恢复：用重建出的记录替换这个会话的状态（见 [`crate::file_state_restore`]）。
    pub fn replace(&self, session: &SessionId, entries: Vec<(FileKey, FileState)>) {
        let mut sessions = self.sessions();
        let files = sessions.session(session);
        *files = SessionFiles::default();
        for (key, state) in entries {
            files.insert(key, state);
        }
    }
}

/// 压缩时清空（kernel-api [`ReadFileStates`]，先取出再清空）：一次锁内取走这个会话的
/// 记录，返回本机的路径，最近用过的在前。远端的记录一起清掉，但压缩后不重读（暂定，remote-exec.md §20.7.3：compact 的
/// 重读用本机的 Read）。
impl ReadFileStates for ReadFileState {
    fn clear(&self, session: &SessionId) -> Vec<String> {
        let mut sessions = self.sessions();
        let taken = sessions.files.remove(session);
        sessions.order.retain(|item| item != session);
        taken
            .map(|files| {
                files
                    .order
                    .into_iter()
                    .rev()
                    .filter(FileKey::is_local)
                    .map(|key| key.path)
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "file_state_tests.rs"]
mod tests;
