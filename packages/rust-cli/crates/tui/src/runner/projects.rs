//! `/resume` 的会话列表在 runner 里的部分（sessions_list.rs、sessions_steps.rs；tui-kit.md §4.2「`/resume`」）：
//! 问内核取会话、搜索、改名、删除、读一个会话；切到别的项目或在别的文件夹新建会话（⌃b）之前经入口注入的 `WorkspaceTrust`
//! 做信任检查。现在的会话的状态变了时按同一个查询重读 `/resume` ⇥ 的看板（tui-panels.md §2.1）。回执按请求序号丢掉过时的
//! （`FuturesUnordered` 里回执的先后不定），不用超时。
use futures_util::FutureExt;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::panels::ProjectSessions;
use rust_cli_kernel_api::session_api::{KernelError, SessionView, StartSource};
use rust_cli_kernel_api::store::{SessionOrder, SessionQuery};

use super::Loop;
use super::session::Done;
use crate::trust::TrustPrompt;
use crate::ui::i18n;
use crate::ui::i18n_projects as words;
use crate::ui::projects::{LIMIT, Request};

/// 两页的查询状态：取会话的最新请求序号，以及切换之后要换到的目录（“Use current directory”）。
#[derive(Default)]
pub(super) struct Projects {
    load_seq: u64,
    pub(super) cd_after_switch: Option<String>,
}

pub(super) enum ProjectsDone {
    /// 会话列表的一个词的回答（按词丢掉过时的，`sessions_found`）。
    Found {
        query: String,
        result: Result<Vec<crate::ui::sessions_list::Found>, KernelError>,
    },
    Loaded {
        seq: u64,
        /// 看板的重读（现在的会话的状态变了）：选中的留着（`sessions_refreshed`）。
        refresh: bool,
        notes: std::collections::HashMap<String, crate::services::CockpitNote>,
        result: Result<ProjectSessions, KernelError>,
    },
    /// 驾驶舱的窗格（序号丢掉过时的）。
    Peek {
        seq: u64,
        result: Option<crate::ui::cockpit::Peek>,
    },
    Replies(Vec<(String, String)>),
    /// 槽的 ← 提示的数（tui-cockpit.md §3.4）：别的会话里不是 Rest 的组；读失败时 None。
    News(Option<Vec<(String, crate::ui::cockpit::Kind)>>),
    /// 驾驶舱里的动作失败了：输入框上方说原因。
    CockpitFailed(String),
    Snapshot {
        id: String,
        result: Box<Result<SessionView, KernelError>>,
    },
    Renamed(Result<(), KernelError>),
    Deleted(Result<(), KernelError>),
    /// 信任检查的结果：None 是已信任。
    Checked {
        root: String,
        then: Request,
        prompt: Option<TrustPrompt>,
    },
    /// 信任记录写完（失败只记日志，同启动时，workspace-trust.md §2.3）。
    Accepted,
}

impl Loop<'_> {
    fn workspace_at(&self, root: &str) -> WorkspaceRef {
        WorkspaceRef {
            environment: self.app.view.workspace().environment.clone(),
            root: EnvPath::new(root),
        }
    }

    /// `/resume`（`startup` 为真时是 `--resume` 不带参数）：打开列表并取当前目录的会话。
    pub(super) fn open_resume(&mut self, startup: bool) {
        let action = self.app.open_sessions(startup);
        if let Err(error) = self.act(action) {
            self.app.error(error.to_string());
        }
    }

    pub(super) fn projects(&mut self, request: Request) {
        match request {
            Request::LoadResume { root, order } => self.load_sessions(root, order, false),
            Request::Search { query, root, order } => self.search_sessions(query, root, order),
            Request::Open { .. } | Request::New { .. } => self.check_trust(request),
            Request::Trusted { root, then } => {
                let trust = self.options.services.trust.clone();
                let workspace = self.workspace_at(&root);
                self.pending.push(
                    async move {
                        trust.accept(workspace).await;
                        Done::Projects(ProjectsDone::Accepted)
                    }
                    .boxed(),
                );
                self.proceed(*then);
            }
            Request::Rename { id, title } => {
                let kernel = self.kernel.clone();
                self.pending.push(
                    async move {
                        let result = kernel.rename_session(&SessionId::new(id), title).await;
                        Done::Projects(ProjectsDone::Renamed(result))
                    }
                    .boxed(),
                );
            }
            Request::Delete(id) => {
                let kernel = self.kernel.clone();
                let notes = self.options.services.cockpit.clone();
                self.pending.push(
                    async move {
                        let session = SessionId::new(id);
                        let result = kernel.delete_session(&session).await;
                        // 删除会话时两个笔记一起删（tui-cockpit.md §1.2）。
                        if let (Ok(()), Some(notes)) = (&result, notes) {
                            notes.forget(session).await;
                        }
                        Done::Projects(ProjectsDone::Deleted(result))
                    }
                    .boxed(),
                );
            }
            Request::Transcript(id) => {
                // ⌃t 与“在这里读”：读了算看过（tui-cockpit.md §1.2）。
                self.mark_seen(id.clone());
                self.snapshot(id);
            }
            Request::Stop { address } => self.cockpit_stop(address),
            Request::GoTo(place) => self.cockpit_go_to(place),
            Request::Replies(ids) => self.cockpit_load_replies(ids),
            Request::Seen(id) => self.mark_seen(id),
            Request::RememberGrouping(by_folder) => self.cockpit_remember(by_folder),
        }
    }

    fn load_sessions(&mut self, root: Option<String>, order: SessionOrder, refresh: bool) {
        let kernel = self.kernel.clone();
        let query = SessionQuery {
            environment: self.app.view.workspace().environment.clone(),
            root: root.map(EnvPath::new),
            order,
            limit: LIMIT,
        };
        self.projects.load_seq += 1;
        let seq = self.projects.load_seq;
        // 驾驶舱开着时笔记一起读：行与笔记是同一次的（tui-cockpit.md §1）。
        let port = self
            .app
            .cockpit_open()
            .then(|| self.options.services.cockpit.clone())
            .flatten();
        self.pending.push(
            async move {
                let result = kernel.project_sessions(query).await;
                let notes = match (&port, &result) {
                    (Some(port), Ok(found)) => {
                        let ids = found.sessions.iter().map(|s| s.record.id.clone()).collect();
                        port.notes(ids).await
                    }
                    _ => Default::default(),
                };
                Done::Projects(ProjectsDone::Loaded {
                    seq,
                    refresh,
                    notes: notes
                        .into_iter()
                        .map(|(id, note)| (id.as_str().to_owned(), note))
                        .collect(),
                    result,
                })
            }
            .boxed(),
        );
    }

    /// 现在的会话的状态变了（runner 的事件，`sessions_status::touches_activity`）：看板开着时按同一个查询重读，序号同打开时，
    /// 所以过时的回执照样丢掉。不另起计时器（tui-panels.md §2.1、D4）。
    pub(super) fn refresh_sessions(&mut self) {
        if let Some(order) = self.app.sessions_refreshing() {
            self.load_sessions(None, order, true);
        }
    }

    /// 会话列表的词：这个目录的会话按 Puffin host 的规则筛（`sessions_list::found_of`；搜索词小写、去掉首尾空白）。
    fn search_sessions(&mut self, query: String, root: Option<String>, order: SessionOrder) {
        let kernel = self.kernel.clone();
        let sessions = SessionQuery {
            environment: self.app.view.workspace().environment.clone(),
            root: root.map(EnvPath::new),
            order,
            limit: LIMIT,
        };
        self.pending.push(
            async move {
                let search = query.trim().to_lowercase();
                let result = kernel.project_sessions(sessions).await.map(|found| {
                    found
                        .sessions
                        .iter()
                        .filter_map(|s| crate::ui::sessions_list::found_of(s, &search))
                        .collect()
                });
                Done::Projects(ProjectsDone::Found { query, result })
            }
            .boxed(),
        );
    }

    fn snapshot(&mut self, id: String) {
        let kernel = self.kernel.clone();
        self.pending.push(
            async move {
                let result = Box::new(kernel.session_snapshot(&SessionId::new(id.as_str())).await);
                Done::Projects(ProjectsDone::Snapshot { id, result })
            }
            .boxed(),
        );
    }

    /// 切到别的目录之前的信任检查（workspace-trust.md §2.8）：目录就是当前工作区、换到当前目录（`cd`）或没有注入
    /// 服务时不查。
    fn check_trust(&mut self, request: Request) {
        let (root, skip) = match &request {
            Request::Open { root, cd, .. } => (root.clone(), cd.is_some()),
            Request::New { root } => (root.clone(), false),
            _ => return,
        };
        let here = root == self.app.view.workspace().root.as_str();
        if here || skip {
            return self.proceed(request);
        }
        let trust = self.options.services.trust.clone();
        let workspace = self.workspace_at(&root);
        self.pending.push(
            async move {
                let prompt = trust.check(workspace).await;
                Done::Projects(ProjectsDone::Checked {
                    root,
                    then: request,
                    prompt,
                })
            }
            .boxed(),
        );
    }

    /// 检查通过之后：打开会话（会话自己的工作目录；`cd` 时打开后换到它），或在这个目录新建会话（⌃b，tui-panels.md §2.4）。
    fn proceed(&mut self, request: Request) {
        match request {
            Request::Open {
                id, cd, startup, ..
            } => {
                // 打开一个会话算看过（tui-cockpit.md §1.2）。
                self.mark_seen(id.clone());
                self.app.overlay = None;
                self.projects.cd_after_switch = cd;
                self.switch(SessionId::new(id), StartSource::Resume, None, !startup);
            }
            Request::New { root } => {
                self.app.overlay = None;
                let label = crate::ui::app::label(&root, self.app.home.as_deref());
                let workspace = self.workspace_at(&root);
                self.clear_into(Some((workspace, label)));
            }
            _ => {}
        }
    }

    /// 切换成功之后（`on_switched`）：“Use current directory” 经 `/cd` 的路径换目录（tui-manage.md §10.5）。
    pub(super) fn after_switch(&mut self, switched: bool) {
        let Some(cd) = self.projects.cd_after_switch.take() else {
            return;
        };
        if switched && let Err(error) = self.manage(crate::ui::manage::Manage::ChangeDirectory(cd))
        {
            self.app.error(error.to_string());
        }
    }

    pub(super) fn on_projects(&mut self, done: ProjectsDone) {
        let lang = self.app.lang;
        match done {
            ProjectsDone::Found { query, result } => match result {
                Ok(found) => self.app.sessions_found(query, found),
                Err(error) => self.app.error(i18n::rejected(lang, &error.to_string())),
            },
            ProjectsDone::Peek { seq, result } => {
                if seq == self.cockpit.peek_seq
                    && let Some(peek) = result
                {
                    self.app.cockpit_peeked(peek);
                }
            }
            ProjectsDone::Replies(replies) => self.app.cockpit_replies(replies),
            ProjectsDone::News(kinds) => self.cockpit_news_done(kinds),
            ProjectsDone::CockpitFailed(error) => self.app.error(error),
            ProjectsDone::Loaded {
                seq,
                refresh,
                notes,
                result,
            } => {
                if seq != self.projects.load_seq {
                    return;
                }
                self.app.cockpit_notes(notes);
                match result {
                    Ok(sessions) if refresh => self.app.sessions_refreshed(&sessions),
                    Ok(sessions) => self.app.sessions_loaded(&sessions),
                    Err(error) => {
                        self.app.overlay = None;
                        self.app.error(i18n::rejected(lang, &error.to_string()));
                    }
                }
            }
            ProjectsDone::Snapshot { id, result } => {
                let now = self.now_ms();
                let view = (*result).map_err(|error| error.to_string());
                self.app.sessions_read(&id, view, now);
            }
            // 改名、删除之后重读开着的列表；失败时输入框上方说原因（Puffin 的 ✗ 一行）。
            ProjectsDone::Renamed(Ok(())) | ProjectsDone::Deleted(Ok(())) => self.reload_page(),
            ProjectsDone::Renamed(Err(error)) => {
                self.app
                    .error(words::rename_failed(lang, &error.to_string()));
            }
            ProjectsDone::Deleted(Err(error)) => self
                .app
                .error(words::delete_failed(lang, &error.to_string())),
            ProjectsDone::Checked { root, then, prompt } => match prompt {
                None => self.proceed(then),
                Some(prompt) => self.app.ask_trust(prompt, root, then),
            },
            ProjectsDone::Accepted => {}
        }
    }

    /// 改名、删除之后重取开着的列表。
    fn reload_page(&mut self) {
        if let Some(request) = self.app.sessions_reload() {
            self.projects(request);
        }
    }
}
