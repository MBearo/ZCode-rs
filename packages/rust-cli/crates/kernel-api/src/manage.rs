//! 管理命令与显示用的查询（docs/specs/rust-cli/tui-display.md §5，工作线 L28）。
//!
//! 后台任务的事实在执行环境（后台命令的账本）与 agents（后台子代理）里；这里只是给前端看的快照，
//! 前端在相关事件之后重取，不自己维护任务表。

use serde::{Deserialize, Serialize};

use crate::env_shell::BackgroundStatus;

/// 后台任务的种类（`local_bash` / `local_agent`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundKind {
    Shell,
    Agent,
}

/// 会话里的一个后台任务（`/tasks` 的一行、footer 的计数）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundEntry {
    /// 任务 ID：命令是 shell 任务 ID，子代理是 agentId（TaskStop 与 `Command::StopTask` 用它）。
    pub id: String,
    pub kind: BackgroundKind,
    /// 命令原文，或子代理的 description。
    pub description: String,
    pub status: BackgroundStatus,
    /// 命令输出文件的末尾（最多 [`OUTPUT_TAIL_LINES`] 行）；子代理没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// 远端的后台命令所在的机器；后台子代理是它的缺省机器（Agent 的 `machine`）；本机为 None（远端执行 R4，
    /// docs/specs/rust-cli/remote-exec.md §21.7）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// 远端的后台命令所在的机器正在重连：这一条是那台机器上一份的内容（远端执行 F96，remote-exec.md §23.3）；本机与
    /// 连着的机器为 false（serde 缺省 false、不写出）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reconnecting: bool,
}

/// `BackgroundEntry.output` 最多保留的行数。
pub const OUTPUT_TAIL_LINES: usize = 40;

/// MCP 服务器的连接状态（`/mcp` 与 `/status`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerState {
    Pending,
    Connected,
    Failed,
    /// 停用（配置里 `enabled: false`，或 `/mcp disable`，tui-manage.md §6）：不连接。
    Disabled,
}

/// `/mcp` 的控制动作（docs/specs/rust-cli/tui-manage.md §6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAction {
    Reconnect,
    Enable,
    Disable,
}

/// 对这些服务器做一个动作；回执是它们改动后的状态（reconnect 与 enable 等到离开 connecting）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpControl {
    pub action: McpAction,
    pub servers: Vec<String>,
}

/// 一个 MCP 服务器的状态快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub state: McpServerState,
    /// 连接失败的原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 已连接时提供的工具数。
    #[serde(default)]
    pub tools: usize,
    /// 服务器绑定的远端机器（R5，remote-exec.md §22.2）；本机为 None，不写出。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

/// 一条已配置的 hook（`/hooks` 只读查看，docs/specs/rust-cli/tui-display.md §3.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEntry {
    /// 事件名（33 个 hook 事件名之一，例如 `PreToolUse`）。
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// `command`、`prompt`、`agent` 或 `http`。
    pub kind: String,
    /// 显示的文本：命令、提示词或地址。
    pub display: String,
    /// 来源：`user`、`project`、`local`、`policy`，插件是 `plugin:<名字>`。
    pub source: String,
}

/// `/cd <path>`（docs/specs/rust-cli/tui-manage.md §7）：`path` 是用户给的路径（相对会话 shell 的当前目录）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeDirectory {
    pub workspace: crate::env::WorkspaceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<crate::ids::SessionId>,
    pub path: String,
}

/// `/cd` 成功：新的工作区根与显示路径（home 写成 `~`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryChanged {
    pub root: crate::env::EnvPath,
    pub display: String,
}

/// `Command::ChangeDirectory` 的结果（`Ack::Accepted.result`，tui-manage.md §7.1，F1）：会话 actor 按自己的工作目录
/// 判定。给用户的文字由调用方按结果组成（显示路径要 home）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DirectoryOutcome {
    /// 换好了，说明已提交。
    Moved,
    /// 已经在这个目录（提示 `Already in <dir>.`）。
    Unchanged,
    /// 执行环境换不过去（目录不在了等）；`current` 是仍然在用的工作区根。
    Failed { current: crate::env::EnvPath },
}

/// `/plan open` 的计划文件查询（docs/specs/rust-cli/tui-manage.md §8）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFileQuery {
    pub workspace: crate::env::WorkspaceRef,
    pub session: crate::ids::SessionId,
}

/// 会话的计划文件（与 ExitPlanMode、计划模式 attachment 同一个位置）；`exists` 为真表示文件存在且不为空。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFile {
    pub path: String,
    pub exists: bool,
}

/// `/export <文件名>` 的写入（docs/specs/rust-cli/tui-manage.md §3）：`text` 是前端渲染好的会话文字；`path`
/// 是用户给的路径（相对工作区根、`~/` 开头、或绝对路径；没有扩展名时内核补 `.txt`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportFile {
    pub workspace: crate::env::WorkspaceRef,
    pub path: String,
    pub text: String,
}

#[cfg(test)]
#[path = "manage_tests.rs"]
mod tests;
