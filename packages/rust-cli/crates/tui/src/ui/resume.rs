//! `/resume` 独立页面（会话选择页）的状态与按键（修复线 F29，docs/specs/rust-cli/tui-manage.md §10）：
//! 筛选、排序、查询、焦点、展开的预览、全屏对话、改名输入、删除确认与目录选择。数据是内核 `project_sessions` 的快照，
//! 改名、删除、预览经 [`Request`] 交给 runner；排版在 `resume_draw.rs`。
use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::panels::ProjectSessions;
use rust_cli_kernel_api::session_api::SessionView;
use rust_cli_kernel_api::store::SessionOrder;

use super::app::{Action, App, Overlay};
use super::i18n_projects as words;
use super::keyboard;
use super::projects::Request;
use super::session_rows::SessionRow;
use crate::view::Transcript;

/// 预览最多几条对话。
pub const PREVIEW_ITEMS: usize = 6;

/// 一个会话的预览（⌃e）：读取中、读到的最近几条（是不是用户的话，文字）、失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    Loading,
    Loaded(Vec<(bool, String)>),
    Failed,
}

/// 全屏对话（⌃t）。`Transcript` 不比较内容：按会话 ID 与行数比较（只为 `Overlay` 的 `PartialEq`）。
#[derive(Debug, Clone)]
pub struct Shown {
    pub id: String,
    pub title: String,
    pub view: Box<Transcript>,
    pub scroll: usize,
}

impl PartialEq for Shown {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.scroll == other.scroll
            && self.view.rows().len() == other.view.rows().len()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptState {
    Loading(String),
    Shown(Shown),
}

/// 盖在列表上的东西：收这一页的按键与粘贴，画在列表上面或替换整页。
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// 改名：会话 ID 与输入的名字（搜索行变成输入框）。
    Rename { id: String, input: String },
    /// 删除确认。
    Confirm(Confirm),
    /// 别的目录的会话：先选目录。
    Cwd(CwdPrompt),
    /// 全屏对话（⌃t）：取快照中或已显示。
    Transcript(TranscriptState),
}

/// 删除确认（焦点 0 是取消：缺省焦点在取消上，误按 enter 不删）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub id: String,
    pub name: String,
    pub focus: usize,
}

/// 目录选择（焦点 0 是会话目录）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CwdPrompt {
    pub row: SessionRow,
    pub focus: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResumePage {
    /// 启动形态（`--resume` 不带参数）：esc 留在新会话，ctrl+c 退出。
    pub startup: bool,
    /// Filter：false 是当前目录，true 是全部。
    pub all: bool,
    /// Sort：false 是更新时间，true 是创建时间。
    pub created: bool,
    /// 工具栏的焦点：false 在 Filter，true 在 Sort。
    pub focus_sort: bool,
    pub comfortable: bool,
    /// None：还在取。
    pub rows: Option<Vec<SessionRow>>,
    pub more: bool,
    pub query: String,
    /// 过滤后的列表里焦点的下标与第一条可见的下标；上一帧列表的行数（翻页用）。
    pub selected: usize,
    pub scroll: usize,
    pub height: usize,
    pub expanded: Option<String>,
    pub previews: HashMap<String, Preview>,
    /// 盖在列表上的东西，一次只有一个（架构整理 U7：以前是 4 个 `Option`，互斥只靠按键的先后）。
    pub modal: Option<Modal>,
    /// 搜索行的红字。
    pub error: Option<String>,
    /// 本地时间的偏移（预览里的绝对时间，打开时由 runner 给出）。
    pub offset_minutes: i32,
}

impl ResumePage {
    /// 查询过滤后的行（比标题、首条输入、会话 ID、分支、路径）。
    pub fn filtered(&self) -> Vec<&SessionRow> {
        let query = self.query.to_lowercase();
        self.rows
            .iter()
            .flatten()
            .filter(|row| row.matches(&query, true))
            .collect()
    }

    pub fn focused(&self) -> Option<&SessionRow> {
        self.filtered().get(self.selected).copied()
    }

    fn clamp(&mut self) {
        let count = self.filtered().len();
        self.selected = self.selected.min(count.saturating_sub(1));
    }

    fn set_query(&mut self, query: String) {
        self.query = query;
        self.clamp();
    }

    fn order(&self) -> SessionOrder {
        if self.created {
            SessionOrder::Created
        } else {
            SessionOrder::Updated
        }
    }
}

/// 一条快照里最近几条有文字的对话：人类输入（界面原文）与助手回复。
pub fn preview_items(view: &SessionView) -> Vec<(bool, String)> {
    use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
    let text = |content: &[ContentBlock]| {
        content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.trim()),
                _ => None,
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut items: Vec<(bool, String)> = view
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) if user.origin == UserOrigin::Human => {
                super::session_rows::user_text(message).map(|text| (true, text))
            }
            Message::Assistant(assistant) => Some((false, text(&assistant.content))),
            _ => None,
        })
        .filter(|(_, text)| !text.trim().is_empty())
        .collect();
    let skip = items.len().saturating_sub(PREVIEW_ITEMS);
    items.drain(..skip);
    items
}

impl App {
    /// `/resume`：打开页面（Filter 当前目录、Sort 更新时间），数据由 runner 取回后填入。
    pub fn open_resume_page(&mut self, startup: bool, offset_minutes: i32) -> Action {
        self.overlay = Some(Overlay::Resume(Box::new(ResumePage {
            startup,
            offset_minutes,
            ..ResumePage::default()
        })));
        self.resume_request()
    }

    fn resume_request(&self) -> Action {
        Action::Projects(self.resume_reload().unwrap_or(Request::LoadResume {
            root: Some(self.view.workspace().root.as_str().to_owned()),
            order: SessionOrder::Updated,
        }))
    }

    /// 按页面现在的筛选与排序重取（改名之后）；页面没开时为 None。
    pub fn resume_reload(&self) -> Option<Request> {
        let Some(Overlay::Resume(page)) = &self.overlay else {
            return None;
        };
        Some(Request::LoadResume {
            root: (!page.all).then(|| self.view.workspace().root.as_str().to_owned()),
            order: page.order(),
        })
    }

    pub fn resume_loaded(&mut self, sessions: ProjectSessions) {
        let current = self.session.clone();
        if let Some(Overlay::Resume(page)) = self.overlay.as_mut() {
            page.rows = Some(
                sessions
                    .sessions
                    .iter()
                    .map(|session| SessionRow::new(session, current.as_deref()))
                    .collect(),
            );
            page.more = sessions.more;
            page.clamp();
        }
    }

    pub fn resume_error(&mut self, text: String) {
        if let Some(Overlay::Resume(page)) = self.overlay.as_mut() {
            page.error = Some(text);
        }
    }

    /// 预览或全屏对话的快照到了（页面关掉、换了会话时丢弃）。
    pub fn resume_snapshot(
        &mut self,
        id: &str,
        transcript: bool,
        view: Result<SessionView, String>,
        now_ms: u64,
    ) {
        let Some(Overlay::Resume(page)) = self.overlay.as_mut() else {
            return;
        };
        if !transcript {
            let preview = view.map_or(Preview::Failed, |view| {
                Preview::Loaded(preview_items(&view))
            });
            page.previews.insert(id.to_owned(), preview);
            return;
        }
        if page.modal != Some(Modal::Transcript(TranscriptState::Loading(id.to_owned()))) {
            return;
        }
        page.modal = match view {
            Ok(view) => {
                let title = page
                    .rows
                    .iter()
                    .flatten()
                    .find(|row| row.id == id)
                    .map(SessionRow::label)
                    .filter(|title| !title.is_empty())
                    .unwrap_or_else(|| id.to_owned());
                Some(Modal::Transcript(TranscriptState::Shown(Shown {
                    id: id.to_owned(),
                    title,
                    view: Box::new(Transcript::from_view(view, now_ms)),
                    scroll: 0,
                })))
            }
            Err(error) => {
                page.error = Some(error);
                None
            }
        };
    }

    /// 改名或删除之后：删除的行去掉（焦点留在原下标），改名的重取。
    pub fn resume_deleted(&mut self, id: &str) {
        if let Some(Overlay::Resume(page)) = self.overlay.as_mut() {
            if let Some(rows) = page.rows.as_mut() {
                rows.retain(|row| row.id != id);
            }
            page.previews.remove(id);
            if page.expanded.as_deref() == Some(id) {
                page.expanded = None;
            }
            page.clamp();
        }
    }

    pub(super) fn resume_key(&mut self, key: KeyEvent) -> Option<Action> {
        let lang = self.lang;
        let Some(Overlay::Resume(page)) = self.overlay.as_mut() else {
            return None;
        };
        // 文字输入不带 ctrl 与 alt（Windows 的 AltGr 算文字，修复线 F65，tui-input.md §9.1）。
        let ctrl = keyboard::ctrl(&key);
        let plain = keyboard::is_plain_text(&key);
        match page.modal.as_mut() {
            Some(Modal::Transcript(transcript)) => {
                if transcript_key(transcript, key, self.viewport.max(1)) {
                    page.modal = None;
                }
                return None;
            }
            Some(Modal::Cwd(prompt)) => {
                // 目录选择：↑/↓ 选，enter 继续，esc 用会话目录，ctrl+c 关闭页面。
                match key.code {
                    KeyCode::Up | KeyCode::Char('1') => prompt.focus = 0,
                    KeyCode::Down | KeyCode::Char('2') => prompt.focus = 1,
                    KeyCode::Char('c') if ctrl => {
                        self.overlay = None;
                        return None;
                    }
                    KeyCode::Enter | KeyCode::Esc => {
                        let current = key.code == KeyCode::Enter && prompt.focus == 1;
                        let row = prompt.row.clone();
                        page.modal = None;
                        let startup = page.startup;
                        let cd = current.then(|| self.view.workspace().root.as_str().to_owned());
                        return self.open_row(&row, cd, startup);
                    }
                    _ => {}
                }
                return None;
            }
            Some(Modal::Confirm(confirm)) => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('1') => confirm.focus = 0,
                    // 删除要明确确认：数字只移到删除，要再按 enter。
                    KeyCode::Down | KeyCode::Char('2') => confirm.focus = 1,
                    KeyCode::Esc => page.modal = None,
                    KeyCode::Char('c') if ctrl => page.modal = None,
                    KeyCode::Enter => {
                        let delete = (confirm.focus == 1).then(|| confirm.id.clone());
                        page.modal = None;
                        return delete.map(|id| Action::Projects(Request::Delete(id)));
                    }
                    _ => {}
                }
                return None;
            }
            Some(Modal::Rename { id, input }) => {
                page.error = None;
                match key.code {
                    KeyCode::Esc => page.modal = None,
                    KeyCode::Char('c') if ctrl => page.modal = None,
                    KeyCode::Enter => {
                        let (id, title) = (id.clone(), input.trim().to_owned());
                        page.modal = None;
                        return (!title.is_empty())
                            .then_some(Action::Projects(Request::Rename { id, title }));
                    }
                    KeyCode::Backspace => {
                        crate::ui::line_edit::backspace(input);
                    }
                    KeyCode::Char(c) if plain => input.push(c),
                    _ => {}
                }
                return None;
            }
            None => page.error = None,
        }
        let count = page.filtered().len();
        let step = page.height.max(1);
        match key.code {
            // 应用内的 ctrl+c 关闭页面；启动形态退出 zcode（启动时的选择页没有可回去的界面，⌃c 是退出）。
            KeyCode::Char('c') if ctrl => {
                let startup = page.startup;
                self.overlay = None;
                return startup.then_some(Action::Quit);
            }
            KeyCode::Esc if !page.query.is_empty() => page.set_query(String::new()),
            KeyCode::Esc => {
                self.overlay = None;
                return None;
            }
            KeyCode::Char('t') if ctrl => {
                let id = page.focused()?.id.clone();
                page.modal = Some(Modal::Transcript(TranscriptState::Loading(id.clone())));
                return Some(Action::Projects(Request::Transcript(id)));
            }
            KeyCode::Char('e') if ctrl => {
                let id = page.focused()?.id.clone();
                if page.expanded.as_deref() == Some(id.as_str()) {
                    page.expanded = None;
                    return None;
                }
                page.expanded = Some(id.clone());
                if !matches!(page.previews.get(&id), Some(Preview::Loaded(_))) {
                    page.previews.insert(id.clone(), Preview::Loading);
                    return Some(Action::Projects(Request::Preview(id)));
                }
            }
            KeyCode::Char('o') if ctrl => page.comfortable = !page.comfortable,
            KeyCode::Char('r') if ctrl => {
                if let Some(row) = page.focused() {
                    page.modal = Some(Modal::Rename {
                        id: row.id.clone(),
                        input: row.title.clone(),
                    });
                }
            }
            KeyCode::Char('d') if ctrl => {
                let row = page.focused()?.clone();
                if row.current {
                    page.error = Some(words::delete_current(lang).to_owned());
                } else {
                    let name = row.label();
                    page.modal = Some(Modal::Confirm(Confirm {
                        id: row.id,
                        name: if name.is_empty() {
                            words::no_message(lang).to_owned()
                        } else {
                            name
                        },
                        focus: 0,
                    }));
                }
            }
            KeyCode::Enter => {
                let row = page.focused()?.clone();
                if !row.current && row.root != self.view.workspace().root.as_str() {
                    page.modal = Some(Modal::Cwd(CwdPrompt { row, focus: 0 }));
                    return None;
                }
                let startup = page.startup;
                return self.open_row(&row, None, startup);
            }
            KeyCode::Up => page.selected = page.selected.saturating_sub(1),
            KeyCode::Char('p' | 'k') if ctrl => page.selected = page.selected.saturating_sub(1),
            KeyCode::Down => page.selected = (page.selected + 1).min(count.saturating_sub(1)),
            KeyCode::Char('n' | 'j') if ctrl => {
                page.selected = (page.selected + 1).min(count.saturating_sub(1));
            }
            KeyCode::PageUp => page.selected = page.selected.saturating_sub(step),
            KeyCode::Char('b') if ctrl => page.selected = page.selected.saturating_sub(step),
            KeyCode::PageDown => {
                page.selected = (page.selected + step).min(count.saturating_sub(1))
            }
            KeyCode::Char('f') if ctrl => {
                page.selected = (page.selected + step).min(count.saturating_sub(1));
            }
            KeyCode::Home => page.selected = 0,
            KeyCode::End => page.selected = count.saturating_sub(1),
            KeyCode::Tab | KeyCode::BackTab => page.focus_sort = !page.focus_sort,
            KeyCode::Left | KeyCode::Right => return self.resume_toggle(),
            KeyCode::Char('h' | 'l') if ctrl => return self.resume_toggle(),
            KeyCode::Backspace => {
                let mut query = page.query.clone();
                crate::ui::line_edit::backspace(&mut query);
                page.set_query(query);
            }
            KeyCode::Char(c) if plain => {
                let query = format!("{}{c}", page.query);
                page.set_query(query);
            }
            _ => {}
        }
        None
    }

    /// ←/→：切换焦点控件的值，从头重取。
    fn resume_toggle(&mut self) -> Option<Action> {
        let Some(Overlay::Resume(page)) = self.overlay.as_mut() else {
            return None;
        };
        if page.focus_sort {
            page.created = !page.created;
        } else {
            page.all = !page.all;
        }
        page.rows = None;
        page.selected = 0;
        page.scroll = 0;
        page.expanded = None;
        Some(self.resume_request())
    }

    /// 粘贴进查询（换行压成空格，查询只有一行）。
    pub(super) fn resume_paste(&mut self, text: &str) {
        let Some(Overlay::Resume(page)) = self.overlay.as_mut() else {
            return;
        };
        let pasted = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if pasted.is_empty() {
            return;
        }
        // 与按键同一层（架构整理 U7）：改名时进名字；确认框、目录选择与全屏对话盖住搜索行，丢掉。
        match page.modal.as_mut() {
            Some(Modal::Rename { input, .. }) => return input.push_str(&pasted),
            Some(Modal::Confirm(_) | Modal::Cwd(_) | Modal::Transcript(_)) => return,
            None => {}
        }
        let mut query = page.query.clone();
        if !query.is_empty() && !query.ends_with(char::is_whitespace) {
            query.push(' ');
        }
        query.push_str(&pasted);
        page.set_query(query);
    }

    /// 滚轮：焦点移动，到头停住（本工程列表的规则）。
    pub(super) fn resume_wheel(&mut self, rows: isize) {
        if let Some(Overlay::Resume(page)) = self.overlay.as_mut() {
            if let Some(Modal::Transcript(TranscriptState::Shown(shown))) = page.modal.as_mut() {
                shown.scroll = (shown.scroll as isize + rows).max(0) as usize;
                return;
            }
            let last = page.filtered().len().saturating_sub(1) as isize;
            page.selected = (page.selected as isize + rows).clamp(0, last) as usize;
        }
    }

    pub(super) fn resume_clicked(&mut self, index: usize) {
        if let Some(Overlay::Resume(page)) = self.overlay.as_mut() {
            page.selected = index;
            page.clamp();
        }
    }
}

/// 全屏对话的按键：只读，滚动与关闭（q / esc / ctrl+c / ctrl+t）。返回 true 时关闭。
fn transcript_key(transcript: &mut TranscriptState, key: KeyEvent, page_rows: usize) -> bool {
    let ctrl = keyboard::ctrl(&key);
    let close = matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
        || (ctrl && matches!(key.code, KeyCode::Char('c' | 't')));
    match transcript {
        _ if close => return true,
        TranscriptState::Loading(_) => {}
        TranscriptState::Shown(shown) => match key.code {
            KeyCode::End => shown.scroll = usize::MAX / 2,
            _ => {
                if let Some(step) = crate::ui::pager::page_move(&key) {
                    crate::ui::pager::scroll_from_top(&mut shown.scroll, step, page_rows);
                }
            }
        },
    }
    false
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
