//! 前端使用的内核会话 API（docs/specs/rust-cli/README.md §3、§7）。
//!
//! TUI 与 headless 只经这里驱动内核；第二阶段的 ZCode Protocol 前端也一样。事件覆盖 spec §7
//! 列出的 V4 适配层输入，每个事件带会话内单调递增的 seq，并能从持久事实重建（§6 规则 13）。

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::env::WorkspaceRef;
use crate::ids::{
    AgentId, CommandId, InteractionId, MessageId, SessionId, ShellTaskId, ToolUseId, TurnId,
};
use crate::message::{CompactTrigger, Message, Usage};
use crate::model::{BlockDelta, ModelSelection};
use crate::orchestration::InteractionRequest;
use crate::permission::PermissionMode;
use crate::store::{ArtifactRef, SessionFilter, SessionMeta, SessionSummary};
use crate::tool::{ToolOutput, ToolProgress};

/// 运行中提交的输入怎样投递（工具边界吸收排队消息）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// 当前回合结束后作为新回合发送。
    Queue,
    /// 在下一个工具边界注入当前回合。
    Steer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewindScope {
    Conversation,
    Code,
    Both,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Submit {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<ArtifactRef>,
        delivery: Delivery,
    },
    Interrupt,
    /// 进程要退出（修复线 F38，docs/specs/rust-cli/session-lifecycle.md §3.5）：以 [`crate::abort::AbortReason::Shutdown`]
    /// 中止在跑的回合（之后这个回合不再记录），此后不再开回合。没有回合在跑时也 `Accepted`。
    Shutdown,
    /// esc 撤回刚发的输入（修复线 F38，session-lifecycle.md §5.5）：空闲、队列为空、
    /// 最后一条可选的人类输入之后没有回答时，对话回退到它之前（与 `Rewind{conversation}` 同一个 `session.revert`）。
    /// `Accepted.result` 是 `{message, removed}`：`message` 是这条输入（[`crate::message::Message`]，前端放回输入框）。
    /// 条件不满足时 `Rejected`。
    RestoreCancelled,
    ResolveInteraction {
        interaction: InteractionId,
        answer: Value,
    },
    SetModel {
        selection: ModelSelection,
    },
    /// 改权限模式（plan 开关不变）；`Plan` 是旧写法，只打开 plan（修复线 F20，shell-permissions.md §4.6）。
    SetMode {
        mode: PermissionMode,
    },
    /// 打开或关掉 plan 开关，权限模式不变（修复线 F20，TUI 的 `/plan`；Node `setExecutionState({planEnabled})`）。
    SetPlan {
        enabled: bool,
    },
    Compact {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    Rewind {
        target: MessageId,
        scope: RewindScope,
    },
    /// 从 `at` 处分叉出新会话；Ack 的 `result` 是新会话 ID。
    Fork {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<MessageId>,
    },
    /// 用户改名（`/rename`，Node `setCustomSessionTitle`）：标题来源记为 `custom`，之后不再自动生成
    /// （docs/specs/rust-cli/titles.md §4）。
    Rename {
        title: String,
    },
    /// 不带参数的 `/rename`（docs/specs/rust-cli/titles.md §8）：会话 actor
    /// 用模型从对话起名，再按 `Rename` 写入。回执等名字：`Accepted.result` 是 `{name, message}`，
    /// 起不出名字时 `Rejected.reason` 是给用户看的文本。
    GenerateName,
    /// 带内容块的输入（文字加粘贴的图片，docs/specs/rust-cli/tui-input.md §3）：回合与 [`Command::Submit`]
    /// 相同，输入消息的内容就是这些块；UPS、标题与斜杠展开只看文字块。
    SubmitContent {
        content: Vec<crate::message::ContentBlock>,
        delivery: Delivery,
    },
    /// bash 模式的输入（`!命令`，tui-input.md §2）：会话经 [`crate::user_input::UserInputs::bash`] 执行，
    /// 记下 `<bash-input>` 与输出两条 user 消息，再调模型。
    Bash {
        command: String,
        delivery: Delivery,
    },
    /// `/fork`（docs/specs/rust-cli/session-lifecycle.md §6.5，工作线 F75）：从一条可选的用户输入分出新会话，
    /// 新会话带到这一轮为止（这一轮最后一条 assistant），文件不动。只在空闲时接受；`Accepted.result` 是
    /// `{session, response, copied, message}`（`message` 是复制到的那条消息，F78）。`input` 不是可选的输入时 `Rejected`。
    ForkFromInput {
        input: MessageId,
    },
    /// 批准或拒绝一条扣住的同伴消息（docs/specs/rust-cli/cross-session.md §2.4，F28）：`message` 是
    /// [`crate::peers::HeldPeerMessage::msg_id`]。已经解决（过期、改模式放行）时 `Rejected`。
    ResolvePeerMessage {
        message: String,
        approve: bool,
    },
    /// 停止会话里的一个后台任务（`/tasks`，docs/specs/rust-cli/tui-display.md §3.1）：与 TaskStop 工具同一条
    /// 停止路径。`task` 是 [`crate::manage::BackgroundEntry::id`]；回执 `Accepted.result` 是 `{message}`，
    /// 找不到或不在跑时 `Rejected.reason` 是给用户看的文本。
    StopTask {
        task: String,
    },
    /// `/cd`（docs/specs/rust-cli/tui-manage.md §7）：把会话的工作目录换成 `root`（会话 actor 先设执行环境的
    /// shell cwd），`note` 是给模型的说明，作为 meta 用户消息一起提交。运行中时 `Rejected`。
    SetWorkspace {
        root: crate::env::EnvPath,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// `/cd` 由会话 actor 判定（docs/specs/rust-cli/tui-manage.md §7.1，F1）：运行中 `Rejected`（固定的文字）；
    /// 否则 `Accepted.result` 是 [`crate::manage::DirectoryOutcome`]。移动时 actor 设 shell cwd、改工作区、
    /// 发 [`KernelEvent::WorkspaceChanged`]，给模型的说明（固定的说明文字加新目录的规则文件）由 actor 组成并提交。
    ChangeDirectory {
        root: crate::env::EnvPath,
    },
    /// 同 [`Command::ChangeDirectory`]，移动成功时 `record` 与说明文字同一批、排在它之前提交（修复线 L-A，
    /// docs/specs/rust-cli/tui-manage.md §11.2：对照实现的 `/cd` 先记命令输入与输出，说明文字是之后的 meta 消息）。
    ChangeDirectoryRecorded {
        root: crate::env::EnvPath,
        record: crate::user_command::LocalCommandRecord,
    },
    /// 本地命令写进对话的记录（修复线 L-A，docs/specs/rust-cli/tui-manage.md §11）：空闲时马上提交一批；回合或压缩在跑时
    /// 留到它收尾、开下一个回合之前提交。总是 `Accepted`（提交失败只记日志）。
    RecordLocalCommand {
        record: crate::user_command::LocalCommandRecord,
    },
    /// 把这个会话在跑的前台 Bash 转到后台（修复线 L-A，docs/specs/rust-cli/tui-input.md §11）：用户的转后台键
    /// （`User`），或低延迟投递的输入（`DeliverMessage`）。会话 actor 交给本机的执行环境；总是 `Accepted`，`result`
    /// 是转了的个数。
    BackgroundForeground {
        reason: crate::env_command::BackgroundReason,
    },
    /// 把续写时补在内存里、还没落库的 assistant（`No response requested.`，docs/specs/rust-cli/session-lifecycle.md
    /// §6.3.1，工作线 F79；有补给未结调用的中断结果时一起，§6.3，工作线 F81）现在单独提交一批。交互界面在会话里切到这个会话（`/resume`、`/fork`）之后发：切过去就算换了
    /// 对话。没有要写的（没有补、已经写过、被回退掉）时什么都不做，回执是 `Accepted`；提交失败时 `Rejected`，它留到下一批
    /// 带消息的提交。
    StoreResumeSentinel,
}

/// 带 ID 的命令；同一个 ID 重复提交时返回 [`Ack::Duplicate`]，不重复执行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub id: CommandId,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Ack {
    Accepted {
        #[serde(default, skip_serializing_if = "Value::is_null")]
        result: Value,
    },
    Duplicate,
    Rejected {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TurnOutcome {
    Success,
    Interrupted,
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    /// 文件所在的远端机器（远端执行 F98，docs/specs/rust-cli/remote-exec.md §25.4.5：`/rewind` 写成 `dev-a:README.md`）；
    /// 本机为 None，不写出。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedInput {
    pub command: CommandId,
    pub text: String,
    pub delivery: Delivery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentPhase {
    Spawned,
    Done,
}

/// 只供显示的系统提示的级别（informational system 消息的 `level`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
}

/// 内核事件。新增变体时同步更新前端与 spec §7。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KernelEvent {
    SessionCreated {
        meta: SessionMeta,
    },
    TitleChanged {
        title: String,
    },
    ModelChanged {
        selection: ModelSelection,
    },
    /// 权限模式变了（修复线 F20 起只带权限模式，不会是 `Plan`；plan 开关见 [`KernelEvent::PlanChanged`]）。
    ModeChanged {
        mode: PermissionMode,
    },
    /// plan 开关变了（修复线 F20）。
    PlanChanged {
        enabled: bool,
    },
    TurnStarted {
        turn: TurnId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<CommandId>,
    },
    TurnFinished {
        turn: TurnId,
        outcome: TurnOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
    /// 正文、thinking、工具输入的流式增量；`response` 是本回合内第几次模型响应。
    StreamDelta {
        turn: TurnId,
        response: u32,
        index: usize,
        delta: BlockDelta,
    },
    /// 一条消息已完整并已落库。
    MessageDone {
        message: Message,
    },
    ToolStarted {
        tool_use: ToolUseId,
        name: String,
        input: Value,
    },
    ToolProgress {
        tool_use: ToolUseId,
        progress: ToolProgress,
    },
    ToolDone {
        tool_use: ToolUseId,
        output: ToolOutput,
    },
    FileChanges {
        turn: TurnId,
        files: Vec<FileChange>,
        rewindable: bool,
    },
    InteractionAsked {
        interaction: InteractionId,
        request: InteractionRequest,
    },
    InteractionResolved {
        interaction: InteractionId,
    },
    ContextUsage {
        used_tokens: u64,
        max_tokens: u64,
    },
    /// 上下文快满的提示（docs/specs/rust-cli/compact.md §2.1）：提交带用量的 assistant 与压缩结果之后重算。
    ContextNotice {
        notice: crate::compaction::ContextNotice,
    },
    TasksChanged {
        tasks: Value,
    },
    QueueChanged {
        items: Vec<QueuedInput>,
    },
    BackgroundWork {
        work: String,
        status: String,
    },
    Subagent {
        agent: AgentId,
        phase: SubagentPhase,
        child: SessionId,
        parent_tool_use: ToolUseId,
    },
    CompactStarted {
        trigger: CompactTrigger,
    },
    CompactDone {
        boundary: MessageId,
    },
    /// 历史被改写（rewind、编辑重发）；前端丢弃旧视图后重新取快照。
    HistoryRewritten {
        generation: u64,
    },
    /// 定时任务触发的界面行（tools-more.md §18.3，L22）：不进模型上下文、不落库，
    /// 紧接着是这次触发开的回合。`at` 是本地时间（`Oct 18 3:04pm`）；`fold` 有值时前端收起上一个 loop tick。
    ScheduledFire {
        at: String,
        kind: crate::cron::CronFireKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fold: Option<crate::cron::LoopFold>,
    },
    /// 只供显示的系统提示，不发给模型、不落库（L18：插话被 UserPromptSubmit 阻塞，
    /// docs/specs/rust-cli/query.md §4.2）。
    SystemNotice {
        level: NoticeLevel,
        text: String,
    },
    /// 会话的工作目录换了（EnterWorktree / ExitWorktree / `/cd`，F1）：前端的工作区只从这里与快照的
    /// `meta.workspace` 取（tools-more.md §11.4、tui-manage.md §7.1）。
    WorkspaceChanged {
        workspace: WorkspaceRef,
    },
    /// 扣住的同伴消息变了（cross-session.md §2.4，F28）：整份快照，界面的批准框只从它与
    /// [`SessionView::peer_held`] 取。
    PeerHeldChanged {
        held: Vec<crate::peers::HeldPeerMessage>,
    },
    /// 主线程模型请求的通知（重试、停顿、恢复）：不落库，前端用它显示重试与停顿
    /// （docs/specs/rust-cli/stall-visibility.md §3）。
    ModelNotice {
        notice: crate::model::ModelNotice,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequencedEvent {
    pub seq: u64,
    pub event: KernelEvent,
    /// 会话的状态 revision（缺口 G1，docs/specs/rust-cli/app-protocol.md §6.4）：会话 actor 在可见状态变化时加一
    /// （回合开始与结束、消息落定、工具开始与结束、询问、队列、模式、模型、plan、标题、历史改写、压缩、工作区、子代理与
    /// 后台任务）；流式增量、进度、用量与只供显示的提示不加。新会话从 0 开始，只在 actor 的寿命内单调（进程重启后从 0
    /// 重来）。这条事件之后的值；serde 缺省 0。
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateSession {
    /// 第二阶段由 Host 给出会话 ID；为空时内核生成。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<SessionId>,
    pub workspace: WorkspaceRef,
    pub selection: ModelSelection,
    /// 权限模式；`Plan` 是旧写法（plan 开、default）。
    pub mode: PermissionMode,
    /// plan 开关（修复线 F20；serde 缺省 false）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
}

/// 会话的当前视图（前端重连或首次打开时用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionView {
    pub meta: SessionMeta,
    /// 显示用的历史：冷读取时是只供显示的压缩点之前的消息（[`crate::store::LoadedSession::earlier`]）接上会话 actor 的
    /// 历史（工作线 F102，docs/specs/rust-cli/compact.md §5.1）。要模型侧的那一段时取 [`crate::history::active`]。
    pub messages: Vec<Message>,
    pub selection: ModelSelection,
    /// 权限模式（修复线 F20 起不会是 `Plan`）。
    pub mode: PermissionMode,
    /// plan 开关（修复线 F20；serde 缺省 false）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
    pub pending: Vec<(InteractionId, InteractionRequest)>,
    pub queue: Vec<QueuedInput>,
    pub last_seq: u64,
    /// `last_seq` 时的状态 revision（缺口 G1，同 [`SequencedEvent::revision`]；serde 缺省 0）。
    #[serde(default)]
    pub revision: u64,
    /// 有回合正在运行。
    #[serde(default)]
    pub running: bool,
    /// 执行环境里还没有处理完的后台命令（`ShellState::background`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background: Vec<ShellTaskId>,
    /// 扣住、等用户批准的同伴消息（cross-session.md §2.4，F28；serde 缺省为空）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peer_held: Vec<crate::peers::HeldPeerMessage>,
    /// 上下文用量（token、有效窗口）：会话 actor 按当前历史与模型经 `Compactor::context_usage` 算（compact.md §2.1、
    /// tui-display.md §2.6，修复线 F27）；压缩模块不知道时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<crate::compaction::ContextOccupancy>,
    /// 上下文快满的提示（同一处算，F27）：冷加载后不用等下一条回复。
    #[serde(default)]
    pub context_notice: crate::compaction::ContextNotice,
}

// 会话生命周期的类型（SessionStart / SessionEnd 的来源、rewind 选择器的条目）放在分出的文件里（400 行上限，
// L24 合并后拆出；路径与名字不变）。
#[path = "session_api_lifecycle.rs"]
mod lifecycle;
pub use lifecycle::{EndReason, RewindOption, StartSource};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KernelError {
    #[error("session not found: {session}")]
    NotFound { session: SessionId },
    #[error("{message}")]
    Failed { message: String },
    #[error("not implemented: {what}")]
    Unsupported { what: String },
}

#[async_trait]
pub trait SessionHandle: Send + Sync {
    fn id(&self) -> &SessionId;

    async fn send(&self, command: CommandEnvelope) -> Ack;

    /// 订阅 `after` 之后的事件；`after` 为空时从当前位置开始。
    fn subscribe(&self, after: Option<u64>) -> BoxStream<'static, SequencedEvent>;

    async fn view(&self) -> Result<SessionView, KernelError>;

    /// 前端离开会话时调用：运行 SessionEnd hook，返回只给用户看的提示（失败的 hook）。
    async fn close(&self, _reason: EndReason) -> Vec<String> {
        Vec::new()
    }

    /// rewind 选择器的条目（活动历史里可选的 user 消息，按顺序）。
    async fn rewind_options(&self) -> Result<Vec<RewindOption>, KernelError> {
        Err(KernelError::Unsupported {
            what: "rewind options".to_owned(),
        })
    }

    /// `/goal` 面板的数据：会话 actor 的当前目标与最近一次达成的结果（docs/specs/rust-cli/goal.md §6.2）。
    async fn goal(&self) -> Result<crate::goal::GoalView, KernelError> {
        Err(KernelError::Unsupported {
            what: "goal".to_owned(),
        })
    }

    /// 会话里还在跑的后台命令与后台子代理（`/tasks` 与 footer 的计数，tui-display.md §2.3、§3.1）。
    async fn background_tasks(&self) -> Result<Vec<crate::manage::BackgroundEntry>, KernelError> {
        Err(KernelError::Unsupported {
            what: "background tasks".to_owned(),
        })
    }

    /// 这个顶层会话里本进程启动过的子代理（含嵌套的），按启动顺序（tui-panels.md §1.1，L30）。
    async fn subagents(&self) -> Result<Vec<crate::panels::SubagentEntry>, KernelError> {
        Err(KernelError::Unsupported {
            what: "subagents".to_owned(),
        })
    }

    /// 子代理的状态有变化时产生一项（多次变化可能合并成一项）；收到后重取 [`SessionHandle::subagents`]
    /// （修复线 F34 起前端重取 [`SessionHandle::tasks_snapshot`]）。
    fn subagent_changes(&self) -> BoxStream<'static, ()> {
        futures_util::stream::pending().boxed()
    }

    /// 子代理列表与后台任务的同一份快照（修复线 F34，tui-panels.md §0、§3.1）：一次查询、一次回执。前端在子代理的变化
    /// 通知与后台任务相关的事件之后都重取它，不再分别取 [`SessionHandle::subagents`] 与
    /// [`SessionHandle::background_tasks`]（两者保留，只加不改）。缺省实现依次取那两个方法（不是同一次读取，只为别的
    /// 实现照常可用）。
    async fn tasks_snapshot(&self) -> Result<crate::panels::TasksSnapshot, KernelError> {
        Ok(crate::panels::TasksSnapshot {
            subagents: self.subagents().await?,
            background: self.background_tasks().await?,
        })
    }

    /// 子代理的只读快照：子会话里已提交的消息；`running` 是它是否在跑（tui-panels.md §1.4）。
    async fn subagent_view(&self, _agent: &AgentId) -> Result<SessionView, KernelError> {
        Err(KernelError::Unsupported {
            what: "subagent view".to_owned(),
        })
    }

    /// 一个后台命令的详情（Shell 详情，tui-panels.md §3.3）；不认识时为 None。
    async fn shell_detail(
        &self,
        _task: &str,
    ) -> Result<Option<crate::panels::ShellDetail>, KernelError> {
        Err(KernelError::Unsupported {
            what: "shell detail".to_owned(),
        })
    }
}

#[async_trait]
pub trait Kernel: Send + Sync {
    async fn create_session(&self, request: CreateSession) -> Result<SessionId, KernelError>;

    async fn open_session(
        &self,
        session: &SessionId,
    ) -> Result<Arc<dyn SessionHandle>, KernelError>;

    async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, KernelError>;

    /// 同 [`Kernel::create_session`]，另给 SessionStart 的来源（`/clear` 之后的新会话是 `Clear`）。
    async fn create_session_as(
        &self,
        request: CreateSession,
        _source: StartSource,
    ) -> Result<SessionId, KernelError> {
        self.create_session(request).await
    }

    /// 同 [`Kernel::open_session`]，另给 SessionStart 的来源（刚 fork 出的会话是 `Fork`）。
    /// 会话已经打开时来源不起作用。
    async fn open_session_as(
        &self,
        session: &SessionId,
        _source: StartSource,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        self.open_session(session).await
    }

    /// 上下文用量（`/context`，docs/specs/rust-cli/context-usage.md）：只读，不改会话。
    async fn context_usage(
        &self,
        _query: crate::context_usage::ContextUsageQuery,
    ) -> Result<crate::context_usage::ContextUsage, KernelError> {
        Err(KernelError::Unsupported {
            what: "context usage".to_owned(),
        })
    }

    /// `@` 补全的文件清单（docs/specs/rust-cli/tui-input.md §1.2）：相对 shell 当前目录的文件路径。
    async fn workspace_files(
        &self,
        _workspace: &crate::env::WorkspaceRef,
    ) -> Result<Vec<String>, KernelError> {
        Err(KernelError::Unsupported {
            what: "workspace files".to_owned(),
        })
    }

    /// 这个工作区里用户可调用的斜杠命令（skill，TUI 的补全；ext.md §2.7，L22）。默认没有。
    async fn user_commands(
        &self,
        _workspace: &crate::env::WorkspaceRef,
    ) -> Vec<crate::user_command::UserCommand> {
        Vec::new()
    }

    /// MCP 服务器的状态（`/mcp`、`/status`，tui-display.md §3.4，L28）。默认没有。
    async fn mcp_servers(&self) -> Vec<crate::manage::McpServerInfo> {
        Vec::new()
    }

    /// `/plan open`（tui-manage.md §8，L29）：会话的计划文件位置，以及它是否有内容。
    async fn plan_file(
        &self,
        _query: crate::manage::PlanFileQuery,
    ) -> Result<crate::manage::PlanFile, KernelError> {
        Err(KernelError::Unsupported {
            what: "plan file".to_owned(),
        })
    }

    /// `/cd <path>`（tui-manage.md §7，L29）：解析与检查路径；有会话时经会话 actor 换工作目录。错误是给用户看的
    /// 文字（`Failed`）。
    async fn change_directory(
        &self,
        _request: crate::manage::ChangeDirectory,
    ) -> Result<crate::manage::DirectoryChanged, KernelError> {
        Err(KernelError::Unsupported {
            what: "changing the working directory".to_owned(),
        })
    }

    /// `/mcp reconnect|enable|disable`（tui-manage.md §6，L29）：经 ext 的连接管理改连接，返回这些服务器
    /// 改动后的状态。默认 `Unsupported`。
    async fn mcp_control(
        &self,
        _control: crate::manage::McpControl,
    ) -> Result<Vec<crate::manage::McpServerInfo>, KernelError> {
        Err(KernelError::Unsupported {
            what: "MCP server control".to_owned(),
        })
    }

    /// 当前生效的 hook 配置（`/hooks`，tui-display.md §3.5，L28）：只读。默认没有。
    async fn hooks(&self) -> Vec<crate::manage::HookEntry> {
        Vec::new()
    }

    /// `/diff` 的未提交改动（tui-manage.md §4，L29）：执行环境里 git 的原始输出。
    async fn uncommitted_diff(
        &self,
        _workspace: &crate::env::WorkspaceRef,
    ) -> Result<crate::env_git::GitDiff, KernelError> {
        Err(KernelError::Unsupported {
            what: "uncommitted diff".to_owned(),
        })
    }

    /// `/export <文件名>`（tui-manage.md §3，L29）：经执行环境写文件，返回写入的绝对路径。
    async fn write_export(&self, _file: crate::manage::ExportFile) -> Result<String, KernelError> {
        Err(KernelError::Unsupported {
            what: "exporting the conversation".to_owned(),
        })
    }

    /// `/permissions` 的规则列表（tui-manage.md §1.1，L29）：规则快照、本机项目规则集（现读）与会话规则。
    async fn permission_rules(
        &self,
        _query: crate::permission::PermissionRulesQuery,
    ) -> Result<Vec<crate::permission::SourcedRule>, KernelError> {
        Err(KernelError::Unsupported {
            what: "permission rules".to_owned(),
        })
    }

    /// `/permissions` 的修改（tui-manage.md §1.1）：落库后换上新的规则快照，返回修改后的列表。
    async fn change_permission_rules(
        &self,
        _change: crate::permission::PermissionRulesChange,
    ) -> Result<Vec<crate::permission::SourcedRule>, KernelError> {
        Err(KernelError::Unsupported {
            what: "changing permission rules".to_owned(),
        })
    }

    /// `/usage` 的 API 时长（docs/specs/rust-cli/usage.md §4）：从存储的用量表查。默认 `Unsupported`。
    async fn session_metering(
        &self,
        _session: &SessionId,
    ) -> Result<crate::metering::SessionMetering, KernelError> {
        Err(KernelError::Unsupported {
            what: "session metering".to_owned(),
        })
    }

    /// footer 的请求统计（TPS 与缓存命中率，docs/specs/rust-cli/tui-display.md §2.6，修复线 F27）：从存储的用量表查。
    /// 默认 `Unsupported`。
    async fn request_stats(
        &self,
        _session: &SessionId,
    ) -> Result<crate::metering::RequestStats, KernelError> {
        Err(KernelError::Unsupported {
            what: "request stats".to_owned(),
        })
    }

    /// 活动统计（`/stats`，usage.md §5）：今天与时区偏移取自内核的时钟，聚合在存储。默认 `Unsupported`。
    async fn activity(
        &self,
        _range: crate::metering::ActivityRange,
    ) -> Result<crate::metering::ActivityStats, KernelError> {
        Err(KernelError::Unsupported {
            what: "activity stats".to_owned(),
        })
    }

    /// ← 项目页与 `/resume` 页的会话（修复线 F29，tui-panels.md §2.1）：存储的查询加本进程里打开着的会话的
    /// 实时状态与目录的分支。默认 `Unsupported`。
    async fn project_sessions(
        &self,
        _query: crate::store::SessionQuery,
    ) -> Result<crate::panels::ProjectSessions, KernelError> {
        Err(KernelError::Unsupported {
            what: "project sessions".to_owned(),
        })
    }

    /// 给会话改名（F29，tui-manage.md §10.6）：打开着的经它的 actor（`Command::Rename`），否则直接写存储。
    async fn rename_session(
        &self,
        _session: &SessionId,
        _title: String,
    ) -> Result<(), KernelError> {
        Err(KernelError::Unsupported {
            what: "renaming sessions".to_owned(),
        })
    }

    /// 删除会话（F29，tui-manage.md §10.6）：打开着的拒绝（`Failed`），否则存储软删除。
    async fn delete_session(&self, _session: &SessionId) -> Result<(), KernelError> {
        Err(KernelError::Unsupported {
            what: "deleting sessions".to_owned(),
        })
    }

    /// 会话的只读快照（F29，`/resume` 页的预览与全屏对话）：打开着的问它的 actor，否则从存储冷加载；不打开会话、
    /// 不启动 actor、不跑 SessionStart。
    async fn session_snapshot(&self, _session: &SessionId) -> Result<SessionView, KernelError> {
        Err(KernelError::Unsupported {
            what: "session snapshots".to_owned(),
        })
    }

    /// 会话可用的工具、agent 与 skill（`-p` 的 stream-json 写 `system/init`，docs/specs/rust-cli/headless.md §3.3，
    /// 修复线 F39）：只读。默认 `Unsupported`。
    async fn session_catalog(
        &self,
        _query: crate::catalog::CatalogQuery,
    ) -> Result<crate::catalog::SessionCatalog, KernelError> {
        Err(KernelError::Unsupported {
            what: "session catalog".to_owned(),
        })
    }

    /// 旧会话的询问（docs/specs/rust-cli/agents.md §4.1.2，工作线 F72）：先给出还没回答的，再给出之后的变化。缺省
    /// 没有（流不结束）。
    fn detached_asks(&self) -> BoxStream<'static, crate::interaction::DetachedEvent> {
        futures_util::stream::pending().boxed()
    }

    /// 回答旧会话的询问（回答的形状同 [`Command::ResolveInteraction`]）。缺省 `Unsupported`。
    async fn resolve_detached(
        &self,
        _interaction: &InteractionId,
        _answer: Value,
    ) -> Result<(), KernelError> {
        Err(KernelError::Unsupported {
            what: "answering questions of closed sessions".to_owned(),
        })
    }

    /// 写一条前端记录（缺口 G2，docs/specs/rust-cli/app-protocol.md §5.2）：内核不解释它，经存储端口原样落库（同 key
    /// 覆盖）；会话不存在时 `NotFound`。缺省 `Unsupported`。
    async fn put_session_record(
        &self,
        _session: &SessionId,
        _record: crate::store::FrontendRecord,
    ) -> Result<(), KernelError> {
        Err(KernelError::Unsupported {
            what: "frontend records".to_owned(),
        })
    }

    /// 这个会话里某一种前端记录，按写入顺序（缺口 G2）。缺省没有。
    async fn session_records(
        &self,
        _session: &SessionId,
        _kind: &str,
    ) -> Result<Vec<crate::store::FrontendRecord>, KernelError> {
        Ok(Vec::new())
    }

    /// 工作区的会话（与 [`Kernel::list_sessions`] 同一个来源），合并本进程里打开着的会话的实时状态
    /// （← 面板，tui-panels.md §2.1，L30）。默认没有实时状态。
    async fn workspace_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<crate::panels::WorkspaceSession>, KernelError> {
        Ok(self
            .list_sessions(filter)
            .await?
            .into_iter()
            .map(|summary| crate::panels::WorkspaceSession {
                summary,
                activity: None,
                last_reply: None,
            })
            .collect())
    }
}
