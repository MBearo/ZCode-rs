//! `/diff`（docs/specs/rust-cli/tui-manage.md §4）：当前未提交的改动与每回合的改动。未提交改动是打开时向内核
//! 现取的 git 原始输出（解析只为显示）；回合改动来自视图的 `turn_changes`。这里只有页面的界面状态。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::env_git::GitDiff;

use super::app::{Action, App, Overlay};
use super::i18n::Lang;
use super::manage::Manage;

/// 文件数、每个文件的行数与 diff 大小的上限。
const MAX_FILES: usize = 50;
const MAX_LINES: usize = 400;
const MAX_BYTES: usize = 1_000_000;

/// 一个改动的文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    pub added: u64,
    pub removed: u64,
    pub binary: bool,
    pub untracked: bool,
    /// 统一 diff 的正文（第一个 `@@` 起，最多 [`MAX_LINES`] 行）；太大或没有时为空。
    pub body: Vec<String>,
}

/// 未提交改动的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Current {
    Loading,
    NotRepository,
    Failed(String),
    Loaded {
        files: Vec<DiffFile>,
        /// 全部改动的文件数与行数（可能多于列出的文件）。
        stats: (usize, u64, u64),
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffView {
    /// 0 是当前改动，`n` 是第 `n` 个有改动的回合。
    pub source: usize,
    pub selected: usize,
    /// 详情页的滚动；None 时是列表。
    pub detail: Option<usize>,
    pub current: Current,
}

/// `git diff HEAD --numstat` + `git diff HEAD` + 未跟踪文件 → 文件清单（tui-manage.md §4.1）。
pub fn parse(numstat: &str, patch: &str, untracked: &str) -> Current {
    let mut bodies: Vec<(String, Vec<String>, usize)> = Vec::new();
    for line in patch.lines() {
        if let Some(header) = line.strip_prefix("diff --git ") {
            let path = header
                .rsplit_once(" b/")
                .map_or(header, |(_, path)| path)
                .to_owned();
            bodies.push((path, Vec::new(), 0));
            continue;
        }
        if let Some((_, body, bytes)) = bodies.last_mut() {
            *bytes += line.len() + 1;
            if !body.is_empty() || line.starts_with("@@") {
                body.push(line.to_owned());
            }
        }
    }
    let body_of = |path: &str| {
        bodies
            .iter()
            .find(|(known, _, _)| known == path)
            .filter(|(_, _, bytes)| *bytes <= MAX_BYTES)
            .map(|(_, body, _)| body.iter().take(MAX_LINES).cloned().collect())
            .unwrap_or_default()
    };
    let mut files = Vec::new();
    let (mut count, mut added, mut removed) = (0, 0, 0);
    for line in numstat.lines().filter(|line| !line.is_empty()) {
        // `git diff --numstat`：新增、删除、路径三段。
        const NUMSTAT_FIELDS: usize = 3;
        let mut parts = line.splitn(NUMSTAT_FIELDS, '\t');
        let (Some(a), Some(d), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let binary = a == "-" || d == "-";
        let (a, d) = (a.parse().unwrap_or(0), d.parse().unwrap_or(0));
        count += 1;
        added += a;
        removed += d;
        if files.len() < MAX_FILES {
            files.push(DiffFile {
                path: path.to_owned(),
                added: a,
                removed: d,
                binary,
                untracked: false,
                body: body_of(path),
            });
        }
    }
    for path in untracked.lines().filter(|line| !line.is_empty()) {
        count += 1;
        if files.len() < MAX_FILES {
            files.push(DiffFile {
                path: path.to_owned(),
                added: 0,
                removed: 0,
                binary: false,
                untracked: true,
                body: Vec::new(),
            });
        }
    }
    Current::Loaded {
        files,
        stats: (count, added, removed),
    }
}

impl DiffView {
    pub fn new() -> Self {
        Self {
            source: 0,
            selected: 0,
            detail: None,
            current: Current::Loading,
        }
    }
}

impl Default for DiffView {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn dismissed(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Diff dialog dismissed",
        Lang::Zh => "已关闭 diff 页面",
    }
}

impl App {
    /// 这个来源的文件数（回合来自视图，当前改动来自查询结果）。
    pub(crate) fn diff_files(&self, view: &DiffView) -> usize {
        match view.source {
            0 => match &view.current {
                Current::Loaded { files, .. } => files.len(),
                _ => 0,
            },
            turn => self
                .view
                .turn_changes()
                .get(turn - 1)
                .map_or(0, |changes| changes.files.len()),
        }
    }

    /// `/diff`：打开页面（当前改动先显示读取中），runner 随后现取。
    pub(super) fn open_diff(&mut self) -> Option<Action> {
        self.overlay = Some(Overlay::Diff(DiffView::new()));
        Some(Action::Manage(Manage::Diff))
    }

    /// 未提交改动到了（页面已关闭时丢掉）。
    pub fn diff_loaded(&mut self, diff: Result<GitDiff, String>) {
        if let Some(Overlay::Diff(view)) = self.overlay.as_mut() {
            view.current = match diff {
                Ok(GitDiff::NotRepository) => Current::NotRepository,
                Ok(GitDiff::Repository {
                    numstat,
                    patch,
                    untracked,
                }) => parse(&numstat, &patch, &untracked),
                Err(reason) => Current::Failed(reason),
            };
        }
    }

    /// 滚轮（tui-select.md §4.1）：文件详情里滚动 `lines` 行；文件列表里每格移动选中项 1 项，到头停住。
    pub(super) fn diff_wheel(&mut self, lines: isize) {
        let Some(Overlay::Diff(view)) = self.overlay.clone() else {
            return;
        };
        let files = self.diff_files(&view);
        if let Some(Overlay::Diff(open)) = self.overlay.as_mut() {
            match open.detail {
                Some(scroll) => open.detail = Some(scroll.saturating_add_signed(lines)),
                None => {
                    let last = files.saturating_sub(1) as isize;
                    open.selected =
                        (open.selected as isize + lines.signum()).clamp(0, last) as usize;
                }
            }
        }
    }

    pub(super) fn diff_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Overlay::Diff(view)) = self.overlay.clone() else {
            return None;
        };
        let sources = 1 + self.view.turn_changes().len();
        let files = self.diff_files(&view);
        let mut next = view.clone();
        let close = match (view.detail, key.code) {
            (_, KeyCode::Char('c')) if ctrl => true,
            (Some(_), KeyCode::Esc | KeyCode::Left) => {
                next.detail = None;
                false
            }
            (Some(scroll), KeyCode::Down | KeyCode::Char('j')) => {
                next.detail = Some(scroll + 1);
                false
            }
            (Some(scroll), KeyCode::Up | KeyCode::Char('k')) => {
                next.detail = Some(scroll.saturating_sub(1));
                false
            }
            (Some(_), _) => false,
            (None, KeyCode::Esc) => true,
            (None, KeyCode::Left) if view.source > 0 => {
                next.source -= 1;
                next.selected = 0;
                false
            }
            (None, KeyCode::Right) if view.source + 1 < sources => {
                next.source += 1;
                next.selected = 0;
                false
            }
            (None, KeyCode::Up | KeyCode::Char('k')) => {
                next.selected = view.selected.saturating_sub(1);
                false
            }
            (None, KeyCode::Down | KeyCode::Char('j')) if view.selected + 1 < files => {
                next.selected += 1;
                false
            }
            (None, KeyCode::Enter) if files > 0 => {
                next.detail = Some(0);
                false
            }
            (None, _) => false,
        };
        if close {
            self.overlay = None;
            self.info(dismissed(self.lang));
        } else {
            self.overlay = Some(Overlay::Diff(next));
        }
        None
    }
}

#[cfg(test)]
#[path = "diff_view_tests.rs"]
mod tests;
