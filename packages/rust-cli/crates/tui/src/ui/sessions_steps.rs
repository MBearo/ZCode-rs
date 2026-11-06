//! `/resume` 列表上旧界面才有的几步（tui-kit.md §4.2「`/resume`」与 §4.3；2026-10-31 主线裁定的保留集）：Puffin 的列表只有
//! ↑↓ ⏎ esc，旧界面还能改名、删除、看全部文件夹、读一个会话的对话。它们照 Puffin 最近的样子画，键只写在 `?` 表里，槽空着：
//!
//! - ⌃r 改名：草稿换成会话的名字（Puffin 的 `/rename` 也在草稿里写名字），板上一行 `Rename “名字”`；⏎ 改、esc 回到列表，
//!   草稿回到 `/resume 词`。
//! - ⌃d 删除：板上的一问（kit 请求的头行与编号的回答）`Delete “名字”?`，`1 Delete`、`2 Keep`；esc 回到列表。现在的会话
//!   不能删：输入框上方说 `Use /clear to leave the current session before deleting it.`。
//! - ⇥ 所有文件夹：列表读所有目录的会话，细节行先写会话的文件夹。⏎ 别的文件夹的会话时先问一次在哪里打开：
//!   `Open “名字” in which folder?`，`1 ~/别的  the session's folder`、`2 ~/这里  this folder`（以前 `/resume` 页的目录选择）。
//! - ⌃t 读对话：一页（正文换成它，底部栈留着），头一行 `Sessions › 名字`，行是主界面的块；↑↓ PgUp PgDn 翻，esc 回到列表。
//! - ⌃e 排序：Puffin 的活动时间（缺省）与创建时间之间换（以前 `/resume` 页的 Sort），按新的顺序重读。
//! - ⌃f 按状态筛：全部 → 等你回答 → 在跑 → 打开着空闲 → 没打开（以前项目页的 Filter），就地筛。
//! - ⌃b 新建：在选中的会话的文件夹新建会话（以前项目页的 `n`，tui-panels.md §2.4）；runner 先做信任检查，再同 `/new`。
//!
//! 排序与筛选在 Puffin 的列表上没有写的地方：画面上只是行的次序与多少不同；筛得一个不剩时，Puffin 写搜索词的那一句
//! `No session matches “…”.` 写这一步（[`SessionsList::said_query`]）。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::panels::SessionActivity;
use rust_cli_kernel_api::session_api::SessionView;
use rust_cli_kernel_api::store::SessionOrder;
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::asks::{
    Answer as KitAnswer, AnswerKind, answer_rows, head_row_said,
};
use rust_cli_tui_kit::app::widgets::key_hints;
use rust_cli_tui_kit::app::widgets::resume_list::State;
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::app::{Action, App, Overlay};
use super::cells::{self, Ctx};
use super::doorway::{Answer, answers};
use super::i18n::Lang;
use super::page_frame::PageBody;
use super::projects::Request;
use super::sessions_list::{Found, SessionsList};
use crate::view::Transcript;

/// 读到的一个会话（⌃t）。`Transcript` 不比较内容：按会话与行数比较（只为 `Overlay` 的 `PartialEq`）。
#[derive(Debug, Clone)]
pub struct Shown {
    pub title: String,
    pub view: Box<Transcript>,
    pub scroll: usize,
}

impl PartialEq for Shown {
    fn eq(&self, other: &Self) -> bool {
        self.title == other.title
            && self.scroll == other.scroll
            && self.view.rows().len() == other.view.rows().len()
    }
}

/// 列表上的这一步。`back`：改名之前的草稿，回到列表时放回去。
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Step {
    #[default]
    List,
    Rename {
        id: String,
        title: String,
        back: String,
    },
    Delete {
        id: String,
        title: String,
        choice: usize,
    },
    /// ⏎ 了别的文件夹的会话：在哪里打开。
    Folder {
        found: Box<Found>,
        choice: usize,
    },
    /// ⏎ 了活在别的进程的会话：去那里，或在这里读（tui-cockpit.md §3.3）。
    Elsewhere {
        found: Box<Found>,
        choice: usize,
    },
    /// 驾驶舱上的 `?`：这一页的键，任何键关上（tui-cockpit.md §3.1）。
    Keys,
    /// 等 runner 读回这个会话。
    Reading {
        id: String,
        title: String,
    },
    Read(Box<Shown>),
}

/// ⌃f 筛的状态（以前项目页 Filter 的四种；内核的 `SessionActivity`，没有时是没打开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    NeedsYou,
    Working,
    Ready,
    Inactive,
}

impl Status {
    /// ⌃f 的次序：全部（None）之后是这四种，再回到全部。
    pub fn next(now: Option<Self>) -> Option<Self> {
        match now {
            None => Some(Self::NeedsYou),
            Some(Self::NeedsYou) => Some(Self::Working),
            Some(Self::Working) => Some(Self::Ready),
            Some(Self::Ready) => Some(Self::Inactive),
            Some(Self::Inactive) => None,
        }
    }

    /// 内核给的状态是哪一种（tui-panels.md §2.1 的表）：内核没有给（只在库里）时是没打开。
    pub fn of(activity: Option<SessionActivity>) -> Self {
        match activity {
            Some(SessionActivity::NeedsInput) => Self::NeedsYou,
            Some(SessionActivity::Working) => Self::Working,
            Some(SessionActivity::Ready) => Self::Ready,
            None => Self::Inactive,
        }
    }

    pub fn admits(self, activity: Option<SessionActivity>) -> bool {
        Self::of(activity) == self
    }

    /// 看板上的样子（kit `resume_list::State`：记号、墨与字）。
    pub fn state(self) -> State {
        match self {
            Self::NeedsYou => State::NeedsYou,
            Self::Working => State::Working,
            Self::Ready => State::Ready,
            Self::Inactive => State::Inactive,
        }
    }

    /// 筛空时那一句里的词（同看板的字；kit 的句子只有英文）。
    fn word(self) -> &'static str {
        self.state().word()
    }
}

impl SessionsList {
    /// 这一步开着一页（⌃t）：板上没有层。
    pub fn reading(&self) -> bool {
        matches!(self.step, Step::Reading { .. } | Step::Read(_))
    }

    /// 筛得一个不剩时 `No session matches “…”.` 里的字：搜索词，⌃f 选了状态时加上它（`“词”, “working”`）。
    pub fn said_query(&self) -> String {
        match self.status {
            None => self.query.clone(),
            Some(s) if self.query.is_empty() => s.word().to_owned(),
            Some(s) => format!("{}”, “{}", self.query, s.word()),
        }
    }

    /// 按现在的范围、顺序与词重读（⇥、⌃e）：整个列表，有词时还有词的回答。换上之前旧的行留着。
    fn reload(&self, here: String) -> Action {
        let root = (!self.folders).then_some(here);
        let order = self.order;
        let mut load = vec![Action::Projects(Request::LoadResume {
            root: root.clone(),
            order,
        })];
        // 驾驶舱的词在 App 里筛，不问 runner（tui-cockpit.md §2.3）。
        if !self.query.is_empty() && self.cockpit.is_none() {
            load.push(Action::Projects(Request::Search {
                query: self.query.clone(),
                root,
                order,
            }));
        }
        Action::Many(load)
    }
}

impl App {
    fn steps_mut(&mut self) -> Option<&mut SessionsList> {
        match self.overlay.as_mut() {
            Some(Overlay::Sessions(list)) => Some(list),
            _ => None,
        }
    }

    /// 列表上 ⌃r ⌃d ⌃t ⇥ ⌃e ⌃f ⌃b：开始这一步（`sessions_key` 先问它）。不是这几个键时为 None。
    pub(super) fn sessions_step_start(&mut self, key: &KeyEvent) -> Option<Option<Action>> {
        let ctrl = crate::ui::keyboard::ctrl(key);
        let root = self.view.workspace().root.as_str().to_owned();
        let draft = self.composer.area.text().to_owned();
        let current = self.session.clone();
        let lang = self.lang;
        let list = self.steps_mut()?;
        if key.code == KeyCode::Tab {
            list.folders = !list.folders;
            list.all = None;
            list.found = None;
            list.choice = 0;
            return Some(Some(list.reload(root)));
        }
        if !ctrl {
            return None;
        }
        match key.code {
            // ⌃e：换顺序，按新的顺序重读（选中同打开时，`sessions_loaded`）。
            KeyCode::Char('e') => {
                list.order = match list.order {
                    SessionOrder::Updated => SessionOrder::Created,
                    SessionOrder::Created => SessionOrder::Updated,
                };
                return Some(Some(list.reload(root)));
            }
            // ⌃f：下一种状态，就地筛；选中的会话还在就留着它。列表空着时也能换（筛空了要能换回来）。
            KeyCode::Char('f') => {
                let was = list.chosen_found().map(|x| x.id.clone());
                list.status = Status::next(list.status);
                list.choice = was
                    .and_then(|id| list.matches().iter().position(|x| x.id == id))
                    .unwrap_or(0);
                return Some(None);
            }
            // ⌃b：在选中的会话的文件夹新建会话（没有行时是这个工作区）。列表关上、草稿清空，同 ⏎ 打开；信任检查与离开
            // 当前会话在 runner（Request::New，tui-panels.md §2.4）。
            KeyCode::Char('b') => {
                let root = list.chosen_found().map_or(root, |x| x.root.clone());
                self.overlay = None;
                self.composer.clear();
                return Some(Some(Action::Projects(Request::New { root })));
            }
            _ => {}
        }
        let chosen = list.chosen_found()?.clone();
        let (id, title) = (chosen.id, chosen.title);
        match key.code {
            KeyCode::Char('r') => {
                list.step = Step::Rename {
                    id,
                    title: title.clone(),
                    back: draft,
                };
                self.composer.set_text(&title);
                Some(None)
            }
            KeyCode::Char('d') if current.as_deref() == Some(id.as_str()) => {
                self.error(super::i18n_projects::delete_current(lang).to_owned());
                Some(None)
            }
            KeyCode::Char('d') => {
                list.step = Step::Delete {
                    id,
                    title,
                    choice: 0,
                };
                Some(None)
            }
            KeyCode::Char('t') => {
                list.step = Step::Reading {
                    id: id.clone(),
                    title,
                };
                Some(Some(Action::Projects(Request::Transcript(id))))
            }
            _ => None,
        }
    }

    /// 一步开着时的键：`None` 让键去编辑草稿（只在改名时）。
    pub(super) fn sessions_step_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let ctrl = crate::ui::keyboard::ctrl(&key);
        let text = self.composer.area.text().trim().to_owned();
        let here = self.view.workspace().root.as_str().to_owned();
        // 按上一帧页的几何滚动，夹到最后一屏（同 `/stats all` 的页，keys.rs）。
        let (rows, total) = self.page_rows;
        let max_top = total.saturating_sub(rows);
        let page = rows.saturating_sub(1).max(1);
        let socket = self.tmux_socket.clone();
        let list = self.steps_mut()?;
        match &mut list.step {
            Step::List => None,
            Step::Rename { id, title, back } => {
                let (id, old, back) = (id.clone(), title.clone(), back.clone());
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Enter => {}
                    _ => return None,
                }
                list.step = Step::List;
                self.composer.set_text(&back);
                let save = key.code == KeyCode::Enter && !text.is_empty() && text != old;
                Some(save.then_some(Action::Projects(Request::Rename { id, title: text })))
            }
            Step::Delete { id, choice, .. } => {
                if key.code == KeyCode::Esc {
                    list.step = Step::List;
                    return Some(None);
                }
                match answers(&key, ctrl, 2, *choice, true) {
                    Answer::Move(i) => *choice = i,
                    Answer::Act(0) => {
                        let id = id.clone();
                        list.step = Step::List;
                        return Some(Some(Action::Projects(Request::Delete(id))));
                    }
                    Answer::Act(_) => list.step = Step::List,
                    Answer::Pass => {}
                }
                Some(None)
            }
            Step::Folder { found, choice } => {
                if key.code == KeyCode::Esc {
                    list.step = Step::List;
                    return Some(None);
                }
                let at = match answers(&key, ctrl, 2, *choice, true) {
                    Answer::Move(i) => {
                        *choice = i;
                        return Some(None);
                    }
                    Answer::Act(i) => i,
                    Answer::Pass => return Some(None),
                };
                let (x, startup) = ((**found).clone(), list.startup);
                self.overlay = None;
                self.composer.clear();
                let cd = (at == 1).then_some(here);
                Some(Some(self.open_found(x, cd, startup)))
            }
            Step::Keys => {
                list.step = Step::List;
                Some(None)
            }
            Step::Elsewhere { found, choice } => {
                if key.code == KeyCode::Esc {
                    list.step = Step::List;
                    return Some(None);
                }
                let place = found
                    .presence
                    .as_ref()
                    .and_then(|presence| presence.tmux.clone())
                    .filter(|place| Some(&place.socket) == socket.as_ref());
                let n = if place.is_some() { 2 } else { 1 };
                let at = match answers(&key, ctrl, n, *choice, true) {
                    Answer::Move(i) => {
                        *choice = i;
                        return Some(None);
                    }
                    Answer::Act(i) => i,
                    Answer::Pass => return Some(None),
                };
                let (id, title) = (found.id.clone(), found.title.clone());
                match place.filter(|_| at == 0) {
                    Some(place) => {
                        list.step = Step::List;
                        Some(Some(Action::Projects(Request::GoTo(place))))
                    }
                    None => {
                        list.step = Step::Reading {
                            id: id.clone(),
                            title,
                        };
                        Some(Some(Action::Projects(Request::Transcript(id))))
                    }
                }
            }
            Step::Reading { .. } | Step::Read(_) => {
                let scroll = match &mut list.step {
                    Step::Read(shown) => Some(&mut shown.scroll),
                    _ => None,
                };
                match (key.code, scroll) {
                    (KeyCode::Esc, _) => list.step = Step::List,
                    (KeyCode::Up, Some(s)) => *s = (*s).min(max_top).saturating_sub(1),
                    (KeyCode::Down, Some(s)) => *s = ((*s).min(max_top) + 1).min(max_top),
                    (KeyCode::PageUp, Some(s)) => *s = (*s).min(max_top).saturating_sub(page),
                    (KeyCode::PageDown, Some(s)) => *s = ((*s).min(max_top) + page).min(max_top),
                    (KeyCode::Home, Some(s)) => *s = 0,
                    (KeyCode::End, Some(s)) => *s = max_top,
                    _ => {}
                }
                Some(None)
            }
        }
    }

    /// 滚轮在读的页上（`wheel.rs`）：翻 `rows` 行；没开着页时为 false。
    pub(super) fn sessions_page_wheel(&mut self, rows: isize) -> bool {
        let (shown_rows, total) = self.page_rows;
        let max_top = total.saturating_sub(shown_rows);
        let Some(list) = self.steps_mut() else {
            return false;
        };
        let Step::Read(shown) = &mut list.step else {
            return false;
        };
        shown.scroll = shown
            .scroll
            .min(max_top)
            .saturating_add_signed(rows)
            .min(max_top);
        true
    }

    /// runner 读回的会话（⌃t）：还在等它时换上；失败时回到列表，输入框上方说原因。
    pub fn sessions_read(&mut self, id: &str, view: Result<SessionView, String>, now_ms: u64) {
        let Some(list) = self.steps_mut() else {
            return;
        };
        let Step::Reading { id: wanted, title } = &list.step else {
            return;
        };
        if wanted != id {
            return;
        }
        match view {
            Ok(view) => {
                list.step = Step::Read(Box::new(Shown {
                    title: title.clone(),
                    view: Box::new(Transcript::from_view(view, now_ms)),
                    scroll: 0,
                }));
            }
            Err(error) => {
                list.step = Step::List;
                self.error(error);
            }
        }
    }

    /// 这一步在板上的行；在列表上时为 None（画列表）。
    pub(super) fn sessions_step_rows(&self, list: &SessionsList) -> Option<Vec<Element>> {
        let theme = &self.ink.theme;
        let zh = self.lang == Lang::Zh;
        let pick = |en: String, cn: String| if zh { cn } else { en };
        let answer = |id: &str, label: String, desc: String| KitAnswer {
            id: id.to_owned(),
            kind: AnswerKind::Keep,
            label,
            desc: vec![(desc, theme.fg3)],
        };
        let ask = |head: String, choice: usize, answers: Vec<KitAnswer>| {
            let mut rows = vec![head_row_said(theme, &head, None, None)];
            rows.extend(answer_rows(theme, &answers, choice, 1.0));
            rows
        };
        match &list.step {
            Step::List | Step::Reading { .. } | Step::Read(_) => None,
            Step::Rename { title, .. } => Some(vec![head_row_said(
                theme,
                &pick(format!("Rename “{title}”"), format!("改名“{title}”")),
                None,
                None,
            )]),
            Step::Delete { title, choice, .. } => Some(ask(
                pick(format!("Delete “{title}”?"), format!("删除“{title}”？")),
                *choice,
                vec![
                    answer(
                        "delete",
                        pick("Delete".into(), "删除".into()),
                        pick("it can't be opened again".into(), "删除后不能再打开".into()),
                    ),
                    answer("keep", pick("Keep".into(), "留着".into()), String::new()),
                ],
            )),
            Step::Keys => Some(self.cockpit_keys_rows()),
            Step::Elsewhere { found, choice } => {
                let title = &found.title;
                let tmux = found
                    .presence
                    .as_ref()
                    .and_then(|presence| presence.tmux.as_ref())
                    .filter(|place| Some(&place.socket) == self.tmux_socket.as_ref());
                let mut options = Vec::new();
                if let Some(place) = tmux {
                    options.push(answer(
                        "go",
                        pick("Go to it".into(), "去那里".into()),
                        pick(
                            format!("tmux {}", place.pane),
                            format!("tmux {}", place.pane),
                        ),
                    ));
                }
                options.push(answer(
                    "read",
                    pick("Read it here".into(), "在这里读".into()),
                    pick(
                        "read only, it keeps running there".into(),
                        "只读，它在那里接着跑".into(),
                    ),
                ));
                Some(ask(
                    pick(
                        format!("“{title}” is running in another terminal."),
                        format!("“{title}”在别的终端里跑着。"),
                    ),
                    *choice,
                    options,
                ))
            }
            Step::Folder { found, choice } => {
                let here =
                    super::app::label(self.view.workspace().root.as_str(), self.home.as_deref());
                let title = &found.title;
                Some(ask(
                    pick(
                        format!("Open “{title}” in which folder?"),
                        format!("在哪个文件夹打开“{title}”？"),
                    ),
                    *choice,
                    vec![
                        answer(
                            "session",
                            found.folder.clone(),
                            pick("the session's folder".into(), "会话的文件夹".into()),
                        ),
                        answer(
                            "here",
                            here,
                            pick("this folder".into(), "这个文件夹".into()),
                        ),
                    ],
                ))
            }
        }
    }

    /// 这一步的槽（Puffin 的列表槽空着；问与改名的槽是它们的键，明显的不写）。
    pub(super) fn sessions_step_slot(&self, list: &SessionsList) -> Vec<Span> {
        let keys: &[(&str, &str)] = match &list.step {
            Step::Rename { .. } => &[("⏎", "rename"), ("esc", "back")],
            Step::Delete { .. } | Step::Folder { .. } | Step::Elsewhere { .. } => {
                &[("esc", "back")]
            }
            _ => &[],
        };
        let keys: Vec<(String, String)> = keys
            .iter()
            .map(|(k, l)| ((*k).to_owned(), (*l).to_owned()))
            .collect();
        key_hints::spans(&self.ink.theme, &key_hints::pick(&keys))
    }

    /// ⌃t 的页（`page_frame.rs`）：头一行 `Sessions › 名字`，然后是这个会话的块（主界面同一套排版）。读回之前只有头一行。
    pub fn sessions_page_body(&self, width: usize, now_ms: u64) -> Option<PageBody> {
        let Some(Overlay::Sessions(list)) = &self.overlay else {
            return None;
        };
        let (title, shown) = match &list.step {
            Step::Reading { title, .. } => (title.as_str(), None),
            Step::Read(shown) => (shown.title.as_str(), Some(shown)),
            _ => return None,
        };
        let mut lines = vec![
            StyledLine::EMPTY,
            super::page::head(
                &self.ink,
                if self.lang == Lang::Zh {
                    "会话"
                } else {
                    "Sessions"
                },
                &[title],
            ),
            StyledLine::EMPTY,
        ];
        if let Some(shown) = shown {
            lines.extend(self.read_lines(&shown.view, width, now_ms));
        }
        Some(PageBody {
            lines,
            top: shown.map_or(0, |s| s.scroll),
            slot: Some(Vec::new()),
            owns_keys: true,
            fit: false,
        })
    }

    /// 一个会话的全部行（块之间空一行，同主界面）。
    fn read_lines(&self, view: &Transcript, width: usize, now_ms: u64) -> Vec<StyledLine> {
        let workspace = view.workspace().root.as_str().to_owned();
        let label = super::app::label(&workspace, self.home.as_deref());
        let ctx = Ctx {
            ink: &self.ink,
            lang: self.lang,
            width,
            expanded: false,
            elapsed: std::time::Duration::ZERO,
            now_ms,
            workspace: &workspace,
            workspace_label: &label,
            version: &self.version,
            mcp_machines: &self.mcp_machines,
            machines: &self.machines,
            skills: &[],
            markdown: None,
        };
        let mut out: Vec<StyledLine> = Vec::new();
        for cell in cells::build(view.rows()) {
            let rendered = cells::lines(&cell, &ctx);
            if rendered.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(StyledLine::EMPTY);
            }
            out.extend(rendered);
        }
        out
    }
}

#[cfg(test)]
#[path = "sessions_steps_tests.rs"]
mod tests;
