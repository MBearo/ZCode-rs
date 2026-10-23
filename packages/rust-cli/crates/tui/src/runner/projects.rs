//! ← 项目页与 `/resume` 页在 runner 里的部分（修复线 F29，docs/specs/rust-cli/tui-panels.md §2、tui-manage.md §10）：
//! 问内核取会话、改名、删除、快照；切到别的项目之前经入口注入的 `WorkspaceTrust` 做信任检查。回执按请求序号丢掉
//! 过时的（`FuturesUnordered` 里回执的先后不定），不用超时。
use futures_util::FutureExt;
use ratatui::backend::Backend;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::panels::ProjectSessions;
use rust_cli_kernel_api::session_api::{KernelError, SessionView, StartSource};
use rust_cli_kernel_api::store::{SessionOrder, SessionQuery};

use super::Loop;
use super::session::Done;
use crate::trust::TrustPrompt;
use crate::ui::app::Overlay;
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
    Loaded {
        seq: u64,
        resume: bool,
        result: Result<ProjectSessions, KernelError>,
    },
    Snapshot {
        id: String,
        transcript: bool,
        result: Box<Result<SessionView, KernelError>>,
    },
    Renamed(Result<(), KernelError>),
    Deleted {
        id: String,
        result: Result<(), KernelError>,
    },
    /// 信任检查的结果：None 是已信任。
    Checked {
        root: String,
        then: Request,
        prompt: Option<TrustPrompt>,
    },
    /// 信任记录写完（失败只记日志，同启动时，workspace-trust.md §2.3）。
    Accepted,
}

impl<B: Backend> Loop<'_, B> {
    fn workspace_at(&self, root: &str) -> WorkspaceRef {
        WorkspaceRef {
            environment: self.app.view.workspace().environment.clone(),
            root: EnvPath::new(root),
        }
    }

    /// `/resume`（`startup` 为真时是 `--resume` 不带参数）：打开页面并取当前目录的会话。
    pub(super) fn open_resume(&mut self, startup: bool) {
        let offset = self.options.clock.utc_offset_minutes(self.now_ms());
        let action = self.app.open_resume_page(startup, offset);
        if let Err(error) = self.act(action) {
            self.app.error(error.to_string());
        }
    }

    pub(super) fn projects(&mut self, request: Request) {
        match request {
            Request::LoadProjects => self.load_sessions(None, SessionOrder::Updated, false),
            Request::LoadResume { root, order } => self.load_sessions(root, order, true),
            Request::Open { .. } | Request::New { .. } => self.check_trust(request),
            Request::Trusted { root, then } => {
                if let Some(trust) = self.options.services.trust.clone() {
                    let workspace = self.workspace_at(&root);
                    self.pending.push(
                        async move {
                            trust.accept(workspace).await;
                            Done::Projects(ProjectsDone::Accepted)
                        }
                        .boxed(),
                    );
                }
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
                self.pending.push(
                    async move {
                        let result = kernel.delete_session(&SessionId::new(id.as_str())).await;
                        Done::Projects(ProjectsDone::Deleted { id, result })
                    }
                    .boxed(),
                );
            }
            Request::Preview(id) => self.snapshot(id, false),
            Request::Transcript(id) => self.snapshot(id, true),
        }
    }

    fn load_sessions(&mut self, root: Option<String>, order: SessionOrder, resume: bool) {
        let kernel = self.kernel.clone();
        let query = SessionQuery {
            environment: self.app.view.workspace().environment.clone(),
            root: root.map(EnvPath::new),
            order,
            limit: LIMIT,
        };
        self.projects.load_seq += 1;
        let seq = self.projects.load_seq;
        self.pending.push(
            async move {
                let result = kernel.project_sessions(query).await;
                Done::Projects(ProjectsDone::Loaded {
                    seq,
                    resume,
                    result,
                })
            }
            .boxed(),
        );
    }

    fn snapshot(&mut self, id: String, transcript: bool) {
        let kernel = self.kernel.clone();
        self.pending.push(
            async move {
                let result = Box::new(kernel.session_snapshot(&SessionId::new(id.as_str())).await);
                Done::Projects(ProjectsDone::Snapshot {
                    id,
                    transcript,
                    result,
                })
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
        let trust = self.options.services.trust.clone();
        match trust {
            Some(trust) if !here && !skip => {
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
            _ => self.proceed(request),
        }
    }

    /// 检查通过之后：打开会话（会话自己的工作目录；`cd` 时打开后换到它）或在这个目录新建会话。
    fn proceed(&mut self, request: Request) {
        match request {
            Request::Open { id, cd, .. } => {
                self.app.overlay = None;
                self.projects.cd_after_switch = cd;
                self.switch(SessionId::new(id), StartSource::Resume, None);
            }
            Request::New { root } => {
                self.app.overlay = None;
                let label = crate::ui::app::label(&root, self.app.home.as_deref());
                let workspace = self.workspace_at(&root);
                if let Err(error) = self.clear_into(Some((workspace, label))) {
                    self.app.error(error.to_string());
                }
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
            ProjectsDone::Loaded {
                seq,
                resume,
                result,
            } => {
                if seq != self.projects.load_seq {
                    return;
                }
                match (result, resume) {
                    (Ok(sessions), true) => self.app.resume_loaded(sessions),
                    (Ok(sessions), false) => self.app.projects_loaded(sessions),
                    (Err(error), _) => {
                        self.app.overlay = None;
                        self.app.error(i18n::rejected(lang, &error.to_string()));
                    }
                }
            }
            ProjectsDone::Snapshot {
                id,
                transcript,
                result,
            } => {
                let now = self.now_ms();
                let view = (*result).map_err(|error| error.to_string());
                self.app.resume_snapshot(&id, transcript, view, now);
            }
            ProjectsDone::Renamed(Ok(())) => self.reload_page(),
            ProjectsDone::Renamed(Err(error)) => {
                let text = words::rename_failed(lang, &error.to_string());
                self.app.projects_error(text.clone());
                self.app.resume_error(text);
            }
            ProjectsDone::Deleted { id, result } => match result {
                Ok(()) => self.app.resume_deleted(&id),
                Err(error) => self
                    .app
                    .resume_error(words::delete_failed(lang, &error.to_string())),
            },
            ProjectsDone::Checked { root, then, prompt } => match prompt {
                None => self.proceed(then),
                Some(prompt) => self.app.ask_trust(prompt, root, then),
            },
            ProjectsDone::Accepted => {}
        }
    }

    /// 改名之后重取开着的那一页。
    fn reload_page(&mut self) {
        let request = match &self.app.overlay {
            Some(Overlay::Projects(_)) => Request::LoadProjects,
            Some(Overlay::Resume(_)) => match self.app.resume_reload() {
                Some(request) => request,
                None => return,
            },
            _ => return,
        };
        self.projects(request);
    }
}
