//! `@` 面板在 runner 里的部分（docs/specs/rust-cli/tui-input.md §1.2；照 Puffin 的 host 的 `mentions.rs`）：
//! 输入框记下的搜索、预览与 git 的改动在这里发出，回答到达时交回输入框。
//!
//! - 搜索：缓存的清单（没有时 `Kernel::workspace_files`）上 `at_search::search`；非空的词一个都没找到时绕过缓存重取一次。
//!   取的期间工作区换了：不缓存旧工作区的清单。
//! - 预览：文件夹从清单数；文件经入口的 `peek_file`（只在本机的工作区里读，远端回答 `Missing`，同 Puffin 的 host）；入口没有
//!   给它时文件不读。
//! - 改动：`Kernel::uncommitted_diff` 的 numstat 与未跟踪的文件（面板每次打开重读）。
//! - 会话：`Kernel::project_sessions`，这个工作区按更新时间最多 [`SESSION_LIMIT`] 个，标题空时用第一个请求（面板每次打开重读，
//!   同 Puffin host 的 `conversations`）。
use std::sync::Arc;

use futures_util::FutureExt;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env_git::GitDiff;
use rust_cli_kernel_api::session_api::Kernel;
use rust_cli_kernel_api::store::{SessionOrder, SessionQuery};
use rust_cli_tui_kit::app::widgets::at_panel::data::{ConversationRef, PathPeek};

use super::Loop;
use super::session::Done;
use crate::ui::at_panel::AtFiles;
use crate::ui::at_search::{self, FileHit, PEEK_NAMES};
use crate::ui::composer::Popup;

type Entries = Arc<Vec<FileHit>>;

/// `@` 面板最多列这么多个会话（同 Puffin host 的 `lists::SESSION_LIMIT`）。
const SESSION_LIMIT: usize = 200;

/// `@` 面板的回答。`entries` 是这次取来的清单（`workspace` 的），收下时缓存。
pub(super) enum AtDone {
    Found {
        workspace: WorkspaceRef,
        entries: Option<Entries>,
        answer: AtFiles,
    },
    Peeked {
        path: String,
        peek: PathPeek,
    },
    Changed(Vec<String>),
    /// 这个工作区最近的会话。
    Conversations(Vec<ConversationRef>),
}

/// 执行环境的清单（相对工作区根，分隔符随执行环境）→ 文件与文件夹；取失败时是错误的说法。
async fn listed(kernel: &Arc<dyn Kernel>, workspace: &WorkspaceRef) -> Result<Entries, String> {
    let files = kernel
        .workspace_files(workspace)
        .await
        .map_err(|error| error.to_string())?;
    let separator = super::session::path_separator(workspace.root.as_str());
    Ok(Arc::new(at_search::entries(&files, separator)))
}

impl Loop<'_> {
    /// 输入框记下的搜索、预览与改动：发出去（按键、回答到达与每一帧之后）。
    pub(super) fn fetch_at(&mut self) {
        let workspace = self.app.view.workspace().clone();
        let files = &mut self.app.composer.files;
        let cached = files.entries.clone();
        if let Some((query, under)) = files.search.take() {
            let kernel = self.kernel.clone();
            let workspace = workspace.clone();
            let cached = cached.clone();
            self.pending.push(
                async move {
                    let (mut entries, mut fresh) = match cached {
                        Some(entries) => (Ok(entries), false),
                        None => (listed(&kernel, &workspace).await, true),
                    };
                    let mut found = match &entries {
                        Ok(list) => at_search::search(list, &query, &under),
                        Err(_) => Vec::new(),
                    };
                    if found.is_empty() && !query.is_empty() && !fresh {
                        entries = listed(&kernel, &workspace).await;
                        fresh = true;
                        if let Ok(list) = &entries {
                            found = at_search::search(list, &query, &under);
                        }
                    }
                    if let Err(error) = &entries {
                        tracing::warn!("@ file search failed: {error}");
                    }
                    Done::At(AtDone::Found {
                        workspace,
                        entries: entries.ok().filter(|_| fresh),
                        answer: AtFiles {
                            query,
                            under,
                            found: at_search::limited(found),
                        },
                    })
                }
                .boxed(),
            );
        }
        // 入口没有给读文件的端口时文件不读：预览只有名字与路径（它一直「在读」，不再问）。
        let peek_file = self.options.services.peek_file.clone();
        if let Some(path) = files
            .peek
            .take()
            .filter(|path| path.ends_with('/') || peek_file.is_some())
        {
            let kernel = self.kernel.clone();
            let workspace = workspace.clone();
            self.pending.push(
                async move {
                    let peek = if path.ends_with('/') {
                        let list = match cached {
                            Some(list) => Ok(list),
                            None => listed(&kernel, &workspace).await,
                        };
                        list.map_or(PathPeek::Missing, |list| {
                            let found = at_search::search(&list, "", &path);
                            PathPeek::Folder {
                                entries: found.len() as u64,
                                names: found
                                    .iter()
                                    .take(PEEK_NAMES)
                                    .map(|hit| hit.path[path.len()..].to_owned())
                                    .collect(),
                            }
                        })
                    } else if let Some(peek_file) = peek_file {
                        peek_file(workspace, path.clone()).await
                    } else {
                        PathPeek::Missing
                    };
                    Done::At(AtDone::Peeked { path, peek })
                }
                .boxed(),
            );
        }
        if std::mem::take(&mut files.reopened) {
            let query = SessionQuery {
                environment: workspace.environment.clone(),
                root: Some(workspace.root.clone()),
                order: SessionOrder::Updated,
                limit: SESSION_LIMIT,
            };
            let kernel = self.kernel.clone();
            self.pending.push(
                async move {
                    // 不在 git 里或内核不支持：没有改动可列。
                    let changed = match kernel.uncommitted_diff(&workspace).await {
                        Ok(GitDiff::Repository {
                            numstat, untracked, ..
                        }) => at_search::changed_paths(&numstat, &untracked),
                        Ok(GitDiff::NotRepository) | Err(_) => Vec::new(),
                    };
                    Done::At(AtDone::Changed(changed))
                }
                .boxed(),
            );
            let kernel = self.kernel.clone();
            self.pending.push(
                async move {
                    let found = match kernel.project_sessions(query).await {
                        Ok(found) => found.sessions.into_iter().map(conversation).collect(),
                        Err(error) => {
                            tracing::warn!("@ conversations: {error}");
                            Vec::new()
                        }
                    };
                    Done::At(AtDone::Conversations(found))
                }
                .boxed(),
            );
        }
    }

    pub(super) fn on_at(&mut self, done: AtDone) {
        let open = matches!(self.app.composer.popup, Some(Popup::Files { .. }));
        let current = self.app.view.workspace().clone();
        let files = &mut self.app.composer.files;
        match done {
            AtDone::Found {
                workspace,
                entries,
                answer,
            } => {
                // 取的期间工作区换了：旧工作区的清单与回答都不要。
                if workspace != current {
                    return;
                }
                if entries.is_some() {
                    files.entries = entries;
                }
                files.found(answer, open);
            }
            AtDone::Peeked { path, peek } => files.peeked(path, peek),
            AtDone::Changed(paths) => files.changed = paths,
            AtDone::Conversations(found) => files.conversations = Some(found),
        }
        self.fetch_at();
    }
}

/// 一个会话 → `@` 面板的引用：标题空时用第一个请求（Puffin host 的 `conversations`）。
fn conversation(session: rust_cli_kernel_api::panels::ProjectSession) -> ConversationRef {
    ConversationRef {
        session_id: session.record.id.to_string(),
        title: if session.record.title.is_empty() {
            session.record.first_prompt.unwrap_or_default()
        } else {
            session.record.title
        },
        updated_at: session.record.updated_at_ms,
    }
}
