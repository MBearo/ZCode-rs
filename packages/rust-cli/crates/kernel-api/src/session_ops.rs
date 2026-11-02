//! 工具与主循环对会话的读与命令（工作线 F134，审查 K2 / C3），经 `orchestration` 重新导出。
//!
//! 以前会话状态的读取散在两个 trait 的 8 个方法里（`RunSink` 的 `current_mode`、`current_mode_and_plan_flags`、
//! `current_workspace`、`current_goal`，`Interactions` 的 `current_selection` 等）：主循环每一步绕会话 actor 3–4 次，
//! 读到的可能是不同时刻的值；`Interactions` 的 10 个方法里混着询问与会话命令。现在询问只在 `Interactions`
//! （`ask`、`cancel_ask`、`lasting`），会话的读与命令在这里，读是一次往返的 [`RunView`]。
//!
//! 方法都没有缺省体：包在会话外面的一层（子代理的输出端）漏转发一个方法是编译错误。测试替身用
//! `testing::OpsOverride`（全委托给 `testing::NoSession`，只覆盖关心的方法）。

use async_trait::async_trait;

use crate::env::{EnvPath, WorkspaceRef};
use crate::ids::SessionId;
use crate::machine::CommandEvents;
use crate::message::Message;
use crate::model::ModelSelection;
use crate::monitor::MonitorArmed;
use crate::orchestration::OrchestrationError;
use crate::permission::PermissionMode;
use crate::plan::PlanModeFlags;

/// 会话在一个时刻的状态：一次往返读出（同一时刻的值，docs/specs/rust-cli/query.md §1、context.md §13.4）。
/// 主循环每一步与每次工具调用前读一次，工具改过的模式与工作目录在同一回合里立即生效（tools-more.md §2.3、§11.4）。
#[derive(Debug, Clone, PartialEq)]
pub struct RunView {
    /// 判决用的权限模式（plan 开时是 plan）。
    pub mode: PermissionMode,
    /// 计划模式的会话标志（与 `mode` 同一个快照）。
    pub plan_flags: PlanModeFlags,
    pub workspace: WorkspaceRef,
    /// 会话现在的主模型（工具与 hook 的旁路请求由它推小模型，model.md §6.1）；没有会话的主模型时为 None
    /// （后台子代理），调用方用自己的回退值。
    pub selection: Option<ModelSelection>,
    /// 会话现在的目标条件（`/goal`，goal.md §4）；子代理没有。
    pub goal: Option<String>,
}

impl RunView {
    /// 这个模式与工作区的视图：计划模式的标志是缺省，没有会话的主模型与目标。会话 actor 与子代理用结构更新补上自己有的
    /// 字段（工作线 F137）。
    pub fn new(mode: PermissionMode, workspace: WorkspaceRef) -> Self {
        Self {
            mode,
            plan_flags: PlanModeFlags::default(),
            workspace,
            selection: None,
            goal: None,
        }
    }
}

/// 会话的读与命令。会话 actor 实现（经回合的提交通道，状态的唯一 owner）；子代理的输出端（agents）按前台、后台
/// 转给父会话或自己处理；不持有会话的输出端（压缩的摘要请求）一律没有。
#[async_trait]
pub trait SessionOps: Send + Sync {
    /// 会话现在的状态；输出端不持有会话的状态时为 None，主循环用请求里的值。
    async fn view(&self) -> Option<RunView>;

    /// 接收交互与排队输入的顶层会话（tools-more.md §10.2）：前台子代理转给父会话，其余是 `session` 自己。定时任务与
    /// worktree 的记录按它。
    async fn owner_session(&self, session: &SessionId) -> SessionId;

    /// 会话已提交的消息（这次回复之前的历史）：fork 子代理继承它（修复线 L-A，agents.md §8.1.2）。不持有会话对话
    /// 的输出端（子代理）为 None。
    async fn conversation(&self) -> Option<Vec<Message>>;

    /// 工具请求打开或关掉 plan 开关（EnterPlanMode、ExitPlanMode，修复线 F20），权限模式不变。owner 是会话 actor：
    /// 先提交事实再通知前端，返回后新状态已生效（tools-more.md §2.3）。不持有会话状态的输出端返回 `Unsupported`。
    async fn set_plan(&self, enabled: bool) -> Result<(), OrchestrationError>;

    /// 改会话的工作目录（EnterWorktree / ExitWorktree）；owner 是会话 actor，它同时设执行环境的 shell cwd
    /// （tools-more.md §11.4）。前台子代理改的是父会话的；不持有会话的返回 `Unsupported`。
    async fn set_workspace(&self, root: EnvPath) -> Result<(), OrchestrationError>;

    /// Monitor 工具起好的 monitor 交给它的 owner（工作线 F83，tools-more.md §19.6）：主线程是会话 actor，子代理里是
    /// agents。`events` 是 `start_command` 返回、工具读过 `Started` 的那条流。不持有的返回 `Unsupported`，调用方停掉
    /// 已经起的命令。
    async fn arm_monitor(
        &self,
        monitor: MonitorArmed,
        events: CommandEvents,
    ) -> Result<(), OrchestrationError>;
}
