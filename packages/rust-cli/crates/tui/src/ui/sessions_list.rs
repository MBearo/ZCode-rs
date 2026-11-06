//! `/resume` 的会话列表（docs/specs/rust-cli/tui-manage.md §10；tui-kit.md §4.2「`/resume`」；Puffin `resume_ui.rs`）：板上
//! 输入框上方的一层，跟着草稿 `/resume <词>`。行是 kit 的 `resume_list::rows`（年龄、记号、标题、`current`、选中的细节行，
//! 细节变了时淡入）。整个列表是内核这个目录的 `project_sessions`；有词时先按标题筛，runner 按 Puffin host 的同一条规则
//! （kit `resume_list::search_hit`）回答这个词之后换成它的回答。↑↓ 选、⏎ 打开、esc 关上（草稿留着）。旧界面才有的改名、
//! 删除、所有文件夹、读对话、排序与按状态筛在 `sessions_steps.rs`；⇥ 列所有文件夹（以前是项目页；← 现在是驾驶舱，tui-cockpit.md），列所有
//! 文件夹时会话按文件夹分组（[`by_folder`]），组名画法同 `@` 面板（kit `resume_list::grouped_rows`），并且是以前项目页的看板
//! （每行的状态、各状态的数量、分支与首条输入、最后的回复，`sessions_status.rs`；tui-panels.md §2）。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::panels::{ProjectSession, ProjectSessions, SessionActivity};
use rust_cli_kernel_api::store::SessionOrder;
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::lists::plain_text;
use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;
use rust_cli_tui_kit::app::widgets::resume_list::{self, Mark, Session, search_hit};

use super::app::{Action, App, Overlay};
use super::projects::Request;

/// 列表跟着的草稿。
pub const DRAFT: &str = "/resume ";

/// 草稿里 `/resume ` 之后的词（Puffin `resume_query`）。
pub fn query_of(text: &str) -> String {
    text.chars()
        .skip(DRAFT.len())
        .collect::<String>()
        .trim()
        .to_owned()
}

/// 列表的一个会话。`snippet`：搜索时命中的那一段（标题含搜索词时没有）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: String,
    pub root: String,
    pub title: String,
    pub created: u64,
    pub updated: u64,
    pub waiting: bool,
    /// 内核给的状态（打开着的会话才有）：列表的 ⌃f 按它筛（sessions_steps.rs）。
    pub activity: Option<SessionActivity>,
    pub snippet: Option<String>,
    /// 会话的文件夹按 `~` 写短（列表读所有文件夹时细节行先写它）；`found_of` 给空，App 收下时填。
    pub folder: String,
    /// 看板的细节（列所有文件夹时，tui-panels.md §2.2）：目录现在的分支、首条输入、最后的回复（打开着的会话才有）。
    pub branch: Option<String>,
    pub prompt: String,
    pub reply: String,
    /// 登记处里活着的进程开着它（tui-cockpit.md §1）。
    pub presence: Option<rust_cli_kernel_api::panels::SessionPresence>,
    /// 回合结束与看过的笔记（驾驶舱，收下时填）。
    pub note: crate::services::CockpitNote,
}

/// 内核的一个会话在搜索 `search`（小写、去掉首尾空白）下的行；不命中时为 None（cli 的 Puffin host `list_item` 的规则）。
pub fn found_of(session: &ProjectSession, search: &str) -> Option<Found> {
    let record = &session.record;
    let first = record.first_prompt.clone().unwrap_or_default();
    let last = session.last_reply.clone().unwrap_or_default();
    let snippet = search_hit(&record.title, &first, &last, search)?;
    let title = if record.title.is_empty() {
        first.lines().next().unwrap_or_default().to_owned()
    } else {
        record.title.clone()
    };
    Some(Found {
        id: record.id.as_str().to_owned(),
        root: record.root.as_str().to_owned(),
        title,
        created: record.created_at_ms,
        updated: record.updated_at_ms,
        waiting: session.activity == Some(SessionActivity::NeedsInput),
        activity: session.activity,
        snippet,
        folder: String::new(),
        branch: session.branch.clone(),
        prompt: first,
        reply: last,
        presence: session.presence.clone(),
        note: Default::default(),
    })
}

/// 开着的列表：整个列表（读到之前是 None）、草稿里的词、这个词的回答、选中的行、细节行与它变了的时刻、`--resume` 启动时。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionsList {
    pub all: Option<Vec<Found>>,
    pub query: String,
    pub found: Option<(String, Vec<Found>)>,
    pub choice: usize,
    pub startup: bool,
    /// ⇥：列所有文件夹的会话（不只是这个文件夹，sessions_steps.rs）。
    pub folders: bool,
    /// 列表上的这一步（改名、删除、读对话）。
    pub step: super::sessions_steps::Step,
    /// ⌃e：问内核要的顺序（缺省是 Puffin 的活动时间，sessions_steps.rs）。
    pub order: SessionOrder,
    /// ⌃f：按状态筛（None 是全部，同 Puffin；sessions_steps.rs）。
    pub status: Option<super::sessions_steps::Status>,
    /// 上一帧的细节行与它出现的时刻（`note`）。
    pub(super) detail: Option<(String, Instant)>,
    /// 开着的是 ← 的驾驶舱（cockpit.rs）：同一个列表，换了次序、画法与键。
    pub cockpit: Option<Box<super::cockpit::Cockpit>>,
}

impl SessionsList {
    /// 显示的会话：没有词时是全部；有词时是这个词的回答（到了的话），之前是标题含它的（Puffin `resume_matches`）。⌃f 选了
    /// 状态时再按它筛（sessions_steps.rs）。
    pub fn matches(&self) -> Vec<&Found> {
        if let Some(cockpit) = &self.cockpit {
            return self.cockpit_order(cockpit);
        }
        let shown: Vec<&Found> = self
            .searched()
            .into_iter()
            .filter(|x| self.status.is_none_or(|s| s.admits(x.activity)))
            .collect();
        // 列所有文件夹时按文件夹分组（以前项目页的分组）：↑↓、⌃f 留着的选中与窗口都按分组后的次序。
        if self.folders {
            by_folder(shown)
        } else {
            shown
        }
    }

    /// ⌃f 筛之前的会话：没有词时是全部，有词时是词的回答（或标题含它的）。看板的数量数它们（sessions_status.rs）。
    pub(super) fn searched(&self) -> Vec<&Found> {
        let Some(all) = &self.all else {
            return Vec::new();
        };
        if self.query.is_empty() {
            all.iter().collect()
        } else {
            match self.found.as_ref().filter(|(q, _)| *q == self.query) {
                Some((_, found)) => found.iter().collect(),
                None => {
                    let lower = self.query.to_lowercase();
                    all.iter()
                        .filter(|x| x.title.to_lowercase().contains(&lower))
                        .collect()
                }
            }
        }
    }

    pub(super) fn chosen_found(&self) -> Option<&Found> {
        self.chosen()
    }

    fn chosen(&self) -> Option<&Found> {
        let ms = self.matches();
        let last = ms.len().checked_sub(1)?;
        Some(ms[self.choice.min(last)])
    }
}

/// 按文件夹分组（tui-manage.md §10.0）：最近活动最新的文件夹在前（组里最新的 `updated`，一样时先出现的在前），组里保留
/// 进来的次序（⌃e 的顺序、搜索的回答）。
pub fn by_folder(shown: Vec<&Found>) -> Vec<&Found> {
    let mut groups: Vec<(u64, Vec<&Found>)> = Vec::new();
    for x in shown {
        match groups.iter_mut().find(|(_, g)| g[0].root == x.root) {
            Some((latest, g)) => {
                *latest = (*latest).max(x.updated);
                g.push(x);
            }
            None => groups.push((x.updated, vec![x])),
        }
    }
    // 稳定排序：一样新的文件夹按先出现的次序。
    groups.sort_by_key(|g| std::cmp::Reverse(g.0));
    groups.into_iter().flat_map(|(_, g)| g).collect()
}

pub(super) fn kit_session<'a>(x: &'a Found, current: Option<&str>, folders: bool) -> Session<'a> {
    Session {
        title: x.title.clone(),
        created_at: x.created as f64,
        updated_at: x.updated as f64,
        model: "",
        effort: "",
        mark: if x.waiting { Mark::Waiting } else { Mark::None },
        changes: None,
        snippet: x.snippet.as_deref(),
        current: current == Some(x.id.as_str()),
        folder: folders.then_some(x.folder.as_str()),
        // 看板只在列所有文件夹时有；只列这个文件夹时同 Puffin。
        dash: folders.then(|| super::sessions_status::dash(x)),
    }
}

impl App {
    fn sessions_mut(&mut self) -> Option<&mut SessionsList> {
        match self.overlay.as_mut() {
            Some(Overlay::Sessions(list)) => Some(list),
            _ => None,
        }
    }

    /// `/resume`：草稿空着时写成 `/resume `，打开列表并请 runner 读这个目录的会话（草稿已有词时也搜它）。
    pub fn open_sessions(&mut self, startup: bool) -> Action {
        self.open_sessions_in(startup, false)
    }

    fn open_sessions_in(&mut self, startup: bool, folders: bool) -> Action {
        if self.composer.is_empty() {
            self.composer.set_text(DRAFT);
        }
        let query = query_of(self.composer.area.text());
        self.overlay = Some(Overlay::Sessions(Box::new(SessionsList {
            query: query.clone(),
            startup,
            folders,
            ..SessionsList::default()
        })));
        let root = (!folders).then(|| self.view.workspace().root.as_str().to_owned());
        let order = SessionOrder::default();
        let load = Action::Projects(Request::LoadResume {
            root: root.clone(),
            order,
        });
        if query.is_empty() {
            load
        } else {
            Action::Many(vec![
                load,
                Action::Projects(Request::Search { query, root, order }),
            ])
        }
    }

    /// 列表现在读的目录（None 是所有文件夹）。
    pub(super) fn sessions_root(&self) -> Option<String> {
        match &self.overlay {
            Some(Overlay::Sessions(list)) if list.folders => None,
            _ => Some(self.view.workspace().root.as_str().to_owned()),
        }
    }

    /// 改名、删除之后重读开着的列表（runner）；列表没开着时为 None。
    pub fn sessions_reload(&self) -> Option<Request> {
        let Some(Overlay::Sessions(list)) = &self.overlay else {
            return None;
        };
        Some(Request::LoadResume {
            root: self.sessions_root(),
            order: list.order,
        })
    }

    /// 收下的会话填上文件夹（按 `~` 写短）。
    pub(super) fn with_folders(&self, mut found: Vec<Found>) -> Vec<Found> {
        let notes = match &self.overlay {
            Some(Overlay::Sessions(list)) => list.cockpit.as_ref().map(|c| &c.notes),
            _ => None,
        };
        for x in &mut found {
            x.folder = super::app::label(&x.root, self.home.as_deref());
            if let Some(note) = notes.and_then(|notes| notes.get(&x.id)) {
                x.note = note.clone();
            }
        }
        found
    }

    /// 整个列表到了：选中最新的别的会话（没有词时，Puffin `listed`）。
    pub fn sessions_loaded(&mut self, sessions: &ProjectSessions) {
        let current = self.session.clone();
        let all = self.with_folders(
            sessions
                .sessions
                .iter()
                .filter_map(|s| found_of(s, ""))
                .collect(),
        );
        let Some(list) = self.sessions_mut() else {
            return;
        };
        list.choice = if list.cockpit.is_some() {
            0
        } else if list.query.is_empty() {
            all.iter()
                .position(|x| current.as_deref() != Some(x.id.as_str()))
                .unwrap_or(0)
        } else {
            0
        };
        list.all = Some(all);
    }

    /// 词的回答：还是草稿里的词时换上，选中的会话还在就留着它。
    pub fn sessions_found(&mut self, query: String, found: Vec<Found>) {
        let found = self.with_folders(found);
        let Some(list) = self.sessions_mut() else {
            return;
        };
        if query.is_empty() || query != list.query {
            return;
        }
        let was = list.chosen().map(|x| x.id.clone());
        list.found = Some((query, found));
        list.choice = was
            .and_then(|id| list.matches().iter().position(|x| x.id == id))
            .unwrap_or(0);
    }

    /// 列表拿的键（`draft_layer.rs` 先问它）：`None` 让键去编辑草稿。
    pub(super) fn sessions_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        if self
            .sessions_mut()
            .is_some_and(|l| l.step != super::sessions_steps::Step::List)
        {
            return self.sessions_step_key(key);
        }
        if self.cockpit_open() {
            if let Some(done) = self.cockpit_key(&key) {
                return Some(done);
            }
            // 现在的会话的请求拿键（tui-cockpit.md §3.2）。
            if self.asking() {
                return None;
            }
        }
        if let Some(done) = self.sessions_step_start(&key) {
            return Some(done);
        }
        let ctrl = crate::ui::keyboard::ctrl(&key);
        let current = self.session.clone();
        let here = self.view.workspace().root.as_str().to_owned();
        let empty = self.composer.area.is_empty();
        let list = self.sessions_mut()?;
        let n = list.matches().len();
        // 驾驶舱上词为空时 → 也打开（tui-cockpit.md §3.1）；有词时 → 移动光标。
        let opens = key.code == KeyCode::Enter
            || (key.code == KeyCode::Right && list.cockpit.is_some() && empty);
        match key.code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Up if n > 0 => list.choice = (list.choice.min(n - 1) + n - 1) % n,
            KeyCode::Char('p') if ctrl && n > 0 => {
                list.choice = (list.choice.min(n - 1) + n - 1) % n
            }
            KeyCode::Down if n > 0 => list.choice = (list.choice.min(n - 1) + 1) % n,
            KeyCode::Char('n') if ctrl && n > 0 => list.choice = (list.choice.min(n - 1) + 1) % n,
            KeyCode::Up | KeyCode::Down | KeyCode::Tab => {}
            KeyCode::Char('p' | 'n') if ctrl => {}
            KeyCode::Enter | KeyCode::Right if opens => {
                let chosen = list.chosen().cloned();
                let startup = list.startup;
                // 活在别的进程的会话：会话库假定一个 owner，在这里打开会有两个写者（tui-cockpit.md §1.3），所以先问。
                if let Some(x) = chosen.as_ref().filter(|x| {
                    current.as_deref() != Some(x.id.as_str())
                        && x.presence.as_ref().is_some_and(|presence| !presence.here)
                }) {
                    list.step = super::sessions_steps::Step::Elsewhere {
                        found: Box::new(x.clone()),
                        choice: 0,
                    };
                    return Some(None);
                }
                // 别的文件夹的会话（⇥ 所有文件夹时才有）：先问在哪个文件夹打开（sessions_steps.rs）。
                if let Some(x) = chosen
                    .as_ref()
                    .filter(|x| current.as_deref() != Some(x.id.as_str()) && x.root != here)
                {
                    list.step = super::sessions_steps::Step::Folder {
                        found: Box::new(x.clone()),
                        choice: 0,
                    };
                    return Some(None);
                }
                if self.cockpit_open()
                    && chosen
                        .as_ref()
                        .is_some_and(|x| current.as_deref() == Some(x.id.as_str()))
                {
                    return Some(self.cockpit_leave());
                }
                // 打开选中的：列表关上、草稿清空；现在的会话只关上列表（Puffin `open_chosen`）。
                self.overlay = None;
                self.composer.clear();
                let x = chosen?;
                if current.as_deref() == Some(x.id.as_str()) {
                    return Some(None);
                }
                return Some(Some(self.open_found(x, None, startup)));
            }
            _ => return None,
        }
        Some(None)
    }

    /// 打开一个会话（列表已关上）：输入框上方说 `Opening …`，到了以后说它多久以前用过（`resumed_into`）。`cd`：打开后换到
    /// 这个目录（别的文件夹的会话选了「这个文件夹」）。
    pub(super) fn open_found(&mut self, x: Found, cd: Option<String>, startup: bool) -> Action {
        self.said.quiet = Some(format!("Opening “{}”…", x.title));
        self.opening = Some(x.updated);
        Action::Projects(Request::Open {
            id: x.id,
            root: x.root,
            cd,
            startup,
        })
    }

    /// 一个键或粘贴之后：草稿不再以 `/resume ` 开头时关上；词变了时选第一行、请 runner 搜这个词。
    pub(super) fn settle_sessions(&mut self, text: &str) -> Option<Action> {
        // 改名时草稿是新名字，不是 `/resume 词`。
        if self
            .sessions_mut()
            .is_some_and(|l| l.step != super::sessions_steps::Step::List)
        {
            return None;
        }
        // 驾驶舱的草稿就是词（tui-cockpit.md §2.3）。
        if self.cockpit_open() {
            return self.cockpit_settle(text);
        }
        let root = self.sessions_root();
        let starts = text
            .get(..DRAFT.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(DRAFT));
        if !starts {
            self.overlay = None;
            return None;
        }
        let q = query_of(text);
        let list = self.sessions_mut()?;
        if q == list.query {
            return None;
        }
        list.query = q.clone();
        list.choice = 0;
        let order = list.order;
        (!q.is_empty()).then_some(Action::Projects(Request::Search {
            query: q,
            root,
            order,
        }))
    }

    /// 画之前记下这一帧的细节行：变了的那一刻起淡入（Puffin `refresh_detail` 的 `named`）。
    pub(super) fn note_sessions(&mut self, now: Instant) {
        let theme = self.ink.theme;
        let current = self.session.clone();
        let zone = self.zone.clone();
        let Some(list) = self.sessions_mut() else {
            return;
        };
        if list.cockpit.is_some() {
            App::note_cockpit(list, now);
            return;
        }
        // 看板的两行（首条输入、最后的回复）与细节行一起淡入：一起记。
        let text = list.chosen().map_or_else(String::new, |x| {
            let session = kit_session(x, current.as_deref(), list.folders);
            let mut text = plain_text(&resume_list::detail_parts(
                &theme,
                &session,
                &list.query,
                usize::MAX,
                &*zone,
            ));
            for line in resume_list::dash_lines(&theme, &session, &list.query, usize::MAX) {
                text.push('\n');
                text.push_str(&plain_text(&line));
            }
            text
        });
        if list.detail.as_ref().is_none_or(|(was, _)| *was != text) {
            list.detail = Some((text, now));
        }
    }

    /// 列表的行（kit `resume_list::rows`）；读到列表之前什么都没有。
    pub(super) fn sessions_rows(&self, width: usize, now: Instant, now_ms: u64) -> Vec<Element> {
        let Some(Overlay::Sessions(list)) = &self.overlay else {
            return Vec::new();
        };
        if let Some(rows) = self.sessions_step_rows(list) {
            return rows;
        }
        let Some(all) = &list.all else {
            return Vec::new();
        };
        let current = self.session.as_deref();
        let ms = list.matches();
        let sessions: Vec<Session<'_>> = ms
            .iter()
            .map(|x| kit_session(x, current, list.folders))
            .collect();
        let named = list.detail.as_ref().map_or(now, |(_, at)| *at);
        let age = now.saturating_duration_since(named).as_millis() / u128::from(TICK_MS);
        // 筛得一个不剩时，Puffin 写搜索词的那一句也写 ⌃f 的状态（sessions_steps.rs）；有行时词只用来标出命中的字母。
        let query = if sessions.is_empty() {
            list.said_query()
        } else {
            list.query.clone()
        };
        let kit = resume_list::List {
            earlier: all.iter().any(|x| current != Some(x.id.as_str())),
            matches: &sessions,
            choice: list.choice,
            query: &query,
            named_age: i64::try_from(age).unwrap_or(i64::MAX),
        };
        // 列所有文件夹时每组上面是文件夹的组名（`@` 面板的组名），第一行是各状态的数量（看板，sessions_status.rs）；只列
        // 这个文件夹时同 Puffin。
        if !list.folders {
            return resume_list::rows(&self.ink.theme, &kit, width, now_ms as f64, &*self.zone);
        }
        let mut rows: Vec<Element> = kit
            .earlier
            .then(|| list.counts_row(&self.ink.theme))
            .flatten()
            .into_iter()
            .collect();
        rows.extend(resume_list::grouped_rows(
            &self.ink.theme,
            &kit,
            width,
            now_ms as f64,
            &*self.zone,
        ));
        rows
    }
}

impl App {
    /// 恢复的会话到了（runner 的 `on_switched`）：transcript 的顶上是它的标题与开始的时刻；从列表打开的还在输入框上方说它多久
    /// 以前用过（Puffin `resumed_into`）。缺陷修复（孪生 welcome-resumed 第 28 步）：以前 `Opening …` 一直留着，没有这句。
    pub fn resumed_into(&mut self, created_ms: u64, utc_offset: i32, now_ms: u64) {
        self.resumed = Some(super::app::Resumed {
            created_ms,
            utc_offset,
        });
        self.status_motion.label_fresh = true;
        if let Some(updated) = self.opening.take() {
            let ago = rust_cli_tui_kit::app::widgets::when::last_active(
                now_ms as f64,
                updated as f64,
                &*self.zone,
            );
            self.said.quiet = Some(format!("Last active {ago}."));
        }
    }
}

#[cfg(test)]
#[path = "sessions_list_tests.rs"]
mod tests;
