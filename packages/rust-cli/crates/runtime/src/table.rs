//! 会话表（docs/specs/rust-cli/session-lifecycle.md §3.3.1）：这个进程里每个会话归谁的唯一来源。
//!
//! 每个会话 ID 至多一个条目，条目是一个状态机：（没有）→ Loading → Open → Closing →（没有）。每次转移都在表的锁里
//! 一步完成；Loading / Closing 带一个丢弃即取消的令牌，等它的调用在锁里取令牌、放锁后等它取消，醒来重新看表。
//!
//! 修复（F33）：以前表里只有打开着的句柄，关闭一直到最后一步才移出它。`/clear` 在后台关旧会话、不等，紧接着
//! `/resume` 选回这个会话时，`open_session_as` 从表里拿到正在关闭的旧句柄（actor 已退出），界面写
//! `the session is closed`。现在关闭第一步就把条目标成 Closing，打开同一个会话等它移出再冷加载；并发的打开也不再
//! 各自冷加载（Loading）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use rust_cli_base::TaskScope;
use rust_cli_context::PromptAssembler;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_policy::PolicyGate;
use rust_cli_session::{Opened, Session, SessionDeps, SessionState};
use tokio_util::sync::CancellationToken;

use crate::LocalKernel;

/// 打开着的会话：句柄与它的任务作用域（释放时中止它的 actor 与主循环）。
pub(crate) struct Open {
    pub(crate) session: Arc<Session>,
    _scope: TaskScope,
}

/// 条目换掉时唤醒等它的调用：丢弃即取消。
pub(crate) struct Changing(CancellationToken);

impl Changing {
    fn new() -> Self {
        Self(CancellationToken::new())
    }
}

impl Drop for Changing {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// 会话表的一个条目。
pub(crate) enum Slot {
    /// 一个调用在冷加载（[`Loading`]）；其余的打开等它换掉。
    Loading(Changing),
    Open(Open),
    /// 关闭任务在跑；它的最后一步（[`Closer`] 的 `released`）移出条目。`_open` 留着作用域，移出时才释放。
    Closing {
        _open: Open,
        changing: Changing,
    },
}

impl Slot {
    /// 打开着的句柄；加载中、关闭中的没有。
    pub(crate) fn open(&self) -> Option<&Arc<Session>> {
        match self {
            Self::Open(open) => Some(&open.session),
            Self::Loading(_) | Self::Closing { .. } => None,
        }
    }
}

pub(crate) type OpenSessions = Arc<Mutex<HashMap<SessionId, Slot>>>;

fn lock(sessions: &Mutex<HashMap<SessionId, Slot>>) -> MutexGuard<'_, HashMap<SessionId, Slot>> {
    sessions.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 看表的结果。
enum Seen {
    Open(Arc<Session>),
    Absent,
    /// 加载中或关闭中：等这个令牌取消（条目被换掉或移出）再看。
    Wait(CancellationToken),
}

fn look(sessions: &HashMap<SessionId, Slot>, id: &SessionId) -> Seen {
    match sessions.get(id) {
        None => Seen::Absent,
        Some(Slot::Open(open)) => Seen::Open(open.session.clone()),
        Some(Slot::Loading(changing) | Slot::Closing { changing, .. }) => {
            Seen::Wait(changing.0.clone())
        }
    }
}

/// `open_session_as` 的第一步的结果。
pub(crate) enum Claim<'a> {
    Open(Arc<Session>),
    /// 这个调用登记了 Loading，由它冷加载。
    Load(Loading<'a>),
}

/// 冷加载的占位：加载失败或调用被丢下时移出 Loading，等它的调用醒来自己再试。加载成功时 [`LocalKernel::start`]
/// 已经把它换成 Open，这里不动。
pub(crate) struct Loading<'a> {
    sessions: &'a OpenSessions,
    id: SessionId,
}

impl Drop for Loading<'_> {
    fn drop(&mut self) {
        let mut sessions = lock(self.sessions);
        if matches!(sessions.get(&self.id), Some(Slot::Loading(_))) {
            sessions.remove(&self.id);
        }
    }
}

/// 会话关闭的收尾（session-lifecycle.md §3.3，R1-1）：关闭开始时把条目标成 Closing；关完时清掉各能力按会话保存的表，
/// 最后移出条目（释放它的任务作用域）。所有按会话的表都在这一条关闭路径上释放；执行环境与定时任务由会话自己在调用
/// `released` 之前释放。会话表用弱引用：表里的会话持有这个收尾者，强引用会成环。
pub(crate) struct Closer {
    pub(crate) sessions: Weak<Mutex<HashMap<SessionId, Slot>>>,
    pub(crate) context: Arc<PromptAssembler>,
    pub(crate) read_files: Arc<rust_cli_tools::ReadFileState>,
    pub(crate) worktrees: Arc<rust_cli_tools::WorktreeBoard>,
    pub(crate) gate: Arc<PolicyGate>,
    /// WaitForMcpServers 的 declared 按会话记，会话关闭时清掉（ext.md §3.8、D34）。
    pub(crate) mcp: Arc<rust_cli_ext::McpManager>,
}

impl rust_cli_session::Released for Closer {
    /// Open → Closing（早于 SessionEnd）：从这时起打开同一个会话都等它关完，不会拿到马上要退出的 actor。
    fn closing(&self, session: &SessionId) {
        let Some(sessions) = self.sessions.upgrade() else {
            return;
        };
        let mut sessions = lock(&sessions);
        if let Some(Slot::Open(_)) = sessions.get(session)
            && let Some(Slot::Open(open)) = sessions.remove(session)
        {
            sessions.insert(
                session.clone(),
                Slot::Closing {
                    _open: open,
                    changing: Changing::new(),
                },
            );
        }
    }

    fn released(&self, session: &SessionId) {
        // `/clear` 清 readFileState 与规则文件缓存；resume 从历史重建它们。
        self.context.session_closed(session);
        self.read_files.forget(session);
        self.worktrees.forget(session);
        self.gate.release(session);
        self.mcp.release(session);
        // 最后移出条目：释放作用域会中止这个会话剩下的任务（actor 已经退出，只剩标题等旁路任务）；Closing 的令牌随之
        // 丢弃，等它的打开醒来冷加载，看到的是上面已经清干净的表（先清后移，顺序不能反）。
        let slot = self.sessions.upgrade().and_then(|sessions| {
            let mut sessions = lock(&sessions);
            match sessions.get(session) {
                Some(Slot::Open(_) | Slot::Closing { .. }) => sessions.remove(session),
                Some(Slot::Loading(_)) | None => None,
            }
        });
        drop(slot);
    }
}

impl LocalKernel {
    pub(crate) fn sessions(&self) -> MutexGuard<'_, HashMap<SessionId, Slot>> {
        lock(&self.sessions)
    }

    /// 条目稳定之后的句柄：Open 给出句柄，没有条目为 None；加载中、关闭中的等它换掉再看（改名、删除、只读快照用）。
    pub(crate) async fn settled(&self, id: &SessionId) -> Option<Arc<Session>> {
        loop {
            let wait = match look(&self.sessions(), id) {
                Seen::Open(session) => return Some(session),
                Seen::Absent => return None,
                Seen::Wait(wait) => wait,
            };
            wait.cancelled().await;
        }
    }

    /// 打开的第一步：Open 给出句柄；没有条目时登记 Loading，由调用方冷加载；加载中、关闭中的等它换掉再看。
    pub(crate) async fn claim(&self, id: &SessionId) -> Claim<'_> {
        loop {
            let wait = {
                let mut sessions = self.sessions();
                match look(&sessions, id) {
                    Seen::Open(session) => return Claim::Open(session),
                    Seen::Absent => {
                        sessions.insert(id.clone(), Slot::Loading(Changing::new()));
                        return Claim::Load(Loading {
                            sessions: &self.sessions,
                            id: id.clone(),
                        });
                    }
                    Seen::Wait(wait) => wait,
                }
            };
            wait.cancelled().await;
        }
    }

    /// 启动会话 actor 并登记为 Open（`Session::spawn` 是同步调用，持锁期间不等待）。新建的会话 ID 是新的；冷加载时
    /// 这里换掉的是调用方自己登记的 Loading，它的令牌随之丢弃，等它的打开醒来拿到这个句柄。
    pub(crate) fn start(&self, state: SessionState, opened: Opened) -> Arc<Session> {
        let id = state.meta.id.clone();
        let mut sessions = self.sessions();
        if let Some(Slot::Open(open)) = sessions.get(&id) {
            return open.session.clone();
        }
        let scope = self.scope.spawner().child(format!("session/{id}"));
        let deps = SessionDeps {
            store: self.ports.store.clone(),
            ids: self.ports.ids.clone(),
            runner: self.runner.clone(),
            compactor: self.compactor.clone(),
            machine: self.machine.clone(),
            local: self.local.clone(),
            max_turns: self.defaults.max_turns,
            agents: self.agents.clone(),
            cron: self.cron.clone(),
            hooks: self.hooks.clone(),
            commands: self.ext.user_commands(),
            inputs: self.inputs.clone(),
            artifacts: self.ports.artifacts.clone(),
            clock: self.ports.clock.clone(),
            generation: self.generation.clone(),
            spawner: scope.spawner(),
            closing: self.scope.spawner(),
            titles: self.titles.clone(),
            side_model: self.ports.model.clone(),
            released: self.closer.clone(),
            directory_rules: self.usage.context.clone(),
            peers: self.peers.clone(),
            detached: Some(self.detached.clone()),
        };
        let session = Arc::new(Session::spawn(deps, state, opened));
        tracing::info!(event = "session.open", session = %id, opened = ?opened, "session opened");
        sessions.insert(
            id,
            Slot::Open(Open {
                session: session.clone(),
                _scope: scope,
            }),
        );
        session
    }
}
