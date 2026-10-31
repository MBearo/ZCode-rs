//! 读过的文件在磁盘上被改动（docs/specs/rust-cli/tools-files.md §4.1）。
//!
//! 每一步的当步 attachment 之前查一次：readFileState 里没有 offset 与 limit 的记录（Edit / Write / NotebookEdit 写的、
//! Bash 读文件记下的、冷恢复重建的；Read 写的记录 offset 是 1，不查）。版本变了就整读一次，记录改成 Read 的形状
//! （之后不再查），内容确实变了时给出改动片段。远端机器上读过的文件（R3，remote-exec.md §20.7.4）每台每步一次批量 stat。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvError, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::{ReadText, StatFile, StatMany, TextRead};
use rust_cli_kernel_api::env_path;
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::machine::Files;
use rust_cli_kernel_api::tool::{ChangedFile, ChangedFiles};

use crate::common::utf16_len;
use crate::file_state::{Baseline, FileKey, FileState, ReadFileState};
use crate::line_diff;
use crate::read_support::{IMAGES, MAX_BYTES, check_tokens, is_binary, is_device};

/// 记录的内容超过这么多 UTF-8 字节时只说改了（记录里不再留着可比的旧内容）。
const MAX_KEPT_BYTES: usize = 4096;
/// 一轮里各文件片段的合计上限（UTF-16 单元）：到了之后，后面的文件只说改了。
const MAX_PASS: usize = 16_384;

/// 当步 attachment 的生产者：readFileState 与执行环境是 runtime 给的同一份。
pub struct FileChanges {
    state: Arc<ReadFileState>,
    files: Arc<dyn Files>,
    /// 远端机器上读过的文件（R3，remote-exec.md §20.7.4）：每台每步一次 `stat_many`。没有时只查本机。
    machines: Option<Arc<dyn EnvironmentRegistry>>,
}

/// 查不查这个路径：图片、PDF、notebook、二进制与设备文件不查。
fn checkable(path: &str) -> bool {
    let extension = env_path::extension(path);
    !(IMAGES.contains(&extension.as_str())
        || matches!(extension.as_str(), ".pdf" | ".ipynb")
        || is_binary(path)
        || is_device(path))
}

impl FileChanges {
    pub fn new(state: Arc<ReadFileState>, files: Arc<dyn Files>) -> Self {
        Self {
            state,
            files,
            machines: None,
        }
    }

    /// 接上执行环境的注册表：连上的远端机器上读过的文件也查。
    #[must_use]
    pub fn with_machines(mut self, registry: Arc<dyn EnvironmentRegistry>) -> Self {
        self.machines = Some(registry);
        self
    }

    /// 本机的一个文件：没变、读不了时为 None。
    async fn check(
        &self,
        (session, workspace): (&SessionId, &WorkspaceRef),
        key: &FileKey,
        entry: FileState,
    ) -> Option<ChangedFile> {
        if !checkable(&key.path) {
            return None;
        }
        let stat = StatFile {
            path: EnvPath::new(&key.path),
        };
        let version = match self.files.stat(workspace, stat).await {
            Ok(Some(stat)) => stat.version,
            Ok(None) | Err(EnvError::NotFound { .. }) => {
                self.state.remove_at(session, key);
                return None;
            }
            Err(_) => return None,
        };
        let files = self.files.as_ref();
        self.compare((session, workspace, files), key, entry, version)
            .await
    }

    /// 一台远端机器上的文件：一次 `stat_many`，版本变了的再逐个读。那台机器没连上、正在重连或批量 stat 失败时都不查。
    ///
    /// 重连中的机器（远端执行 F96，remote-exec.md §23.3）：它的调用要等重连，SSH 又一直重连，问它会挡住这一步；同没
    /// 连上一样跳过，记录留着，连回来之后再查。
    async fn check_remote(
        &self,
        session: &SessionId,
        name: &str,
        entries: Vec<(FileKey, FileState)>,
    ) -> Vec<(FileKey, ChangedFile)> {
        let Some((machine, workspace)) = self
            .machines
            .as_ref()
            .filter(|registry| !registry.reconnecting(name))
            .and_then(|registry| registry.remote(name))
        else {
            return Vec::new();
        };
        let entries: Vec<(FileKey, FileState)> = entries
            .into_iter()
            .filter(|(key, _)| checkable(&key.path))
            .collect();
        if entries.is_empty() {
            return Vec::new();
        }
        let request = StatMany {
            paths: entries
                .iter()
                .map(|(key, _)| EnvPath::new(&key.path))
                .collect(),
        };
        let Ok(stats) = machine.files().stat_many(&workspace, request).await else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for ((key, entry), stat) in entries.into_iter().zip(stats) {
            let version = match stat {
                Ok(Some(stat)) => stat.version,
                Ok(None) | Err(EnvError::NotFound { .. }) => {
                    self.state.remove_at(session, &key);
                    continue;
                }
                Err(_) => continue,
            };
            let ports = (session, &workspace, machine.files().as_ref());
            if let Some(mut change) = self.compare(ports, &key, entry, version).await {
                // 提醒里写成 `<机器>:<路径>`（scp 的写法，暂定，remote-exec.md §20.5.2）。
                change.path = format!("{name}:{}", key.path);
                found.push((key, change));
            }
        }
        found
    }

    /// 版本变了：整读一次、记录改成 Read 的形状，内容确实变了时给出片段（只说改了时片段为空）。
    async fn compare(
        &self,
        (session, workspace, files): (&SessionId, &WorkspaceRef, &dyn Files),
        key: &FileKey,
        entry: FileState,
        version: rust_cli_kernel_api::env::FileVersion,
    ) -> Option<ChangedFile> {
        if matches!(&entry.baseline, Baseline::Version(recorded) if *recorded == version) {
            return None;
        }
        let path = key.path.as_str();
        let extension = env_path::extension(path);
        let request = ReadText {
            path: EnvPath::new(path),
            start_line: 0,
            max_lines: None,
            max_bytes: Some(MAX_BYTES),
        };
        let slice = match files.read_text(workspace, request).await {
            Ok(TextRead::Text(slice)) => slice,
            Err(EnvError::NotFound { .. }) => {
                self.state.remove_at(session, key);
                return None;
            }
            Ok(TextRead::Directory | TextRead::TooLarge { .. }) | Err(_) => return None,
        };
        if check_tokens(&slice.content, &extension).is_err() {
            return None;
        }
        // 冷恢复的记录没有版本：内容相同就只记下版本，形状不变。
        if entry.baseline == Baseline::Content && slice.content == entry.content {
            self.state.rebase_at(session, key, slice.version);
            return None;
        }
        let reread = FileState {
            content: slice.content.clone(),
            baseline: Baseline::Version(slice.version),
            offset: Some(1),
            limit: None,
            partial: false,
            not_in_context: false,
        };
        self.state.set_at(session, &key.machine, &key.path, reread);
        if slice.content == entry.content {
            return None;
        }
        let hidden = || {
            self.state.mark_not_in_context_at(session, key);
            ChangedFile {
                path: path.to_owned(),
                snippet: String::new(),
            }
        };
        if entry.content.len() > MAX_KEPT_BYTES {
            return Some(hidden());
        }
        let snippet = line_diff::snippet(&entry.content, &slice.content);
        if snippet.is_empty() {
            return None;
        }
        if sensitive(path) {
            return Some(hidden());
        }
        Some(ChangedFile {
            path: path.to_owned(),
            snippet,
        })
    }
}

#[async_trait]
impl ChangedFiles for FileChanges {
    async fn changed_files(
        &self,
        session: &SessionId,
        workspace: &WorkspaceRef,
    ) -> Vec<ChangedFile> {
        let mut found: Vec<(FileKey, ChangedFile)> = Vec::new();
        // 远端的按机器分组（按第一次出现的顺序），本机的照旧逐个查。
        let mut remote: Vec<(String, Vec<(FileKey, FileState)>)> = Vec::new();
        for (key, entry) in self.state.whole_files_everywhere(session) {
            if key.is_local() {
                if let Some(change) = self.check((session, workspace), &key, entry).await {
                    found.push((key, change));
                }
                continue;
            }
            match remote.iter_mut().find(|(name, _)| *name == key.machine) {
                Some((_, entries)) => entries.push((key, entry)),
                None => remote.push((key.machine.clone(), vec![(key, entry)])),
            }
        }
        for (name, entries) in remote {
            found.extend(self.check_remote(session, &name, entries).await);
        }
        // 合计在加进每个片段之前比较：越过上限的那个文件仍然完整。
        let mut total = 0;
        for (key, change) in &mut found {
            if total >= MAX_PASS {
                change.snippet.clear();
                self.state.mark_not_in_context_at(session, key);
            } else {
                total += utf16_len(&change.snippet);
            }
        }
        found.into_iter().map(|(_, change)| change).collect()
    }
}

/// 结尾的这些扩展名与后缀是密钥、证书一类的文件。
fn secret_suffix(name: &str) -> bool {
    const EXTENSIONS: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".keystore", ".jks"];
    if EXTENSIONS.iter().any(|extension| name.ends_with(extension)) {
        return true;
    }
    for algorithm in ["_rsa", "_dsa", "_ecdsa", "_ed25519"] {
        if let Some(at) = name.rfind(algorithm) {
            let rest = &name[at + algorithm.len()..];
            if rest.is_empty() || rest == "_sk" || rest == "_sk_rk" || rest.starts_with("_sk_rk_") {
                return true;
            }
        }
    }
    let stems = ["secret.", "secrets."];
    let kinds = ["yaml", "yml", "json", "toml"];
    stems.iter().any(|stem| {
        kinds
            .iter()
            .any(|kind| name.ends_with(&format!("{stem}{kind}")))
    })
}

/// `.env` 一类：`.env`、`.env.<x>`、`<p>.env`，x 与 p 的最后一段不是示例用的名字。
fn env_file(name: &str) -> bool {
    const SAMPLES: &[&str] = &["example", "sample", "template", "dist"];
    if name == ".env" {
        return true;
    }
    if let Some(rest) = name.strip_prefix(".env.") {
        return !SAMPLES.contains(&rest);
    }
    match name.strip_suffix(".env") {
        Some(prefix) => !SAMPLES.contains(&prefix.rsplit('.').next().unwrap_or_default()),
        None => false,
    }
}

/// 比较前的规范化：小写，去掉零宽与方向控制字符，去掉 `:` 之后的部分与结尾的 `.`、空格。
fn normalized(name: &str) -> String {
    let lowered: String = name
        .to_lowercase()
        .replace('\u{131}', "i")
        .replace('\u{17f}', "s");
    // 修复（F122）：以前只去掉 U+200C–U+200F、U+202A–U+202E、U+206A–U+206F、U+FEFF，藏着 U+061C、零宽空格或
    // U+2066–U+2069 的 `.env` 认不出来；三处各写一份范围，现在用 base 的同一张表（constants.md §5.1.4）。
    let visible: String = lowered
        .chars()
        .filter(|c| !rust_cli_base::chars::is_bidi_or_zero_width(*c))
        .collect();
    let base = visible.split(':').next().unwrap_or_default();
    let trimmed = base.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        lowered
    } else {
        trimmed.to_owned()
    }
}

/// 结尾的空白、换行与 `.`、空格去掉。
fn trailing_trimmed(name: &str) -> &str {
    name.trim_end_matches(['\n', '\r', '\u{2028}', '\u{2029}', '.', ' '])
}

/// 文件名是密钥、`.env` 一类的敏感文件：只说改了，不给片段。
pub(crate) fn sensitive(path: &str) -> bool {
    let Some(name) = path.rsplit(['/', '\\']).find(|part| !part.is_empty()) else {
        return true;
    };
    let candidates = [
        normalized(name),
        trailing_trimmed(&normalized(trailing_trimmed(name))).to_owned(),
    ];
    candidates
        .iter()
        .any(|name| env_file(name) || secret_suffix(name))
}

#[cfg(test)]
#[path = "changes_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "changes_machine_tests.rs"]
mod machine_tests;
