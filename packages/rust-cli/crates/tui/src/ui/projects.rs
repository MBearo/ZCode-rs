//! ← 项目页（修复线 F29，docs/specs/rust-cli/tui-panels.md §2；分组方式 `Group: Project`）：所有项目的
//! 会话按工作目录分组。数据是内核 `project_sessions` 的快照（打开时取一次）；这里只有分组、筛选、搜索、改名输入与焦点，
//! 排版在 `projects_draw.rs`。打开、新建、改名经 [`Request`] 交给 runner。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::panels::{ProjectSessions, SessionActivity};
use rust_cli_kernel_api::store::SessionOrder;

use super::app::{Action, App, Overlay};
use super::i18n_projects as words;
use super::session_rows::SessionRow;

/// 项目页与 `/resume` 页一次最多取的会话数（tui-panels.md §2.1）。
pub const LIMIT: usize = 200;

/// 面板里一个会话的状态（Host §8.9 的四种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    NeedsYou,
    Working,
    Ready,
    Inactive,
}

impl State {
    pub fn of(activity: Option<SessionActivity>) -> Self {
        match activity {
            Some(SessionActivity::NeedsInput) => Self::NeedsYou,
            Some(SessionActivity::Working) => Self::Working,
            Some(SessionActivity::Ready) => Self::Ready,
            None => Self::Inactive,
        }
    }
}

/// 筛选标签：全部加四种状态。
pub const FILTERS: [Option<State>; 5] = [
    None,
    Some(State::NeedsYou),
    Some(State::Working),
    Some(State::Ready),
    Some(State::Inactive),
];

/// 要 runner 去做的事（问内核、切换会话、信任检查）。
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// ← 项目页已打开：取所有项目的会话。
    LoadProjects,
    /// `/resume` 页：按筛选（None 是全部）与排序取会话。
    LoadResume {
        root: Option<String>,
        order: SessionOrder,
    },
    /// 切到别的目录的会话：先做信任检查；`cd` 是打开后要换到的目录（“Use current directory”）。`startup`：启动形态的
    /// `/resume` 页（`--resume` 不带参数）选中的，打开时不写续写占位，同 `--resume <id>`（session-lifecycle.md §6.3.1，
    /// 工作线 F79）；它选当前目录的会话也走这里（信任检查对当前目录不弹框）。
    Open {
        id: String,
        root: String,
        cd: Option<String>,
        startup: bool,
    },
    /// 在这个目录新建会话（`n`）。
    New {
        root: String,
    },
    Rename {
        id: String,
        title: String,
    },
    Delete(String),
    /// 信任对话框里同意了：写信任记录，再做 `then`（不再检查）。
    Trusted {
        root: String,
        then: Box<Request>,
    },
    /// `/resume` 页的预览（⌃e）与全屏对话（⌃t）要的只读快照。
    Preview(String),
    Transcript(String),
}

/// 列表里的一行：组头、组之间的空行、会话（`rows` 的下标）、末尾的“只显示前 N 个”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Group {
        row: usize,
        shown: usize,
        total: usize,
    },
    Gap,
    Session(usize),
    More,
}

/// 页面的视图状态。焦点按会话 ID 记（筛选与搜索改变可见行时焦点不乱跳）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectsPage {
    /// None：还在取。
    pub rows: Option<Vec<SessionRow>>,
    pub more: bool,
    pub filter: usize,
    pub selected: Option<String>,
    /// 搜索中的查询（`/` 打开）。
    pub search: Option<String>,
    /// 改名中：（会话，输入）。
    pub rename: Option<(String, String)>,
    /// 搜索行的红字（改名失败）。
    pub error: Option<String>,
    /// 列表滚过的行数与上一帧的可见行数（翻页用）。
    pub scroll: usize,
    pub height: usize,
}

impl ProjectsPage {
    /// 会话按分组排好的下标：当前工作区的组在最前，其余按路径的字典序；组内按更新时间从新到旧。
    pub fn ordered(&self, current_root: &str) -> Vec<usize> {
        let rows = self.rows.as_deref().unwrap_or_default();
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by(|&a, &b| {
            let (a, b) = (&rows[a], &rows[b]);
            let rank = |row: &SessionRow| u8::from(row.root != current_root);
            rank(a)
                .cmp(&rank(b))
                .then_with(|| a.root.cmp(&b.root))
                .then_with(|| b.updated.cmp(&a.updated))
                .then_with(|| b.id.cmp(&a.id))
        });
        order
    }

    /// 筛选与搜索之后可见的会话（改名中的那一个总可见，改名时不会消失）。
    pub fn visible(&self, current_root: &str) -> Vec<usize> {
        let rows = self.rows.as_deref().unwrap_or_default();
        let filter = FILTERS[self.filter % FILTERS.len()];
        let query = self.search.as_deref().unwrap_or("").to_lowercase();
        let renaming = self.rename.as_ref().map(|(id, _)| id.as_str());
        self.ordered(current_root)
            .into_iter()
            .filter(|&index| {
                let row = &rows[index];
                renaming == Some(row.id.as_str())
                    || (filter.is_none_or(|f| State::of(row.activity) == f)
                        && row.matches(&query, false))
            })
            .collect()
    }

    /// 画出来的行：每组前面是组头（组之间空一行），最后是“只显示前 N 个”。
    pub fn entries(&self, current_root: &str) -> Vec<Entry> {
        let rows = self.rows.as_deref().unwrap_or_default();
        let visible = self.visible(current_root);
        let mut entries = Vec::new();
        let mut previous: Option<&str> = None;
        for &index in &visible {
            let root = rows[index].root.as_str();
            if previous != Some(root) {
                if previous.is_some() {
                    entries.push(Entry::Gap);
                }
                entries.push(Entry::Group {
                    row: index,
                    shown: visible.iter().filter(|&&i| rows[i].root == root).count(),
                    total: rows.iter().filter(|r| r.root == root).count(),
                });
                previous = Some(root);
            }
            entries.push(Entry::Session(index));
        }
        if self.more && !entries.is_empty() {
            entries.push(Entry::More);
        }
        entries
    }

    /// 焦点所在的会话在 `rows` 里的下标；焦点不可见时取第一个可见的。
    pub fn focused(&self, current_root: &str) -> Option<usize> {
        let rows = self.rows.as_deref()?;
        let visible = self.visible(current_root);
        visible
            .iter()
            .copied()
            .find(|&i| Some(rows[i].id.as_str()) == self.selected.as_deref())
            .or_else(|| visible.first().copied())
    }

    pub fn focused_row(&self, current_root: &str) -> Option<&SessionRow> {
        let index = self.focused(current_root)?;
        self.rows.as_deref()?.get(index)
    }

    /// 移动焦点：`step` 个会话（跨组）；`wrap` 时到头绕回（按键），否则停住（滚轮）。
    fn step(&mut self, current_root: &str, step: isize, wrap: bool) {
        let visible = self.visible(current_root);
        let Some(rows) = self.rows.as_deref() else {
            return;
        };
        if visible.is_empty() {
            return;
        }
        let at = self
            .focused(current_root)
            .and_then(|f| visible.iter().position(|&i| i == f))
            .unwrap_or(0) as isize;
        let last = visible.len() as isize - 1;
        let next = if wrap {
            (at + step).rem_euclid(last + 1)
        } else {
            (at + step).clamp(0, last)
        };
        self.selected = Some(rows[visible[next as usize]].id.clone());
    }

    fn select_index(&mut self, current_root: &str, first: bool) {
        let visible = self.visible(current_root);
        let pick = if first {
            visible.first()
        } else {
            visible.last()
        };
        if let (Some(&index), Some(rows)) = (pick, self.rows.as_deref()) {
            self.selected = Some(rows[index].id.clone());
        }
    }
}

impl App {
    /// 输入框为空时按 ←（或点击 `← for projects`）：先打开页面，数据由 runner 取回后填入。
    pub(super) fn open_projects(&mut self) -> Option<Action> {
        self.overlay = Some(Overlay::Projects(Box::default()));
        Some(Action::Projects(Request::LoadProjects))
    }

    /// runner 取回的数据（页面已关掉时丢弃）；焦点留在原来的会话上（没有时是当前会话）。
    pub fn projects_loaded(&mut self, sessions: ProjectSessions) {
        let current = self.session.clone();
        if let Some(Overlay::Projects(page)) = self.overlay.as_mut() {
            let rows: Vec<SessionRow> = sessions
                .sessions
                .iter()
                .map(|session| SessionRow::new(session, current.as_deref()))
                .collect();
            if page.selected.is_none() {
                page.selected = current.filter(|id| rows.iter().any(|r| &r.id == id));
            }
            page.rows = Some(rows);
            page.more = sessions.more;
        }
    }

    /// 改名失败：在搜索行显示原因（页面开着时）。
    pub fn projects_error(&mut self, text: String) {
        if let Some(Overlay::Projects(page)) = self.overlay.as_mut() {
            page.error = Some(text);
        }
    }

    pub(super) fn projects_key(&mut self, key: KeyEvent) -> Option<Action> {
        let Some(Overlay::Projects(page)) = self.overlay.as_mut() else {
            return None;
        };
        let root = self.view.workspace().root.as_str().to_owned();
        page.error = None;
        // 文字输入不带 ctrl 与 alt（Windows 的 AltGr 算文字，修复线 F65，tui-input.md §9.1）。
        let ctrl = self.typing.ctrl(&key);
        let plain = self.typing.is_plain_text(&key);
        if let Some((id, input)) = page.rename.as_mut() {
            match key.code {
                KeyCode::Esc => page.rename = None,
                KeyCode::Char('c') if ctrl => page.rename = None,
                KeyCode::Enter => {
                    let title = input.trim().to_owned();
                    let id = id.clone();
                    page.rename = None;
                    return (!title.is_empty())
                        .then_some(Action::Projects(Request::Rename { id, title }));
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if plain => input.push(c),
                _ => {}
            }
            return None;
        }
        if let Some(query) = page.search.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    page.search = None;
                    return None;
                }
                KeyCode::Char('c') if ctrl => {
                    page.search = None;
                    return None;
                }
                KeyCode::Backspace => {
                    query.pop();
                    return None;
                }
                KeyCode::Char(c) if plain => {
                    query.push(c);
                    // 查询变了：焦点回到第一个匹配（搜索时选第一个可见的）。
                    page.selected = None;
                    let first = page.visible(&root).first().copied();
                    page.selected = first.and_then(|i| page.rows.as_ref().map(|r| r[i].id.clone()));
                    return None;
                }
                _ => {}
            }
        }
        let height = page.height.max(1) as isize;
        // 搜索中 ← / → 不关页面、不打开（编辑时左右键不动列表）。
        if page.search.is_some() && matches!(key.code, KeyCode::Left | KeyCode::Right) {
            return None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
                self.overlay = None;
                return None;
            }
            KeyCode::Char('c') if ctrl => {
                self.overlay = None;
                return None;
            }
            KeyCode::Up => page.step(&root, -1, true),
            KeyCode::Down => page.step(&root, 1, true),
            KeyCode::Char('k') if plain => page.step(&root, -1, true),
            KeyCode::Char('j') if plain => page.step(&root, 1, true),
            KeyCode::Char('p') if ctrl => page.step(&root, -1, true),
            KeyCode::Char('n') if ctrl => page.step(&root, 1, true),
            KeyCode::PageUp => page.step(&root, -height, false),
            KeyCode::PageDown => page.step(&root, height, false),
            KeyCode::Home => page.select_index(&root, true),
            KeyCode::End => page.select_index(&root, false),
            KeyCode::Tab => {
                page.filter = (page.filter + 1) % FILTERS.len();
                page.scroll = 0;
            }
            KeyCode::BackTab => {
                page.filter = (page.filter + FILTERS.len() - 1) % FILTERS.len();
                page.scroll = 0;
            }
            KeyCode::Char('/') if page.search.is_none() => page.search = Some(String::new()),
            KeyCode::Char('n') => {
                let target = page
                    .focused_row(&root)
                    .map_or_else(|| root.clone(), |row| row.root.clone());
                return Some(Action::Projects(Request::New { root: target }));
            }
            KeyCode::Char('r') => {
                if let Some(row) = page.focused_row(&root) {
                    page.rename = Some((row.id.clone(), row.title.clone()));
                    page.search = None;
                }
            }
            KeyCode::Char('o') => {
                self.overlay = None;
                return Some(Action::OpenResume);
            }
            KeyCode::Enter | KeyCode::Right => return self.open_project_row(),
            _ => {}
        }
        None
    }

    /// 滚轮：焦点移动，到头停住。
    pub(super) fn projects_wheel(&mut self, rows: isize) {
        let root = self.view.workspace().root.as_str().to_owned();
        if let Some(Overlay::Projects(page)) = self.overlay.as_mut() {
            page.step(&root, rows, false);
        }
    }

    /// 点击一行：选中它再打开。
    pub(super) fn project_clicked(&mut self, index: usize) -> Option<Action> {
        if let Some(Overlay::Projects(page)) = self.overlay.as_mut() {
            let id = page.rows.as_ref()?.get(index)?.id.clone();
            page.selected = Some(id);
        }
        self.open_project_row()
    }

    /// 打开焦点所在的会话（tui-panels.md §2.4）：当前会话只关闭页面并提示；同一个工作区的直接切过去；别的目录的
    /// 先交给 runner 做信任检查（页面开着，检查通过后由 runner 关闭）。
    fn open_project_row(&mut self) -> Option<Action> {
        let Some(Overlay::Projects(page)) = self.overlay.as_mut() else {
            return None;
        };
        let root = self.view.workspace().root.as_str().to_owned();
        let row = page.focused_row(&root)?.clone();
        page.search = None;
        self.open_row(&row, None, false)
    }

    /// 两页共用的“打开这个会话”（tui-panels.md §2.4、tui-manage.md §10.5）。`startup`：启动形态的 `/resume` 页
    ///（`Request::Open.startup`）。
    pub(super) fn open_row(
        &mut self,
        row: &SessionRow,
        cd: Option<String>,
        startup: bool,
    ) -> Option<Action> {
        if row.current {
            self.overlay = None;
            let title = row.label();
            let title = if title.is_empty() {
                row.id.clone()
            } else {
                title
            };
            self.info(words::already_viewing(self.lang, &title));
            return None;
        }
        if cd.is_some() || row.root == self.view.workspace().root.as_str() {
            self.overlay = None;
            if cd.is_none() && !startup {
                return Some(Action::Resume(row.id.clone()));
            }
        }
        Some(Action::Projects(Request::Open {
            id: row.id.clone(),
            root: row.root.clone(),
            cd,
            startup,
        }))
    }
}

#[cfg(test)]
#[path = "projects_tests.rs"]
mod tests;
