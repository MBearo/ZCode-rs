//! 本进程里启动过的子代理（ListAgents、SendMessage、TaskStop 与后台列表都查这里）。
//!
//! 子代理的消息只在子会话里（存储是唯一事实源）；这里只记身份、归属、运行状态与邮箱。发给子代理的
//! 通知也记在这里它自己的条目上（docs/specs/rust-cli/agents.md §5.1），不另起状态源。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::{AgentId, SessionId, ToolUseId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;

use crate::definitions::AgentDefinition;
use crate::doing::Doing;
use crate::report::{Ending, Report};

/// 发给某个子代理的一条通知：它派生的后台子代理结束了。
#[derive(Debug, Clone)]
pub(crate) struct Notice {
    pub(crate) from: AgentId,
    pub(crate) status: &'static str,
    /// `<task-notification>` 信封（不带说明与包装）。
    pub(crate) envelope: String,
}

/// 邮箱里的一项：SendMessage 的消息在工具边界作为插话并入，通知作为 `task-notification` 并入。
#[derive(Debug, Clone)]
pub(crate) enum Mail {
    Message(String),
    Notice(Notice),
}

#[derive(Debug, Clone)]
pub(crate) enum AgentState {
    /// 正在跑；`mailbox` 在下一个工具边界并入。
    Running {
        abort: Abort,
        mailbox: Vec<Mail>,
    },
    /// 跑完了，但还有在跑的后台子代理（keepalive，只在交互模式）：自己的通知推迟到续跑结束后。
    /// 记着最后一次运行的报告（被 TaskStop 停止时的通知用它）。
    Parked(Report),
    Finished(Ending),
}

#[derive(Debug, Clone)]
pub(crate) struct LiveAgent {
    pub(crate) agent: AgentId,
    pub(crate) child: SessionId,
    /// 启动它的会话。
    pub(crate) parent: SessionId,
    /// 顶层会话：任务清单、链路 ID 与“送顶层会话”的通知用它。
    pub(crate) root: SessionId,
    /// 派生它的子代理；主线程派生的没有（通知送 `root`）。
    pub(crate) owner: Option<AgentId>,
    /// 主线程派生的是 1，每往下一层加 1（Agent 工具只给深度小于 3 的子代理）。
    pub(crate) depth: u32,
    /// 启动它的 Agent 调用；SendMessage 留下的消息续跑之后为 None（续跑不属于哪次工具调用，通知里
    /// 没有 `<tool-use-id>`，A8）。
    pub(crate) tool_use: Option<ToolUseId>,
    pub(crate) description: String,
    /// 定义（含 system prompt 正文）共享一份：条目每次被查询、快照都要复制（修复线 F6，审查 R5-11）。
    pub(crate) definition: Arc<AgentDefinition>,
    pub(crate) workspace: WorkspaceRef,
    /// 子代理的工具缺省在哪台远端机器上跑（Agent 的 `machine`，R3，remote-exec.md §20.6）；本机为 None。
    pub(crate) machine: Option<String>,
    pub(crate) selection: ModelSelection,
    pub(crate) mode: PermissionMode,
    pub(crate) background: bool,
    /// 父会话的会话级询问入口（交互界面里才有，agents.md §4.1.1）：后台运行（含续跑、停放后的唤醒）经它询问。
    pub(crate) asks: Option<crate::child::Asks>,
    pub(crate) state: AgentState,
    /// 列表显示用的记录（tui-panels.md §1.1）；由 `live::Live` 在每次改动后统一维护。
    pub(crate) shown: Shown,
    /// fork 子代理继承的前缀（修复线 L-A，agents.md §8.1.3）：不落库，续跑时拼在子会话的冷读取之前；别的子代理为 None。
    pub(crate) fork_prefix: Option<Arc<Vec<rust_cli_kernel_api::message::Message>>>,
}

/// 列表显示用的记录：启动顺序、这次运行的开始与结束时刻、等待中的交互、子会话的提交次数、当前活动。
#[derive(Debug, Clone, Default)]
pub(crate) struct Shown {
    pub(crate) order: u64,
    pub(crate) started_ms: u64,
    pub(crate) ended_ms: Option<u64>,
    pub(crate) waiting: u32,
    pub(crate) version: u64,
    /// 当前活动与记下它的时刻（L35，stall-visibility.md §4.3）；离开运行时清掉。
    pub(crate) doing: Option<(Doing, u64)>,
    /// 最近一个完成的子代理请求的输出速度（修复线 F112，tui-panels.md §1.7：查看它时 footer 的 TPS）：只在内存里，
    /// 与主会话的速度同一个口径（tui-display.md §2.6）。续跑与结束都不清掉。
    pub(crate) speed: Option<rust_cli_kernel_api::metering::OutputSpeed>,
}

impl LiveAgent {
    pub(crate) fn status(&self) -> &'static str {
        match &self.state {
            AgentState::Running { .. } => "running",
            // 停放的子代理状态是 completed（带 keepalive）。
            AgentState::Parked(_) => "completed",
            AgentState::Finished(ending) => ending.status(),
        }
    }

    /// 还在跑或停放。
    pub(crate) fn is_live(&self) -> bool {
        !matches!(self.state, AgentState::Finished(_))
    }
}

pub(crate) type Agents = BTreeMap<AgentId, LiveAgent>;

/// 没有注入时钟时（单测）的时刻：一律 0。
struct ZeroClock;

impl Clock for ZeroClock {
    fn now_ms(&self) -> u64 {
        0
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

pub(crate) struct Live {
    agents: Mutex<Agents>,
    clock: Arc<dyn Clock>,
    /// 启动顺序的计数（列表按它排序）。
    order: AtomicU64,
    /// 每次改动后加一（tokio watch：多次改动合并成一次通知，tui-panels.md §0）。
    changes: tokio::sync::watch::Sender<u64>,
}

impl Default for Live {
    fn default() -> Self {
        Self::new(Arc::new(ZeroClock))
    }
}

impl Live {
    pub(crate) fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            agents: Mutex::default(),
            clock,
            order: AtomicU64::new(0),
            changes: tokio::sync::watch::channel(0).0,
        }
    }

    fn agents(&self) -> MutexGuard<'_, Agents> {
        self.agents.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 改动条目：锁里改完、记好时刻，放锁后再通知（订阅方收到通知后去查，一定看得到这次改动）。
    fn change<R>(&self, work: impl FnOnce(&mut Agents) -> R) -> R {
        let result = {
            let mut agents = self.agents();
            let result = work(&mut agents);
            settle_times(&mut agents, self.clock.now_ms());
            result
        };
        self.changes
            .send_modify(|version| *version = version.wrapping_add(1));
        result
    }

    /// 变化通知的订阅（列表的查询方据此重取）。
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// 顶层会话 `root` 里条目的快照与查询时刻（列表用）。先过滤再复制（修复线 F6，审查 R5-11：以前每次列表查询都复制
    /// 全部会话的全部条目）。
    pub(crate) fn snapshot_in(&self, root: &SessionId) -> (Vec<LiveAgent>, u64) {
        let agents: Vec<LiveAgent> = self
            .agents()
            .values()
            .filter(|agent| &agent.root == root)
            .cloned()
            .collect();
        (agents, self.clock.now_ms())
    }

    /// 在一把锁里读改（通知的路由要一次看清并改动几个条目，agents.md §5.1）。
    pub(crate) fn with<R>(&self, work: impl FnOnce(&mut Agents) -> R) -> R {
        self.change(work)
    }

    pub(crate) fn insert(&self, mut agent: LiveAgent) {
        agent.shown.order = self.order.fetch_add(1, Ordering::Relaxed);
        agent.shown.started_ms = self.clock.now_ms();
        self.change(|agents| agents.insert(agent.agent.clone(), agent));
    }

    /// 子代理经父会话询问时记一次等待（`waiting` 为假时结束一次）。
    pub(crate) fn waiting(&self, agent: &AgentId, waiting: bool) {
        self.change(|agents| {
            if let Some(entry) = agents.get_mut(agent) {
                entry.shown.waiting = if waiting {
                    entry.shown.waiting + 1
                } else {
                    entry.shown.waiting.saturating_sub(1)
                };
            }
        });
    }

    /// 子会话提交了一条消息（只读查看据此重取）；这条消息是一个完成的子代理请求的回复时同时记下它的速度（F112）。
    /// 速度与版本在同一次改动里：看到新版本去取快照时，速度一定已经是这个请求的。
    pub(crate) fn committed(
        &self,
        agent: &AgentId,
        speed: Option<rust_cli_kernel_api::metering::OutputSpeed>,
    ) {
        self.change(|agents| {
            if let Some(entry) = agents.get_mut(agent) {
                entry.shown.version += 1;
                if speed.is_some() {
                    entry.shown.speed = speed;
                }
            }
        });
    }

    /// 子循环的事件改变了当前活动（L35）。活动没变时不通知；变了只发变化通知、不加 `version`
    /// （只读查看不因此重取子会话）。
    pub(crate) fn doing(&self, agent: &AgentId, doing: Option<Doing>) {
        let changed = {
            let mut agents = self.agents();
            let Some(entry) = agents.get_mut(agent) else {
                return;
            };
            if entry.shown.doing.as_ref().map(|(now, _)| now) == doing.as_ref() {
                false
            } else {
                entry.shown.doing = doing.map(|doing| (doing, self.clock.now_ms()));
                true
            }
        };
        if changed {
            self.changes
                .send_modify(|version| *version = version.wrapping_add(1));
        }
    }

    pub(crate) fn get(&self, agent: &AgentId) -> Option<LiveAgent> {
        self.agents().get(agent).cloned()
    }

    /// 顶层会话：`session` 是某个子代理的子会话时取它的顶层会话，否则就是它自己。
    pub(crate) fn root_of(&self, session: &SessionId) -> SessionId {
        self.agents()
            .values()
            .find(|agent| &agent.child == session)
            .map_or_else(|| session.clone(), |agent| agent.root.clone())
    }

    /// 由 `session` 启动的子代理，按 ID 排序。
    pub(crate) fn spawned_by(&self, session: &SessionId) -> Vec<LiveAgent> {
        self.agents()
            .values()
            .filter(|agent| &agent.parent == session)
            .cloned()
            .collect()
    }

    /// 顶层会话 `root` 里还在跑或停放的后台子代理（含子代理里再派生的），按 ID 排序。
    pub(crate) fn background_in(&self, root: &SessionId) -> Vec<LiveAgent> {
        self.agents()
            .values()
            .filter(|agent| &agent.root == root && agent.background && agent.is_live())
            .cloned()
            .collect()
    }

    /// 重新开始运行（SendMessage 续跑已结束或停放的子代理）。已在跑时返回 false。
    pub(crate) fn restart(&self, agent: &AgentId, abort: Abort) -> bool {
        self.change(|agents| {
            let Some(entry) = agents.get_mut(agent) else {
                return false;
            };
            if matches!(entry.state, AgentState::Running { .. }) {
                return false;
            }
            entry.background = true;
            entry.state = AgentState::Running {
                abort,
                mailbox: Vec::new(),
            };
            true
        })
    }

    pub(crate) fn finish(&self, agent: &AgentId, ending: Ending) {
        self.change(|agents| {
            if let Some(entry) = agents.get_mut(agent) {
                entry.state = AgentState::Finished(ending);
            }
        });
    }

    /// 排进正在跑的子代理的邮箱；不在跑时返回 false。
    pub(crate) fn enqueue(&self, agent: &AgentId, message: String) -> bool {
        match self.agents().get_mut(agent).map(|entry| &mut entry.state) {
            Some(AgentState::Running { mailbox, .. }) => {
                mailbox.push(Mail::Message(message));
                true
            }
            _ => false,
        }
    }

    /// 工具边界取走邮箱里的一类（按到达顺序）；已中止时不取，留到运行结束时处理。
    fn take(&self, agent: &AgentId, notices: bool) -> Vec<Mail> {
        match self.agents().get_mut(agent).map(|entry| &mut entry.state) {
            Some(AgentState::Running { abort, mailbox }) if !abort.is_aborted() => {
                let (taken, kept) = std::mem::take(mailbox)
                    .into_iter()
                    .partition(|mail| matches!(mail, Mail::Notice(_)) == notices);
                *mailbox = kept;
                taken
            }
            _ => Vec::new(),
        }
    }

    /// SendMessage 排进来的消息。
    pub(crate) fn take_mailbox(&self, agent: &AgentId) -> Vec<String> {
        self.take(agent, false)
            .into_iter()
            .filter_map(|mail| match mail {
                Mail::Message(text) => Some(text),
                Mail::Notice(_) => None,
            })
            .collect()
    }

    /// 发给它的通知（信封）。
    pub(crate) fn take_notices(&self, agent: &AgentId) -> Vec<Notice> {
        self.take(agent, true)
            .into_iter()
            .filter_map(|mail| match mail {
                Mail::Notice(notice) => Some(notice),
                Mail::Message(_) => None,
            })
            .collect()
    }

    /// 中止正在跑的子代理；不在跑时返回 false（停放的由 `routing::stop_parked` 处理）。
    pub(crate) fn stop(&self, agent: &AgentId) -> bool {
        match self.agents().get(agent).map(|entry| &entry.state) {
            Some(AgentState::Running { abort, .. }) => {
                abort.abort(AbortReason::User);
                true
            }
            _ => false,
        }
    }
}

/// 状态转移之后统一记时刻：进入运行时重新开始计时，离开运行（停放、结束）时记结束时刻。状态改在很多处
/// （routing、TaskStop、SendMessage），时刻只在这里记，不在每个转移点各写一遍。
fn settle_times(agents: &mut Agents, now: u64) {
    for agent in agents.values_mut() {
        let running = matches!(agent.state, AgentState::Running { .. });
        match (running, agent.shown.ended_ms) {
            (true, Some(_)) => {
                agent.shown.started_ms = now;
                agent.shown.ended_ms = None;
            }
            (false, None) => {
                agent.shown.ended_ms = Some(now);
                agent.shown.doing = None;
            }
            _ => {}
        }
    }
}
