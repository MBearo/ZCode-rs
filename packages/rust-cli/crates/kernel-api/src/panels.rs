//! L30 面板的查询（docs/specs/rust-cli/tui-panels.md §5）：输入框下面的子代理列表、← 会话面板与 Shell 详情。
//!
//! 事实都在内核里（agents 的在跑子代理表、runtime 的会话表、执行环境的后台命令账本）；这里只是给前端看的快照。
//! 前端在变化通知或相关事件之后重取，按请求序号丢掉过时的回执，不自己维护这些状态。

use serde::{Deserialize, Serialize};

use crate::env_shell::BackgroundStatus;
use crate::ids::{AgentId, SessionId};
use crate::store::{SessionRecord, SessionSummary};

/// 子代理的运行状态（列表圆点的颜色）。停放的子代理（agents.md §5.1）算 `Completed`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

/// 顶层会话里的一个子代理（输入框下面的列表一行，tui-panels.md §1.1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentEntry {
    pub agent: AgentId,
    /// 子会话（只读查看用它的消息）。
    pub child: SessionId,
    pub agent_type: String,
    pub description: String,
    pub status: SubagentStatus,
    pub background: bool,
    /// 有交互在等用户回答（前台子代理经父会话询问的审批或提问）。
    #[serde(default)]
    pub waiting: bool,
    /// 用时：在跑时从这次运行开始到查询时刻，结束后是开始到结束（毫秒，内核的时钟）。
    pub elapsed_ms: u64,
    /// 结束了多久（查询时刻）；在跑时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ago_ms: Option<u64>,
    /// 子会话的提交次数：变了说明子会话有新消息（查看中的视图据此重取）。
    pub version: u64,
    /// 派生它的子代理；主线程派生的为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<AgentId>,
    /// 正在做什么（只在跑的子代理有；只在内存，不持久化，docs/specs/rust-cli/stall-visibility.md §4.3）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<SubagentActivity>,
    /// 停放（跑完了但还有在跑的后台子代理，keepalive，agents.md §5.1）；`status` 仍是 `Completed`。
    /// 列表据此区分停放与真的结束了（修复线 F30，tui-panels.md §1.2：停放的不驱逐、照常当父代理）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub parked: bool,
}

/// 子代理的当前活动：子循环事件的投影（stall-visibility.md §4.3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubagentActivity {
    /// 推理块在流式输出。
    Thinking,
    /// 工具在执行。
    Tool { name: String },
    /// 模型请求在等待重试；`retry_in_ms` 是查询时刻还要等多久，`error` 是失败的文字。
    Retrying {
        attempt: u32,
        max_retries: u32,
        retry_in_ms: u64,
        error: String,
    },
    /// 流停住了；`retry_in_ms` 是查询时刻离看门狗中止还有多久。
    Stalled { retry_in_ms: u64 },
}

/// 本进程里打开着的会话的实时状态（← 面板，tui-panels.md §2.1）。只在库里的会话没有状态（Inactive）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActivity {
    /// 有待处理的交互（审批、提问、计划审批）。
    NeedsInput,
    /// 有回合在跑，或有后台命令 / 后台子代理。
    Working,
    /// 打开着、空闲。
    Ready,
}

/// 工作区里的一个会话：库里的摘要，加上本进程里打开着时的实时状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSession {
    pub summary: SessionSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<SessionActivity>,
    /// 最近一条回复的文字（打开着的会话才有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reply: Option<String>,
}

/// Shell 详情里输出的最多行数。
pub const SHELL_DETAIL_LINES: usize = 10;

/// 后台命令输出的预览。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShellOutput {
    /// 输出文件末尾最多 [`SHELL_DETAIL_LINES`] 行与文件的总行数。
    Tail { lines: Vec<String>, total: u64 },
    /// 文件太大，不预览（字节数）。
    TooLarge { bytes: u64 },
    /// 读不到（没有输出目录、读取失败）。
    Unavailable,
}

/// 一个后台命令的详情（tui-panels.md §3.3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellDetail {
    pub id: String,
    /// 命令原文。
    pub command: String,
    pub status: BackgroundStatus,
    /// 从开始到现在（只有在跑时有，执行环境记下的开始时刻）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    pub output: ShellOutput,
}

/// 输入框下面的子代理列表与 footer 的后台任务的同一份快照（修复线 F34，tui-panels.md §0、§3.1）：一次查询、一次回执，
/// 前端一次收下、一次重画（两者同一帧出现）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksSnapshot {
    /// 顶层会话里本进程启动过的子代理（含嵌套的），按启动顺序（同 [`crate::session_api::SessionHandle::subagents`]）。
    #[serde(default)]
    pub subagents: Vec<SubagentEntry>,
    /// 还在跑的后台命令与后台子代理（footer 的 pill、Background 面板的 shell）。这份快照里命令不带输出末尾
    /// （`output` 为空：界面不用它，它在 agents 每次变化后都重取）。[`crate::agents::AgentSignals::tasks_snapshot`]
    /// 给出的只有后台子代理。
    #[serde(default)]
    pub background: Vec<crate::manage::BackgroundEntry>,
}

#[cfg(test)]
#[path = "panels_tests.rs"]
mod tests;

/// ← 项目页与 `/resume` 页的一个会话（修复线 F29，tui-panels.md §2.1）：库里的记录加本进程的实时状态与目录的分支。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSession {
    pub record: SessionRecord,
    /// 本进程打开着时的状态；只在库里时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<SessionActivity>,
    /// 打开着的会话最后一条 assistant 的文字。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reply: Option<String>,
    /// 会话目录现在的 git 分支（读 `.git/HEAD`）；分离头、不在仓库里或读不到时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// [`crate::session_api::Kernel::project_sessions`] 的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSessions {
    pub sessions: Vec<ProjectSession>,
    /// 超出查询的上限还有更多会话。
    #[serde(default)]
    pub more: bool,
}
