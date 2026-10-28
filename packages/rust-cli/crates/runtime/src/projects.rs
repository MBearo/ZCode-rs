//! ← 项目页与 `/resume` 页的内核部分（修复线 F29，docs/specs/rust-cli/tui-panels.md §2、tui-manage.md §10）：存储的
//! 查询合并本进程里打开着的会话的状态与每个目录的分支；改名、删除与只读快照按“打开着的会话由 actor 管”分派。

use std::collections::HashMap;

use rust_cli_kernel_api::env::{EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_files::{FileKind, StatFile};
use rust_cli_kernel_api::env_git::{gitdir_pointer, marks_git_root};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::machine::Files;
use rust_cli_kernel_api::panels::{ProjectSession, ProjectSessions};
use rust_cli_kernel_api::session_api::{Command, CommandEnvelope, SessionView};
use rust_cli_kernel_api::store::{SessionQuery, SessionRecord};

use super::workspace_sessions::{activity, last_reply};
use super::*;

/// 打开着的会话拒绝删除时的文字（tui-manage.md §10.6）。
const OPEN_SESSION: &str = "The session is open in this window.";

fn failed(error: StoreError) -> KernelError {
    match error {
        StoreError::NotFound { session } => KernelError::NotFound { session },
        other => KernelError::Failed {
            message: other.to_string(),
        },
    }
}

/// 路径的上一层（按字符串，两种分隔符都认）；已经是根时为 None。
fn parent(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let index = trimmed.rfind(['/', '\\'])?;
    Some(if index == 0 {
        &trimmed[..1]
    } else {
        &trimmed[..index]
    })
}

fn join(dir: &str, name: &str) -> String {
    let separator = if dir.contains('\\') && !dir.contains('/') {
        '\\'
    } else {
        '/'
    };
    format!("{}{separator}{name}", dir.trim_end_matches(['/', '\\']))
}

/// 按段去掉 `.`、`..` 回退一段（不碰文件系统；`gitdir:` 给的相对路径相对 `.git` 所在的目录）。
fn normalize(path: &str) -> String {
    let separator = if path.contains('\\') && !path.contains('/') {
        '\\'
    } else {
        '/'
    };
    let leading = path.starts_with(['/', '\\']);
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join(&separator.to_string());
    if leading {
        format!("{separator}{joined}")
    } else {
        joined
    }
}

/// 绝对路径（`/…`、`\…`、`C:…`）。
fn absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with(['/', '\\'])
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// 目录现在的 git 分支（tui-panels.md §2.1、D11）：从目录向上找 `.git`；是文件时按 `gitdir:` 找；读 `HEAD` 的
/// `ref: refs/heads/<名字>`。只经执行环境的 `stat` / `read_file`，不跑 git。UNC 路径不读（同 workspace-trust.md §2.1）。
async fn branch(files: &dyn Files, environment: &EnvironmentId, root: &EnvPath) -> Option<String> {
    let workspace = WorkspaceRef {
        environment: environment.clone(),
        root: root.clone(),
    };
    let read = |path: String| {
        let workspace = workspace.clone();
        async move {
            let content = files
                .read_file(
                    &workspace,
                    ReadFile {
                        path: EnvPath::new(path),
                    },
                )
                .await
                .ok()?;
            Some(String::from_utf8_lossy(&content.bytes).trim().to_owned())
        }
    };
    let mut dir = Some(root.as_str());
    while let Some(current) = dir {
        let dot_git = join(current, ".git");
        let stat = files
            .stat(
                &workspace,
                StatFile {
                    path: EnvPath::new(dot_git.as_str()),
                },
            )
            .await
            .ok()
            .flatten();
        // 缺陷修复（F58）：以前 `.git` 是 FIFO、socket、设备时也当文件读，读不到就没有分支；只认
        // 目录与文件，别的种类继续往上。判定与 `gitdir:` 的解析（含 UNC 的拒绝）改用 kernel-api（唯一定义）。
        let head = match stat
            .map(|stat| stat.kind)
            .filter(|kind| marks_git_root(*kind))
        {
            Some(FileKind::Directory) => join(&dot_git, "HEAD"),
            Some(_) => {
                let pointer = read(dot_git).await?;
                let gitdir = gitdir_pointer(&pointer)?;
                let gitdir = if absolute(gitdir) {
                    gitdir.to_owned()
                } else {
                    normalize(&join(current, gitdir))
                };
                join(&gitdir, "HEAD")
            }
            None => {
                dir = parent(current);
                continue;
            }
        };
        let head = read(head).await?;
        return head
            .strip_prefix("ref:")
            .map(str::trim)
            .and_then(|reference| reference.strip_prefix("refs/heads/"))
            .map(str::to_owned);
    }
    None
}

impl LocalKernel {
    /// 打开着的会话（不在会话表里时为 None）。加载中、关闭中的先等它稳定（session-lifecycle.md §3.3.1，F33）：
    /// 关闭中的 actor 不再接命令，关完之后才能直接读写存储。
    async fn open(&self, session: &SessionId) -> Option<Arc<Session>> {
        self.settled(session).await
    }

    pub(crate) async fn projects(
        &self,
        query: SessionQuery,
    ) -> Result<ProjectSessions, KernelError> {
        let environment = query.environment.clone();
        let root = query.root.clone();
        let page = self
            .ports
            .store
            .query_sessions(query)
            .await
            .map_err(failed)?;
        // 先在锁里取出句柄，放锁后再逐个问（view 要等 actor 回复）。只合并打开着的：加载中、关闭中的按库里的行。
        let open: Vec<Arc<Session>> = self
            .sessions()
            .values()
            .filter_map(crate::table::Slot::open)
            .cloned()
            .collect();
        let mut live = HashMap::new();
        for session in open {
            let Ok(view) = session.view().await else {
                continue;
            };
            if view.meta.kind == SessionKind::Subagent
                || view.meta.workspace.environment != environment
                || root
                    .as_ref()
                    .is_some_and(|root| &view.meta.workspace.root != root)
            {
                continue;
            }
            live.insert(view.meta.id.clone(), view);
        }
        let mut sessions: Vec<ProjectSession> = page
            .sessions
            .into_iter()
            .map(|record| {
                let view = live.remove(&record.id);
                ProjectSession {
                    activity: view.as_ref().map(activity),
                    last_reply: view.as_ref().and_then(last_reply),
                    branch: None,
                    record,
                }
            })
            .collect();
        // 打开着、但不在这一页里的（超出上限）也列出来。
        for (id, view) in live {
            sessions.push(ProjectSession {
                activity: Some(activity(&view)),
                last_reply: last_reply(&view),
                branch: None,
                record: SessionRecord {
                    id,
                    title: view.meta.title.clone(),
                    root: view.meta.workspace.root.clone(),
                    created_at_ms: view.meta.created_at_ms,
                    updated_at_ms: view.meta.created_at_ms,
                    first_prompt: None,
                },
            });
        }
        // 每个目录读一次分支。
        let mut branches: HashMap<EnvPath, Option<String>> = HashMap::new();
        for session in &mut sessions {
            let root = session.record.root.clone();
            if !branches.contains_key(&root) {
                let found = branch(self.machine.files().as_ref(), &environment, &root).await;
                branches.insert(root.clone(), found);
            }
            session.branch = branches.get(&root).cloned().flatten();
        }
        Ok(ProjectSessions {
            sessions,
            more: page.more,
        })
    }

    /// 打开着的会话经它的 actor（唯一 owner，`TitleChanged` 更新视图）；否则直接写存储。
    pub(crate) async fn rename(
        &self,
        session: &SessionId,
        title: String,
    ) -> Result<(), KernelError> {
        if let Some(open) = self.open(session).await {
            let envelope = CommandEnvelope {
                id: self.ports.ids.command(),
                command: Command::Rename { title },
            };
            return match open.send(envelope).await {
                rust_cli_kernel_api::session_api::Ack::Rejected { reason } => {
                    Err(KernelError::Failed { message: reason })
                }
                _ => Ok(()),
            };
        }
        self.ports
            .store
            .rename(session, &title)
            .await
            .map_err(failed)
    }

    /// 打开着的会话不能删（它的 actor 还会提交）；否则存储软删除。
    pub(crate) async fn delete(&self, session: &SessionId) -> Result<(), KernelError> {
        if self.open(session).await.is_some() {
            return Err(KernelError::Failed {
                message: OPEN_SESSION.to_owned(),
            });
        }
        self.ports.store.delete(session).await.map_err(failed)
    }

    /// 只读快照：打开着的问 actor，否则冷加载（不启动 actor、不跑 SessionStart）。
    pub(crate) async fn snapshot(&self, session: &SessionId) -> Result<SessionView, KernelError> {
        if let Some(open) = self.open(session).await {
            return open.view().await;
        }
        let loaded = self.ports.store.load(session).await.map_err(failed)?;
        let execution = loaded.mode.map_or_else(
            || self.defaults.mode.into(),
            |mode| rust_cli_kernel_api::permission::ExecutionState::from_parts(mode, loaded.plan),
        );
        Ok(SessionView {
            meta: loaded.meta,
            // 显示用的历史：压缩点之前只供显示的消息接上模型侧的历史（工作线 F102，compact.md §5.1）。
            messages: [loaded.earlier, loaded.messages].concat(),
            selection: loaded
                .model
                .unwrap_or_else(|| self.defaults.selection.clone()),
            mode: execution.mode,
            plan: execution.plan,
            pending: Vec::new(),
            queue: Vec::new(),
            last_seq: 0,
            revision: 0,
            running: false,
            background: Vec::new(),
            peer_held: Vec::new(),
            context_usage: None,
            context_notice: Default::default(),
            output_speed: None,
        })
    }
}

#[cfg(test)]
#[path = "projects_tests.rs"]
mod tests;
