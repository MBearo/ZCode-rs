//! 读过的文件在磁盘上被改动（docs/specs/rust-cli/tools-files.md §4.1）。
//!
//! 每一步的当步 attachment 之前查一次：readFileState 里没有 offset 与 limit 的记录（Edit / Write / NotebookEdit 写的、
//! Bash 读文件记下的、冷恢复重建的；Read 写的记录 offset 是 1，不查）。版本变了就整读一次，记录改成 Read 的形状
//! （之后不再查），内容确实变了时给出改动片段。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvError, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::{ReadText, StatFile, TextRead};
use rust_cli_kernel_api::env_path;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::machine::Files;
use rust_cli_kernel_api::tool::{ChangedFile, ChangedFiles};

use crate::common::utf16_len;
use crate::file_state::{Baseline, FileState, ReadFileState};
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
}

impl FileChanges {
    pub fn new(state: Arc<ReadFileState>, files: Arc<dyn Files>) -> Self {
        Self { state, files }
    }

    /// 一个文件：没变、读不了时为 None；变了时更新记录，返回片段（只说改了时片段为空）。
    async fn check(
        &self,
        (session, workspace): (&SessionId, &WorkspaceRef),
        path: &str,
        entry: FileState,
    ) -> Option<ChangedFile> {
        let extension = env_path::extension(path);
        if IMAGES.contains(&extension.as_str())
            || matches!(extension.as_str(), ".pdf" | ".ipynb")
            || is_binary(path)
            || is_device(path)
        {
            return None;
        }
        let stat = StatFile {
            path: EnvPath::new(path),
        };
        let version = match self.files.stat(workspace, stat).await {
            Ok(Some(stat)) => stat.version,
            Ok(None) | Err(EnvError::NotFound { .. }) => {
                self.state.remove(session, path);
                return None;
            }
            Err(_) => return None,
        };
        if matches!(&entry.baseline, Baseline::Version(recorded) if *recorded == version) {
            return None;
        }
        let request = ReadText {
            path: EnvPath::new(path),
            start_line: 0,
            max_lines: None,
            max_bytes: Some(MAX_BYTES),
        };
        let slice = match self.files.read_text(workspace, request).await {
            Ok(TextRead::Text(slice)) => slice,
            Err(EnvError::NotFound { .. }) => {
                self.state.remove(session, path);
                return None;
            }
            Ok(TextRead::Directory | TextRead::TooLarge { .. }) | Err(_) => return None,
        };
        if check_tokens(&slice.content, &extension).is_err() {
            return None;
        }
        // 冷恢复的记录没有版本：内容相同就只记下版本，形状不变。
        if entry.baseline == Baseline::Content && slice.content == entry.content {
            self.state.rebase(session, path, slice.version);
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
        self.state.set(session, path, reread);
        if slice.content == entry.content {
            return None;
        }
        let hidden = || {
            self.state.mark_not_in_context(session, path);
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
        let mut found = Vec::new();
        for (path, entry) in self.state.whole_files(session) {
            if let Some(change) = self.check((session, workspace), &path, entry).await {
                found.push(change);
            }
        }
        // 合计在加进每个片段之前比较：越过上限的那个文件仍然完整。
        let mut total = 0;
        for change in &mut found {
            if total >= MAX_PASS {
                change.snippet.clear();
                self.state.mark_not_in_context(session, &change.path);
            } else {
                total += utf16_len(&change.snippet);
            }
        }
        found
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
    let visible: String = lowered
        .chars()
        .filter(|c| {
            !matches!(u32::from(*c), 0x200c..=0x200f | 0x202a..=0x202e | 0x206a..=0x206f | 0xfeff)
        })
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
