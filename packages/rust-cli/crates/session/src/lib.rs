//! 会话 actor：一个会话的状态的唯一 owner（docs/specs/rust-cli/README.md §3、§8）。
//!
//! 接收命令（按 CommandId 幂等）、排队、驱动主循环、把主循环事件转成持久事实（先提交再发事件）、
//! 给事件分配 seq。前端只经 [`SessionHandle`] 访问。
//!
//! 已实现：回合、排队、中断、切换模型与模式、主循环的提交屏障（[`sink`]）、待回答的交互
//! （先持久化再通知）；会话侧 hooks、文件 checkpoint、rewind、fork 与中断后续跑
//! （docs/specs/rust-cli/session-lifecycle.md）。

mod actor;
mod agent_signals;
mod bash;
mod checkpoints;
mod closing;
mod compact;
mod cron;
mod fork;
mod fork_checkpoint;
mod goal;
mod hooks;
#[cfg(test)]
mod input_tests;
mod lane;
mod log;
mod loop_fold;
mod loop_tick;
#[cfg(test)]
mod metering_tests;
mod mode;
mod notices;
mod peer_receipts;
mod peer_restore;
mod peers;
mod rename;
mod rename_text;
mod resume;
mod rewind;
mod rewind_plan;
mod rules;
mod sink;
mod slash;
mod steering;
mod tasks;
#[cfg(test)]
mod test_support;
mod title;
mod title_model;
mod title_parse;
mod turn;
mod watched;
mod workspace;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_base::{Spawner, Task};
use rust_cli_kernel_api::agents::AgentSignals;
use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::env::Environment;
use rust_cli_kernel_api::ids::{Generation, SessionId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::{ModelPort, ModelSelection};
use rust_cli_kernel_api::orchestration::{Compactor, HookRunner, QueryRunner};
use rust_cli_kernel_api::session_api::{
    Ack, CommandEnvelope, EndReason, KernelError, KernelEvent, RewindOption, SequencedEvent,
    SessionHandle, SessionView, StartSource,
};
use rust_cli_kernel_api::store::{ArtifactStore, SessionMeta, SessionStore, WorkspaceCheckpoint};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, Request};
use crate::log::EventLog;
pub use crate::peers::PeerDeps;
pub use crate::title_model::TitleModel;

/// 命令队列的缓冲；满了时发送方等待。
const REQUESTS: usize = 64;

/// 会话 actor 依赖的能力。
#[derive(Clone)]
pub struct SessionDeps {
    pub store: Arc<dyn SessionStore>,
    pub ids: Arc<dyn Ids>,
    pub runner: Arc<dyn QueryRunner>,
    /// 手动压缩（`Command::Compact`，docs/specs/rust-cli/compact.md §4.2）。
    pub compactor: Arc<dyn Compactor>,
    /// 执行环境：订阅后台命令的终态通知、查询后台工作（docs/specs/rust-cli/query.md §5）。
    pub env: Arc<dyn Environment>,
    /// 每次提问最多几轮（`--max-turns`）。
    pub max_turns: Option<u32>,
    /// 子代理的信号：后台子代理的终态通知、任务清单变化（docs/specs/rust-cli/agents.md §5）。
    pub agents: Arc<dyn AgentSignals>,
    /// 会话内定时任务的唤醒与到期任务（docs/specs/rust-cli/tools-more.md §10.4）；`-p` 是 `NoCron`。
    pub cron: Arc<dyn rust_cli_kernel_api::cron::CronSignals>,
    /// 会话侧 hooks（UserPromptSubmit、SessionStart、SessionEnd、Notification）。
    pub hooks: Arc<dyn HookRunner>,
    /// 用户输入的斜杠命令（skill 的用户调用，docs/specs/rust-cli/ext.md §2.7）：回合任务先展开再跑 UPS。
    pub commands: Arc<dyn rust_cli_kernel_api::user_command::UserCommands>,
    /// 用户输入的 `@` 提及与 bash 模式（docs/specs/rust-cli/tui-input.md §1.1、§2.2）：回合任务在斜杠展开之后调用。
    pub inputs: Arc<dyn rust_cli_kernel_api::user_input::UserInputs>,
    /// checkpoint 的 artifact（rewind 读回写入前后的内容）。
    pub artifacts: Arc<dyn ArtifactStore>,
    /// fork 出的会话的创建时间。
    pub clock: Arc<dyn Clock>,
    /// 本地执行环境实例的 generation（回退事务的操作上下文）。
    pub generation: Generation,
    /// 会话自己的任务作用域：actor 与主循环都在这里启动。
    pub spawner: Spawner,
    /// 关闭任务的作用域（内核的根作用域，session-lifecycle.md §3.3.1）：关闭不依赖调用方的 future；会话自己的作用域
    /// 在关闭的最后一步释放，关闭任务不能在里面跑。
    pub closing: Spawner,
    /// 会话标题的生成（交互形态才有；`-p` 为 None，docs/specs/rust-cli/titles.md §2）。
    pub titles: Option<TitleModel>,
    /// 会话 actor 自己发的旁路请求用的模型端口（不带参数的 `/rename` 起名，titles.md §8；`-p` 也有）。
    pub side_model: Arc<dyn ModelPort>,
    /// 关闭之后由组装方释放的按会话状态（session-lifecycle.md §3.3）。
    pub released: Arc<dyn Released>,
    /// `/cd` 的说明里新目录的规则文件（tui-manage.md §7.1）：actor 用自己的活动历史调用。
    pub directory_rules: Arc<dyn rust_cli_kernel_api::orchestration::DirectoryRules>,
    /// 本机跨会话通信（docs/specs/rust-cli/cross-session.md §2.4）。
    pub peers: PeerDeps,
}

/// 会话关闭的收尾（docs/specs/rust-cli/session-lifecycle.md §3.3、§3.3.1）：每个会话的关闭任务各调用一次。
pub trait Released: Send + Sync {
    /// 关闭任务的第一步（早于 SessionEnd）：组装方把会话表里的条目标成“关闭中”，之后打开同一个会话的调用等关闭走完。
    fn closing(&self, _session: &SessionId) {}

    /// 关闭任务的最后一步（actor 退出、执行环境与定时任务释放之后）：组装方（runtime）清掉各能力按会话保存的表，
    /// 最后把会话移出会话表、释放它的任务作用域。
    fn released(&self, session: &SessionId);
}

/// 没有按会话的表要释放（测试与嵌入）。
pub struct NothingToRelease;

impl Released for NothingToRelease {
    fn released(&self, _session: &SessionId) {}
}

/// 打开会话时的状态（新建或冷加载）。
pub struct SessionState {
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
    pub selection: ModelSelection,
    /// 执行状态（权限模式 × plan 开关，修复线 F20）；旧的单值模式经 `From<PermissionMode>`（`Plan` 是 plan 开）。
    pub mode: rust_cli_kernel_api::permission::ExecutionState,
    /// 还没有被代码回退撤销的 checkpoint（冷加载的 `LoadedSession.checkpoints`）。
    pub checkpoints: Vec<WorkspaceCheckpoint>,
    /// SessionStart hook 的来源（session-lifecycle.md §3.2）。
    pub start: StartSource,
    /// 当前轮（冷加载的 `LoadedSession.turn`；新建的会话没有）。
    pub turn: Option<rust_cli_kernel_api::store::TurnAnchor>,
}

/// 会话是新建的还是从存储加载的；新建时发出 `SessionCreated`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    Created,
    Loaded,
}

pub struct Session {
    id: SessionId,
    requests: mpsc::Sender<Request>,
    log: Arc<EventLog>,
    deps: SessionDeps,
    /// 关闭的进度（[`closing`]）：None 还没关；Some 关闭任务已经起了，令牌在它结束时取消。
    closing: Mutex<Option<CancellationToken>>,
    _actor: Task<Option<()>>,
}

impl Session {
    pub fn spawn(deps: SessionDeps, mut state: SessionState, opened: Opened) -> Self {
        let id = state.meta.id.clone();
        let log = Arc::new(EventLog::new());
        match opened {
            Opened::Created => {
                log.append(KernelEvent::SessionCreated {
                    meta: state.meta.clone(),
                });
            }
            // 中断后续跑：去掉所有调用都没有结果的 assistant。
            Opened::Loaded => state.messages = resume::filter_unresolved(state.messages),
        }
        let (requests, receiver) = mpsc::channel(REQUESTS);
        let spawner = deps.spawner.clone();
        // 从存储打开的会话：进入循环之前先让任务表从活动历史复原会话任务（tools-more.md
        // §18.2），第一次 `take_due` 时任务表已就绪。新建的会话没有历史，不调。
        let resume = (opened == Opened::Loaded).then(|| state.messages.clone());
        let cron = deps.cron.clone();
        let session = id.clone();
        let actor = Actor::new(deps.clone(), log.clone(), state, requests.downgrade());
        let task = spawner.spawn_resident("session-actor", async move {
            if let Some(history) = resume {
                cron.resume(&session, &history).await;
            }
            actor.run(receiver).await
        });
        Self {
            id,
            requests,
            log,
            deps,
            closing: Mutex::default(),
            _actor: task,
        }
    }

    async fn ask<T>(
        &self,
        request: impl FnOnce(oneshot::Sender<T>) -> Request,
    ) -> Result<T, KernelError> {
        closing::ask(&self.requests, request).await
    }
}

fn closed() -> KernelError {
    KernelError::Failed {
        message: "the session is closed".to_owned(),
    }
}

#[async_trait]
impl SessionHandle for Session {
    fn id(&self) -> &SessionId {
        &self.id
    }

    async fn send(&self, envelope: CommandEnvelope) -> Ack {
        let (reply, answer) = oneshot::channel();
        if self
            .requests
            .send(Request::Command { envelope, reply })
            .await
            .is_err()
        {
            return Ack::Rejected {
                reason: closed().to_string(),
            };
        }
        answer.await.unwrap_or_else(|_| Ack::Rejected {
            reason: closed().to_string(),
        })
    }

    fn subscribe(&self, after: Option<u64>) -> BoxStream<'static, SequencedEvent> {
        self.log.subscribe(after)
    }

    async fn view(&self) -> Result<SessionView, KernelError> {
        self.ask(|reply| Request::View { reply }).await
    }

    /// SessionEnd 在关闭任务里跑（hook 只读 meta），不占用 actor（spec §3.3）。之后关闭 actor、释放执行环境
    /// 与定时任务、交组装方释放按会话的表；只关一次，关闭任务不依赖调用方（[`closing`]，§3.3.1）。
    ///
    /// 修复（R1-1）：以前这里只跑 SessionEnd 与 `cron.session_closed`，actor 一直活着（内核的会话表持有句柄）：
    /// `/clear`、`/resume`、fork 之后旧会话收到后台通知就在用户看不到的地方自己开回合，按会话的表与 EventLog 不释放，
    /// `Environment::close_session` 没有调用方，再打开同一会话拿到旧 actor、不跑 SessionStart。现在：前台工作
    /// 中止，后台工作保留（`/clear`、`/resume` 不 `forget`），actor 退出，组装方把会话移出会话表。
    async fn close(&self, reason: EndReason) -> Vec<String> {
        closing::close(self, reason).await
    }

    /// actor 给出可选消息与 checkpoint 的快照；读 artifact 算统计在调用方的任务里做。
    async fn rewind_options(&self) -> Result<Vec<RewindOption>, KernelError> {
        let snapshot = self.ask(|reply| Request::RewindOptions { reply }).await?;
        Ok(rewind::options(self.deps.artifacts.as_ref(), snapshot).await)
    }

    async fn goal(&self) -> Result<rust_cli_kernel_api::goal::GoalView, KernelError> {
        self.ask(|reply| Request::Goal { reply }).await
    }

    async fn checkpoints(
        &self,
    ) -> Result<Vec<rust_cli_kernel_api::store::WorkspaceCheckpoint>, KernelError> {
        self.ask(|reply| Request::Checkpoints { reply }).await
    }

    /// 后台任务的事实不在 actor 里（执行环境的账本与 agents）：取 meta 后在调用方的任务里汇总
    /// （docs/specs/rust-cli/tui-display.md §3.1）。
    async fn background_tasks(
        &self,
    ) -> Result<Vec<rust_cli_kernel_api::manage::BackgroundEntry>, KernelError> {
        let meta = self.ask(|reply| Request::Meta { reply }).await?;
        Ok(tasks::background_entries(&self.deps, &meta.workspace, &meta.id).await)
    }

    /// 子代理的事实在 agents 里（tui-panels.md §1）：不经 actor，直接转给它，按这个顶层会话过滤。
    async fn subagents(
        &self,
    ) -> Result<Vec<rust_cli_kernel_api::panels::SubagentEntry>, KernelError> {
        Ok(self.deps.agents.subagents(&self.id).await)
    }

    fn subagent_changes(&self) -> BoxStream<'static, ()> {
        self.deps.agents.subagent_changes()
    }

    /// 子代理列表与后台任务的同一份快照（修复线 F34，tui-panels.md §0）：actor 只给出工作区，命令与子代理在调用方的
    /// 任务里取，一次回执。
    async fn tasks_snapshot(
        &self,
    ) -> Result<rust_cli_kernel_api::panels::TasksSnapshot, KernelError> {
        let meta = self.ask(|reply| Request::Meta { reply }).await?;
        Ok(tasks::snapshot(&self.deps, &meta.workspace, &meta.id).await)
    }

    async fn subagent_view(
        &self,
        agent: &rust_cli_kernel_api::ids::AgentId,
    ) -> Result<SessionView, KernelError> {
        self.deps.agents.subagent_view(&self.id, agent).await
    }

    /// Shell 详情：与 `background_tasks` 同一份账本与输出文件（tui-panels.md §3.3）。
    async fn shell_detail(
        &self,
        task: &str,
    ) -> Result<Option<rust_cli_kernel_api::panels::ShellDetail>, KernelError> {
        let meta = self.ask(|reply| Request::Meta { reply }).await?;
        Ok(tasks::shell_detail(&self.deps, &meta.workspace, &meta.id, task).await)
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
