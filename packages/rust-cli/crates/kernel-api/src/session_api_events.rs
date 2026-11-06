//! 内核事件与回合结局（docs/specs/rust-cli/README.md §7）；从 `session_api.rs` 拆出（800 行上限，工作线 F131），
//! 经 `session_api` 重新导出，路径不变。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Delivery;
use crate::env::WorkspaceRef;
use crate::ids::{CommandId, InteractionId, MessageId, ToolUseId, TurnId};
use crate::message::{CompactTrigger, Message, Usage};
use crate::model::{BlockDelta, ModelSelection};
use crate::orchestration::InteractionRequest;
use crate::permission::PermissionMode;
use crate::store::SessionMeta;
use crate::tool::{ToolOutput, ToolProgress};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TurnOutcome {
    Success,
    Interrupted,
    /// 工具的 hook 要求停止（[`crate::orchestration::QueryStop::HookStopped`]）：不是失败，前端按成功结束显示；`-p` 的
    /// `terminal_reason` 是 `hook_stopped`（docs/specs/rust-cli/headless.md §3.6）。
    HookStopped,
    /// Stop hook 阻止继续（[`crate::orchestration::QueryStop::StopHookPrevented`]）：同 `HookStopped`，不是失败；`-p` 的
    /// `terminal_reason` 是 `stop_hook_prevented`（headless.md §3.6，工作线 F134）。
    StopHookPrevented,
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    /// 文件所在的机器（远端执行 F98，docs/specs/rust-cli/remote-exec.md §25.4.5：`/rewind` 把额外机器的写成
    /// `dev-a:README.md`）；主 env 不写出。
    #[serde(default, skip_serializing_if = "crate::ids::MachineName::is_primary")]
    pub machine: crate::ids::MachineName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedInput {
    pub command: CommandId,
    pub text: String,
    pub delivery: Delivery,
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
        /// 生效的自动压缩阈值（[`crate::compaction::ContextOccupancy::compact_at`]，compact.md §2.3）。serde 缺省 None。
        #[serde(default)]
        compact_at: Option<u64>,
    },
    /// 上下文快满的提示（docs/specs/rust-cli/compact.md §2.1）：提交带用量的 assistant 与压缩结果之后重算。
    ContextNotice {
        notice: crate::compaction::ContextNotice,
    },
    /// 完成的主回合请求的输出速度（footer 的 TPS，tui-display.md §2.6，F105）：计量提交之后发，不落库。
    OutputSpeed {
        speed: crate::metering::OutputSpeed,
    },
    TasksChanged {
        tasks: Value,
    },
    QueueChanged {
        items: Vec<QueuedInput>,
    },
    /// 后台工作（命令、Monitor、后台子代理）的终态通知进了会话（工作线 F134：`status` 以前是字符串，tui 另写一份解析）。
    BackgroundWork {
        work: String,
        status: crate::env_shell::BackgroundStatus,
    },
    CompactStarted {
        trigger: CompactTrigger,
    },
    CompactDone {
        boundary: MessageId,
    },
    /// 开始了的压缩失败了（docs/specs/rust-cli/compact.md §4.1）：`message` 是给用户的原因。自动压缩失败时回合照常继续。
    CompactFailed {
        trigger: CompactTrigger,
        message: String,
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
    /// 回合结束那一行的数据（tui-display.md §2.7，F118）：`TurnFinished` 之后、下一个回合之前发；只供显示，不落库。
    TurnDuration(crate::turn_duration::TurnDuration),
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
