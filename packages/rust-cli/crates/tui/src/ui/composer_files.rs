//! `@` 补全（docs/specs/rust-cli/tui-input.md §1.2）：输入框里的 `@` 词 → 索引匹配 → 弹出列表 → 接受。
//!
//! 文件清单的唯一来源是执行环境（经 `Kernel::workspace_files`，runner 去取）；这里只持有可丢弃的索引。新的
//! `@` 词出现时设 `wanted`，runner 看到后按 5 s 的节流决定是否再取（`due`）。
use std::time::{Duration, Instant};

use super::{Composer, Popup};
use crate::ui::file_index::{FileIndex, MAX_SUGGESTIONS};
use crate::ui::mention::{at_token, common_prefix, replacement};

/// 再取一次清单的最短间隔。
pub const REFRESH: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Files {
    index: Option<FileIndex>,
    /// 当前 `@` 词的候选。
    pub matches: Vec<String>,
    /// 当前 `@` 词的起点；换了词才考虑刷新。
    active: Option<usize>,
    /// 出现了新的 `@` 词，等 runner 判断要不要取清单。
    wanted: bool,
    loading: bool,
    fetched_at: Option<Instant>,
}

impl Files {
    /// runner：现在该不该去取清单（新词出现，且没有索引或已超过 [`REFRESH`]，且没有在途的请求）。
    /// 返回真时记为在途。
    pub fn due(&mut self, now: Instant) -> bool {
        if !std::mem::take(&mut self.wanted) || self.loading {
            return false;
        }
        let stale = self
            .fetched_at
            .is_none_or(|at| now.duration_since(at) >= REFRESH);
        if stale {
            self.loading = true;
        }
        stale
    }
}

impl Composer {
    /// 清单到达：替换索引（取失败时保留旧的），用当前的 `@` 词重新匹配。
    pub fn set_files(&mut self, files: Option<Vec<String>>, separator: char, now: Instant) {
        self.files.loading = false;
        self.files.fetched_at = Some(now);
        if let Some(files) = files {
            self.files.index = Some(FileIndex::new(files, separator));
        }
        self.refresh_popup();
    }

    /// 输入变化后：光标在 `@` 词里时匹配并显示文件列表；返回是否处理了（不是 `@` 词时交给斜杠列表的判断）。
    pub(super) fn refresh_files(&mut self) -> bool {
        let Some(token) = at_token(self.area.text(), self.area.cursor()) else {
            self.files.active = None;
            self.files.matches.clear();
            return false;
        };
        if self.files.active != Some(token.start) {
            self.files.active = Some(token.start);
            self.files.wanted = true;
        }
        let query = match token.query.as_str() {
            "." | "./" | ".\\" => "",
            other => other
                .strip_prefix("./")
                .or_else(|| other.strip_prefix(".\\"))
                .unwrap_or(other),
        };
        self.files.matches = self
            .files
            .index
            .as_ref()
            .map(|index| index.search(query, MAX_SUGGESTIONS))
            .unwrap_or_default();
        if self.files.matches.is_empty() {
            self.popup = None;
            return true;
        }
        let selected = match &self.popup {
            Some(Popup::Files { selected }) => *selected,
            _ => 0,
        };
        self.popup = Some(Popup::Files {
            selected: selected.min(self.files.matches.len() - 1),
        });
        true
    }

    /// 接受文件候选：tab 时若公共前缀比已输入的长，先补到公共前缀；
    /// 否则把整个 `@` 词换成选中的路径加空格。
    pub fn accept_file(&mut self, tab: bool) {
        let Some(Popup::Files { selected }) = self.popup else {
            return;
        };
        let Some(token) = at_token(self.area.text(), self.area.cursor()) else {
            return;
        };
        if tab {
            let prefix = common_prefix(&self.files.matches);
            if prefix.chars().count() > token.query.chars().count() {
                let text = replacement(&prefix, token.quoted, false);
                self.area.replace_range(token.start, token.end, &text);
                self.refresh_popup();
                return;
            }
        }
        let Some(path) = self.files.matches.get(selected).cloned() else {
            return;
        };
        let text = replacement(&path, token.quoted, true);
        self.area.replace_range(token.start, token.end, &text);
        self.popup = None;
        self.files.active = None;
        self.files.matches.clear();
    }
}
