//! TUI 的会话视图模型（docs/specs/rust-cli/tui.md §4）：把 [`SessionView`] 与 [`KernelEvent`] 折叠成
//! 有序的行。视图是内核事实的投影，随时可以丢弃后从 `view()` 重建；这里不保存内核以外的会话事实。
//!
//! 行带视图内唯一的 `id` 与每次变化加一的 `version`，排版缓存按这两个值命中。
mod extras;
mod failure;
mod fire;
mod fold;
pub mod in_flight;
mod notice;
mod tool_kind;

pub use extras::{Activity, AgentActivity, TurnChanges};
pub use failure::Failure;
pub use notice::Notice;
pub use tool_kind::{BASH, ToolKind};

use std::collections::BTreeMap;

use rust_cli_kernel_api::compaction::{ContextNotice, ContextOccupancy};
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::{InteractionId, SessionId, ToolUseId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::{ExecutionState, PermissionMode};
use rust_cli_kernel_api::session_api::{Delivery, NoticeLevel, QueuedInput};
use serde_json::Value;

/// 回合的状态（回合行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    Running,
    Success,
    Interrupted,
    Failed,
}

/// 工具调用的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// 模型已给出调用，还没开始执行。
    Pending,
    /// 等待用户审批。
    Approval,
    Running,
    Success,
    Error,
    /// 回合结束时还没有结果（中断）。
    Cancelled,
}

impl ToolStatus {
    pub fn is_live(self) -> bool {
        matches!(self, Self::Pending | Self::Approval | Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolRow {
    pub id: ToolUseId,
    pub name: String,
    /// 按名字认出的种类（建行时算一次）。
    pub kind: ToolKind,
    pub input: Value,
    pub status: ToolStatus,
    /// 结果的文字（tool_result 的 text 内容拼接）。
    pub output: String,
    /// 运行中的输出预览（Bash 的 `bash_progress`）。
    pub preview: String,
    /// 工具给 UI 的展示数据（`ToolOutput::display`，例如 Write 的 `type`）。
    pub display: Value,
    /// 前台子代理的活动（Agent 工具的进度，tui-display.md §2.1）；其他工具为 None。
    pub activity: Option<AgentActivity>,
    /// 调用所在的模型回答（Puffin 投影的 `response`：同一条 assistant 消息或同一段流式响应的调用相同；一批调用的名册
    /// 按它分，docs/puffin-tui.md §4.2「一批调用」）；不知道时为空。
    pub response: String,
    /// 开始执行（`ToolStarted`）与结束的时刻（毫秒，调用方的时钟）；没开始、没结束时为 0。行尾的用时与运行中的
    /// `· 12s` 按它们算（Puffin 的 `started_at` / `ended_at`）。
    pub started_ms: u64,
    pub ended_ms: u64,
}

impl ToolRow {
    /// 新的工具行：还没有输出；种类按名字认一次。
    pub fn new(id: ToolUseId, name: String, input: Value, status: ToolStatus) -> Self {
        Self {
            id,
            kind: ToolKind::of(&name),
            name,
            input,
            status,
            output: String::new(),
            preview: String::new(),
            display: Value::Null,
            activity: None,
            response: String::new(),
            started_ms: 0,
            ended_ms: 0,
        }
    }
}

/// 一次压缩（Puffin 投影的 `CompactItem`）：状态与压缩前后的 token（知道时）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compact {
    pub status: rust_cli_tui_kit::app::widgets::entries::Compaction,
    pub before: Option<u64>,
    pub after: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    /// 回合头：不显示，用来分组与判断运行状态。
    Turn(TurnState),
    User {
        text: String,
    },
    Agent {
        text: String,
        streaming: bool,
    },
    Reasoning {
        text: String,
        streaming: bool,
        /// 推理用时；流式中为空，排版时按当前时间算。
        duration_ms: Option<u64>,
    },
    Tool(Box<ToolRow>),
    /// 压缩：一行，开始时在跑，压缩后的第一次上下文占用到达时成功（docs/puffin-tui.md §4.2「命令的结果」的 `/compact`）。
    Compact(Compact),
    /// 回合失败（docs/puffin-tui.md §4.2「回合失败」）：原因与它的事实（[`Failure`]）。
    Failure(Box<Failure>),
    /// 本地命令的输出（`user-invocable: false` 的提示、展开失败的 stderr；dim 的 `⎿` 行，ext.md §2.7）。
    Output(String),
    /// 定时任务的触发行（`Running scheduled task`、`ZCode resuming /loop wakeup`，tools-more.md §18.3）。
    Fire(Box<Fire>),
    /// 只供显示的系统提示（`SystemNotice`，L18：插话被 UserPromptSubmit 阻塞）。
    Notice {
        level: NoticeLevel,
        text: String,
    },
    /// bash 模式的输入（`<bash-input>`，tui-input.md §2.1）。
    Shell {
        command: String,
    },
    /// bash 模式的输出（`<bash-stdout>` / `<bash-stderr>`，已转义的文字还原）。
    ShellOutput {
        stdout: String,
        stderr: String,
    },
    /// `/goal` 的评估结果（`goal_status`，不含设定 / 清除的标记，goal.md §6.1）。
    Goal(Box<rust_cli_kernel_api::goal::GoalStatus>),
    /// 跨会话：并入或开回合的同伴消息与发送方收到的回执（docs/specs/rust-cli/cross-session.md §2.6）。
    Peer(Box<PeerRow>),
    /// 后台子代理的结果回到了主会话（`<task-notification>`，内核开的回合或工具边界并入的）：`• Subagent 描述 finished`
    /// （Puffin 的通知，`NoticeSource::Subagent`）。标题是它的 Agent 调用的 `description`（没有时 `subagent_type`）。
    Returned {
        title: String,
    },
    /// fork 出的会话的痕迹行（Puffin 的 `Item::Fork`，`host/fold.rs` 的 `load`）：会话的 `meta.parent` 有、又不是子代理时，
    /// 打开它的视图是第一行，所以 `/resume` 之后也在。`title` 是离开的会话的标题：这次运行里从那个会话挑点回去的才知道
    /// （`App::rewound_into` 写进来，Puffin `titles`），否则 None，只写 `Rewound here`。
    Forked {
        title: Option<String>,
    },
    /// 回合结束那一行（`TurnDuration`，修复线 F118，tui-display.md §2.7）：只在实时事件里，冷加载没有（不落库）。
    Duration(Box<rust_cli_kernel_api::turn_duration::TurnDuration>),
}

/// 跨会话的一行（同伴消息与回执的警告行）。
#[derive(Debug, Clone, PartialEq)]
pub enum PeerRow {
    /// 同伴消息：发送方的名字（信封的 `from-name`，没有时是 socket 文件名或 `peer`）与正文。
    Message { name: String, body: String },
    /// 回执：`held` / `denied` / `expired` / `delivered` / `refused`。
    Receipt { status: String },
}

/// 一次触发（内核事件 `ScheduledFire`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Fire {
    pub at: String,
    pub kind: rust_cli_kernel_api::cron::CronFireKind,
    pub fold: Option<rust_cli_kernel_api::cron::LoopFold>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: u64,
    pub version: u64,
    /// 行出现的时间（毫秒，调用方的时钟）。
    pub created_ms: u64,
    pub body: Body,
}

impl Row {
    pub fn tool(&self) -> Option<&ToolRow> {
        match &self.body {
            Body::Tool(tool) => Some(tool),
            _ => None,
        }
    }

    /// 还在变化的行（流式中的回复与推理、运行中的工具、运行中的回合）。
    pub fn live(&self) -> bool {
        match &self.body {
            Body::Turn(state) => *state == TurnState::Running,
            Body::Agent { streaming, .. } | Body::Reasoning { streaming, .. } => *streaming,
            Body::Tool(tool) => tool.status.is_live(),
            Body::Compact(compact) => {
                compact.status == rust_cli_tui_kit::app::widgets::entries::Compaction::Running
            }
            Body::User { .. }
            | Body::Failure(_)
            | Body::Output(_)
            | Body::Fire(_)
            | Body::Notice { .. }
            | Body::Shell { .. }
            | Body::ShellOutput { .. }
            | Body::Goal(_)
            | Body::Peer(_)
            | Body::Returned { .. }
            | Body::Forked { .. }
            | Body::Duration(_) => false,
        }
    }
}

/// 响应段：一次模型响应的流式行从 `start` 开始；`blocks` 是内容块序号 → 行下标。
#[derive(Debug, Clone, Default)]
struct Segment {
    /// 这一段的调用共有的 `response`（[`ToolRow::response`]）：流式的段按序号，没有流的消息是消息 id。
    key: String,
    response: Option<u32>,
    start: usize,
    blocks: Vec<(usize, usize)>,
}

/// 一个子代理的数字（Puffin `SubagentUsage`）：它的子会话完成的请求的 token（输入加输出）、最近一次的速度与全部量到的
/// 请求的平均速度，都出自 `request_stats(子会话)`（usage.md §4.1）。
///
/// 缺陷修复（2026-11-02）：以前只填 `last`，`tokens` 与 `avg` 从来没有值，在跑的 Agent 行没有 token、结束的行没有平均
/// 速度；原因是请求统计只有最近一次请求，说不出和。现在存储给出和，三个数同一次查询。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SubagentFigures {
    pub tokens: Option<u64>,
    pub last: Option<f64>,
    pub avg: Option<f64>,
}

impl SubagentFigures {
    pub fn of(stats: &rust_cli_kernel_api::metering::RequestStats) -> Self {
        Self {
            tokens: stats.tokens(),
            last: stats.tokens_per_second(),
            avg: stats.average_tps(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Transcript {
    rows: Vec<Row>,
    next_id: u64,
    pending: Vec<(InteractionId, InteractionRequest)>,
    /// 扣住、等批准的同伴消息（快照的 `peer_held` 与 `PeerHeldChanged`，cross-session.md §2.6）。
    peer_held: Vec<rust_cli_kernel_api::peers::HeldPeerMessage>,
    queue: Vec<QueuedInput>,
    selection: ModelSelection,
    /// 执行状态：权限模式 × plan 开关（修复线 F20；`ModeChanged` / `PlanChanged` 各改一半）。
    execution: ExecutionState,
    /// 会话的工作区（快照的 `meta.workspace` 与 `WorkspaceChanged`）；没有会话时是新会话的草稿。前端的工作区只有
    /// 这一个来源（R1-3）。
    workspace: WorkspaceRef,
    title: String,
    /// 内核给出的“上下文快满”提示（`ContextNotice` 事件与快照，compact.md §2.1）。
    context: ContextNotice,
    /// 内核给出的上下文用量（`ContextUsage` 事件与快照的 `context_usage`）：footer 的 ctx（tui-display.md §2.6，F27）。
    occupancy: Option<ContextOccupancy>,
    /// 内核给出的最近一个完成的主回合请求的输出速度（`OutputSpeed` 事件与快照的 `output_speed`）：footer 的 TPS
    /// （tui-display.md §2.6，修复线 F105）。
    speed: Option<rust_cli_kernel_api::metering::OutputSpeed>,
    last_seq: u64,
    segment: Option<Segment>,
    /// 本回合流出的回复与推理字数（状态行的 `↓ N tokens`）。
    streamed_chars: usize,
    /// 状态栏现场速度要的字数（Puffin 投影的 `usage.live`，kit `live_speed::Heard`）：最新回合里模型写下的字（回答、思考与
    /// 工具的输入）与其中的宽字，回合开始时清零；`last_call` 是最近一次结束的模型调用（消息的 id 与它的输出 token）。
    live_chars: u64,
    live_wide: u64,
    last_call: Option<(String, u64)>,
    /// 任务清单、每回合改动、用量（L28，`extras.rs`）。
    extras: extras::Extras,
    /// 主线程还在生效的重试或停顿通知（L35，`notice.rs`）。
    notice: Option<Notice>,
    /// 流式响应段的序号（[`Segment::key`]）。
    segments: u64,
    /// 这个回合里重试过最多的一次与它的错误（失败的块写试过几次，Puffin `turn.retried`）。
    retried: Option<(u32, rust_cli_kernel_api::model::ModelError)>,
    /// 这个回合的第一条你的消息（失败的块 ⏎ 再发它）。
    turn_user: Option<String>,
    /// 子代理：派生它的 Agent 调用 → 它的子会话（Agent 工具结果的 display，fold.rs 的 `ToolDone`）。
    subagents: BTreeMap<String, SessionId>,
    /// runner 为每个子会话查来的数字（Puffin `subagents`）。按子会话记：任务卡的 Subagents 的行（含子代理派生的子代理，
    /// 主会话里没有它们的 Agent 调用）与 Agent 行读同一份（tui-panels.md §1.3）。
    figures: BTreeMap<SessionId, SubagentFigures>,
    /// 主会话已经收到结果的后台工作（`<task-notification>` 的 `<task-id>`，内核开的回合或工具边界并入的）：输入框上方
    /// 后台子代理的坞里结束了的行到这时离开（tui-panels.md §1.8，Puffin 的运行等到它的结果通知）。
    reported: std::collections::BTreeSet<String>,
    /// 这条消息里刚收到结果的后台子代理的标题：活的事件（`MessageDone`）把它们写成 [`Body::Returned`]；冷加载与追加的
    /// 快照丢掉（Puffin 只在活的事件里推这条通知，`/resume` 之后没有）。
    returned: Vec<String>,
    /// 在跑的压缩的行 id，与它做完了没有（之后的第一次上下文占用给出压缩后的 token，Puffin `compact`）。
    compact: Option<(u64, bool)>,
    /// 冷加载的行（快照里已有的）的 id 都小于它：它们照自己的样子画，没有出现的动效、不重放文字（Puffin 的快照）。
    cold: u64,
    /// ⏎ 发出、内核还没写下 `<bash-input>` 的 `!` 命令（Puffin `state.shells`）：画在 transcript 的末尾，成为在跑的块。
    /// 内核在命令跑完之后才写下输入与输出，以前块要等输出到了才出现（twin shell-bang 第 24 步）。
    shells: Vec<String>,
    /// 内核写下了 `<bash-input>`、还没写下输出：命令还在跑（Puffin 的块到输出到了才 final）。回合结束时清掉。
    shell_open: bool,
    /// 发出了、内核还没认领的输入（`in_flight.rs`，Puffin `outgoing`）：在折叠里与 `queue` 一起更新。
    in_flight: in_flight::InFlight,
}

impl Transcript {
    /// 还没有会话时的空视图（模型、执行状态与工作区是新会话的草稿）。单值的 `Plan` 是“plan 开、default”。
    pub fn new(
        selection: ModelSelection,
        execution: impl Into<ExecutionState>,
        workspace: WorkspaceRef,
    ) -> Self {
        Self {
            rows: Vec::new(),
            next_id: 1,
            pending: Vec::new(),
            peer_held: Vec::new(),
            queue: Vec::new(),
            selection,
            execution: execution.into(),
            workspace,
            title: String::new(),
            context: ContextNotice::Hidden,
            occupancy: None,
            speed: None,
            last_seq: 0,
            segment: None,
            streamed_chars: 0,
            live_chars: 0,
            live_wide: 0,
            last_call: None,
            extras: extras::Extras::default(),
            notice: None,
            segments: 0,
            retried: None,
            turn_user: None,
            subagents: BTreeMap::new(),
            figures: BTreeMap::new(),
            reported: std::collections::BTreeSet::new(),
            returned: Vec::new(),
            compact: None,
            cold: 0,
            shells: Vec::new(),
            shell_open: false,
            in_flight: in_flight::InFlight::default(),
        }
    }

    /// 痕迹行说出离开的会话的标题（挑选回退的点之后新会话的视图到了时，`App::rewound_into`）。
    pub fn name_fork(&mut self, title: Option<String>) {
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| matches!(row.body, Body::Forked { .. }))
        {
            row.body = Body::Forked { title };
            row.version += 1;
        }
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// 在跑的回合（它的回合头的行 id）；没有时为 None。
    pub fn running_turn(&self) -> Option<u64> {
        self.rows
            .iter()
            .rev()
            .find(|row| matches!(row.body, Body::Turn(_)))
            .filter(|row| row.body == Body::Turn(TurnState::Running))
            .map(|row| row.id)
    }

    /// Agent 调用 `tool_use` 的子代理的数字；还不知道时为 None。
    pub fn figures(&self, tool_use: &str) -> Option<SubagentFigures> {
        self.child_figures(self.subagents.get(tool_use)?)
    }

    /// 子会话 `child` 的数字；还不知道时为 None。
    pub fn child_figures(&self, child: &SessionId) -> Option<SubagentFigures> {
        self.figures.get(child).copied()
    }

    /// 派生子会话 `child` 的 Agent 调用（主会话里有它时）。
    pub fn spawned_by(&self, child: &SessionId) -> Option<&str> {
        self.subagents
            .iter()
            .find_map(|(item, c)| (c == child).then_some(item.as_str()))
    }

    /// 主会话收到了后台工作 `id`（子代理的 agent id、后台命令的 id）的结果（`<task-notification>`）。
    pub fn reported(&self, id: &str) -> bool {
        self.reported.contains(id)
    }

    /// 主会话里 Agent 调用派生的子会话（runner 按它们查数字）。
    pub fn subagent_children(&self) -> impl Iterator<Item = &SessionId> {
        self.subagents.values()
    }

    /// 查来的子会话 `child` 的数字（Puffin `Fold::set_subagent`）。
    pub fn set_figures(&mut self, child: SessionId, figures: SubagentFigures) {
        self.figures.insert(child, figures);
    }

    /// 最近的回合（它的回合头的行 id，在跑或已经结束）；还没有回合时为 None。
    pub fn latest_turn(&self) -> Option<u64> {
        self.rows
            .iter()
            .rev()
            .find(|row| matches!(row.body, Body::Turn(_)))
            .map(|row| row.id)
    }

    /// 冷加载的行的 id 都小于它（[`Self::from_view`]、[`Self::append_view`]）。
    pub fn cold_until(&self) -> u64 {
        self.cold
    }

    pub fn selection(&self) -> &ModelSelection {
        &self.selection
    }

    /// 权限模式（不会是 plan；plan 看 [`Self::plan`]）。
    pub fn mode(&self) -> PermissionMode {
        self.execution.mode
    }

    /// plan 开关（修复线 F20）。
    pub fn plan(&self) -> bool {
        self.execution.plan
    }

    pub fn execution(&self) -> ExecutionState {
        self.execution
    }

    /// 没有会话时改新会话的草稿；有会话时以内核事件为准，调用方不用它。
    pub fn set_draft(&mut self, selection: ModelSelection, execution: ExecutionState) {
        self.selection = selection;
        self.execution = execution;
    }

    /// 会话的工作区；没有会话时是新会话的草稿。
    pub fn workspace(&self) -> &WorkspaceRef {
        &self.workspace
    }

    /// 没有会话时 `/cd` 换新会话的工作区草稿；有会话时以 `WorkspaceChanged` 为准，调用方不用它。
    pub fn set_draft_workspace(&mut self, workspace: WorkspaceRef) {
        self.workspace = workspace;
    }

    /// 没有会话时 `/rename 名字` 换新会话的名字草稿（titles.md §4）；有会话时以 `TitleChanged` 为准，调用方不用它。
    pub fn set_draft_title(&mut self, title: String) {
        self.title = title;
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn queue(&self) -> &[QueuedInput] {
        &self.queue
    }

    /// 发出了、内核还没认领的输入（[`in_flight::InFlight`]）。
    pub fn in_flight(&self) -> &in_flight::InFlight {
        &self.in_flight
    }

    pub fn in_flight_mut(&mut self) -> &mut in_flight::InFlight {
        &mut self.in_flight
    }

    /// 栈与尾巴画的排着的消息：内核的队列，后面是还没认领的输入，按内核将要排上它们的样子（tui.md §5.1.2）。回合在跑时
    /// 按各自的投递；空闲时内核用它开回合，画在尾巴里（`Steer` 的位置，Puffin `!after` 的 outgoing）；空闲时的 `!` 命令不在
    /// 这里，它的块已经画在末尾（[`Self::hold_shell`]）。
    pub fn outgoing(&self) -> Vec<QueuedInput> {
        let running = self.running();
        let sent = self.in_flight.items().filter_map(|item| {
            if running {
                return Some(item.clone());
            }
            let shell = rust_cli_kernel_api::user_input::display(&item.text).is_some();
            (!shell).then(|| QueuedInput {
                delivery: Delivery::Steer,
                ..item.clone()
            })
        });
        self.queue.iter().cloned().chain(sent).collect()
    }

    /// 还没写进对话的 `!` 命令（[`Self::hold_shell`]）。
    pub fn held_shells(&self) -> &[String] {
        &self.shells
    }

    /// 有 `!` 命令在跑：⏎ 时画的块还在，或内核写下了输入、输出还没到（Puffin `shell_running`）。
    pub fn shell_running(&self) -> bool {
        !self.shells.is_empty() || self.shell_open
    }

    /// ⏎ 立即运行的 `!` 命令：内核写下它的 `<bash-input>` 之前，块先画在末尾（Puffin `run_shell`）。
    pub fn hold_shell(&mut self, command: &str) {
        self.shells.push(command.to_owned());
    }

    /// 内核没收下这条命令（拒绝、会话没能打开）：不再画它（Puffin 的 `Msg::ShellStarted` 出错时）。
    pub fn drop_shell(&mut self, command: &str) {
        if let Some(at) = self.shells.iter().position(|held| held == command) {
            self.shells.remove(at);
        }
    }

    /// 会话没能打开：留着的命令都不会运行。
    pub fn drop_shells(&mut self) {
        self.shells.clear();
    }

    /// 第一个待处理的交互（同时只显示一个）。
    /// 第一条扣住的同伴消息（批准框显示它，cross-session.md §2.6）。
    pub fn peer_held(&self) -> Option<&rust_cli_kernel_api::peers::HeldPeerMessage> {
        self.peer_held.first()
    }

    /// 扣住的条数。
    pub fn peer_held_count(&self) -> usize {
        self.peer_held.len()
    }

    pub fn pending(&self) -> Option<&(InteractionId, InteractionRequest)> {
        self.pending.first()
    }

    /// 全部待处理交互，按到达顺序（板头的 `N more waiting`、全部允许，tui-kit.md §4.2「请求」）。
    pub fn pending_all(&self) -> &[(InteractionId, InteractionRequest)] {
        &self.pending
    }

    /// 全部扣住的同伴消息，按到达顺序。
    pub fn peer_held_all(&self) -> &[rust_cli_kernel_api::peers::HeldPeerMessage] {
        &self.peer_held
    }

    /// footer 的“上下文快满”提示（docs/specs/rust-cli/compact.md §2.1）：级别与数字由内核的压缩模块
    /// 算好，这里只取最新的一条。
    ///
    /// 缺陷修复（F14）：以前这里按 `(token, 有效窗口)` 自己算，常数写死（不看百分比覆盖），只有 `N% context used`
    /// 一种文字。
    pub fn context_notice(&self) -> ContextNotice {
        self.context
    }

    /// footer 的 ctx（tui-display.md §2.6，F27）：会话 actor 经压缩模块算的 `(token, 有效窗口)`，只取最新的一份；
    /// 没有会话或压缩模块不知道时为空。
    pub fn context_usage(&self) -> Option<ContextOccupancy> {
        self.occupancy
    }

    /// footer 的 TPS（tui-display.md §2.6，修复线 F105）：会话 actor 记的最近一个完成的主回合请求的速度；本进程还没有
    /// 这样的请求（冷打开、切换会话之后）时为空。
    pub fn output_speed(&self) -> Option<rust_cli_kernel_api::metering::OutputSpeed> {
        self.speed
    }

    /// 最后一个回合行：运行状态与开始时间。
    fn last_turn(&self) -> Option<(TurnState, u64)> {
        self.rows.iter().rev().find_map(|row| match row.body {
            Body::Turn(state) => Some((state, row.created_ms)),
            _ => None,
        })
    }

    /// 最后一个回合行的状态；还没有回合时为 None（程序状态的根记录，tui-manage.md §5.4）。
    pub fn turn_state(&self) -> Option<TurnState> {
        self.last_turn().map(|(state, _)| state)
    }

    /// 最后一条失败的说明（程序状态的 `error`，tui-manage.md §5.4）。
    pub fn last_failure(&self) -> Option<&str> {
        self.rows.iter().rev().find_map(|row| match &row.body {
            Body::Failure(failure) => Some(failure.message.as_str()),
            _ => None,
        })
    }

    /// 回合是否在运行（唯一依据：最后一个回合行，docs/specs/rust-cli/tui.md §4.3）。
    pub fn running(&self) -> bool {
        self.last_turn()
            .is_some_and(|(state, _)| state == TurnState::Running)
    }

    /// 运行中回合的开始时间。
    pub fn turn_started_ms(&self) -> Option<u64> {
        self.last_turn()
            .filter(|(state, _)| *state == TurnState::Running)
            .map(|(_, at)| at)
    }

    pub fn streamed_chars(&self) -> usize {
        self.streamed_chars
    }

    /// 状态栏的现场速度听说的（kit `live_speed::Heard`，同 Puffin 投影的 `usage.live` 与 `usage.speed.last`）：回合在模型
    /// 手里是在跑、没有待处理的请求、没有在运行的工具（送进后台的不算，Puffin `tool_running`）。
    pub fn heard(&self) -> rust_cli_tui_kit::app::widgets::live_speed::Heard {
        let tool_running = self.rows.iter().any(|row| match &row.body {
            Body::Tool(tool) => {
                matches!(tool.status, ToolStatus::Running | ToolStatus::Approval)
                    && !tool.input["run_in_background"].as_bool().unwrap_or(false)
            }
            _ => false,
        });
        rust_cli_tui_kit::app::widgets::live_speed::Heard {
            chars: self.live_chars,
            wide: self.live_wide,
            model_turn: self.running() && self.pending_all().is_empty() && !tool_running,
            last_call: self.last_call.clone(),
            speed_last: self.speed.and_then(|s| s.tokens_per_second()),
        }
    }

    /// 模型写下的一段字记进现场速度的字数（kit `live_speed::written`，同 Puffin fold 的 `count_live`）。
    pub(super) fn count_live(&mut self, text: &str) {
        let (chars, wide) = rust_cli_tui_kit::app::widgets::live_speed::written(text);
        self.live_chars += chars;
        self.live_wide += wide;
    }

    /// 是否有过会话内容（用户行）。
    pub fn has_messages(&self) -> bool {
        self.rows
            .iter()
            .any(|row| matches!(row.body, Body::User { .. } | Body::Shell { .. }))
    }
}

#[cfg(test)]
#[path = "fold_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "duration_tests.rs"]
mod duration_tests;

/// 测试用的工作区（`/Users/dev/project`）。
#[cfg(test)]
pub(crate) fn test_workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
        root: rust_cli_kernel_api::env::EnvPath::new("/Users/dev/project"),
    }
}
