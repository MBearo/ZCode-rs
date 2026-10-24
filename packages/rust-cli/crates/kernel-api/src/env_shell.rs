//! shell 执行的 DTO（docs/specs/rust-cli/shell-permissions.md §3）。
//!
//! Bash 工具经 [`crate::machine::Commands::start_command`] 起命令，这里是它需要的附加类型：
//! - 选哪种 shell（[`ShellKind`]）、命令的任务 ID（[`ShellTaskId`]，也是输出文件名）；
//! - cwd 是否在调用之间延续（[`CwdScope`]），延续时执行侧按会话保存 shell cwd（[`ShellState`]）；
//! - sandbox 的读写与网络规则（[`SandboxPolicy`]），由执行环境翻译成平台后端的参数；
//! - 后台命令（[`BackgroundCommand`]）的终态是至少一次送达的通知（[`ShellNotice`]），内核持久化后
//!   回 ack（README §6 规则 6、9）。
//!
//! 这些都是可序列化的 DTO：上云后经网络传给远端执行环境，形状不变。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::env::{EnvPath, Outcome, WorkspaceRef};
use crate::ids::{OperationId, SessionId, ShellTaskId, ToolUseId};
use crate::machine::Shell;

/// 会话的当前目录（docs/specs/rust-cli/tools-files.md §3.3）。唯一来源是执行环境
/// 里这个会话的 shell cwd（[`ShellState::cwd`]：Bash `cd` 改它，越出工作目录时执行侧改回起点；EnterWorktree /
/// ExitWorktree / `/cd` 由会话 actor 经 `set_shell_cwd` 设置）；执行环境答不出时是工作区根。文件工具的相对路径、
/// 权限判定、nested memory 与 readFileState 的冷恢复都经这里取，不各存一份（修复线 F19）。
pub async fn current_dir(
    shell: &dyn Shell,
    workspace: &WorkspaceRef,
    session: &SessionId,
) -> EnvPath {
    shell
        .shell_state(workspace, session)
        .await
        .map_or_else(|_| workspace.root.clone(), |state| state.cwd)
}

/// 跑命令的 shell。Bash 工具用 POSIX shell（Unix 上 bash，Windows 上 Git Bash）；PowerShell 工具
/// 用 PowerShell（docs/specs/rust-cli/shell-permissions.md §1.6）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    #[default]
    Bash,
    PowerShell,
}

/// 命令结束后 cwd 的去留（主线程延续，子代理与远端固定）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CwdScope {
    /// 在 `StartCommand::cwd` 启动，结束后的目录不保留。
    #[default]
    Pinned,
    /// 在会话的 shell cwd 启动（还没有时用 `StartCommand::cwd`）。结束后的目录在 `roots` 之一里时
    /// 保留给下一次调用；否则回到 `StartCommand::cwd`，并在 `Finished::cwd_reset` 里报告。
    Session { roots: Vec<EnvPath> },
}

/// 会话的 shell 状态（read）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellState {
    /// 下一条命令的起始目录。
    pub cwd: EnvPath,
    /// 这个会话的命令输出所在的目录：模型用 Read 读输出文件时不必询问。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<EnvPath>,
    /// 还在跑、或已结束但终态通知还没有 ack 的后台命令。它与 ack 同源：内核据此判断"没有后台工作"
    /// 时，不会漏掉已结束、通知还在路上的命令（docs/specs/rust-cli/query.md §5）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background: Vec<ShellTaskId>,
}

/// 后台命令：`start_command` 立即返回，命令跑完后执行侧发一条 [`ShellNotice`]。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundCommand {
    /// 通知里的命令说明（Bash 输入的 `description`，没有时是命令本身）。
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use: Option<ToolUseId>,
    /// 后台命令的种类（工作线 F83 新增，缺省 Bash）。
    #[serde(default, skip_serializing_if = "BackgroundKind::is_bash")]
    pub kind: BackgroundKind,
}

/// 后台命令的种类（工作线 F83 新增，docs/specs/rust-cli/tools-more.md §19.5）。
///
/// `Monitor`：stdout 是事件流（写进输出文件，按游标读），stderr 写进旁边的 `<任务>.stderr`、不进事件流；
/// 终态通知同样记，带上种类（会话 actor 据此写 monitor 的终态文字）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundKind {
    #[default]
    Bash,
    Monitor,
}

impl BackgroundKind {
    pub fn is_bash(&self) -> bool {
        *self == Self::Bash
    }
}

/// Monitor 的 WebSocket 来源（工作线 F83 新增，tools-more.md §19.7）：[`crate::env::StartCommand::socket`] 有值时
/// 执行环境开这个 WebSocket，不跑命令。每个文本帧加换行写进输出文件，二进制帧写成 `[binary frame, <N> bytes]`；
/// 生命周期按行写成 [`SocketEvent`] 的 JSON，进 `<任务>.stderr`（事件流里是 `Stderr` 的输出）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorSocket {
    /// `ws://` 或 `wss://`。
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocols: Vec<String>,
}

/// WebSocket 的生命周期（工作线 F83 新增）：执行环境按行写成 JSON，会话 actor 按它送说明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SocketEvent {
    /// 出错（连不上、读写失败）；之后还有 `Closed`。
    Error { message: String },
    /// 升级请求的响应不是 101。
    Rejected { status: u16 },
    /// 一帧超过上限（字节数），随即关闭。
    Dropped { bytes: u64 },
    /// 连接关闭：close 帧的代码与原因（没有 close 帧时是 1006 与错误说明）。
    Closed {
        code: u16,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reason: String,
    },
}

/// 前台命令到了超时不结束，转到后台继续跑（docs/specs/rust-cli/shell-permissions.md §1.2.1，工作线 F69 新增）。
/// 转后台时执行侧把命令登记成后台命令（任务 ID 与输出文件不变），发 [`crate::env::CommandEvent::Backgrounded`]，
/// 之后照后台命令处理：结束时记 [`ShellNotice`]，不写回会话的当前目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeoutBackground {
    /// 通知里的命令说明与工具调用（同后台命令）。
    pub command: BackgroundCommand,
    /// 转到后台之后的期限（从转后台时算起）；None 时一直跑到结束或被停止（交互会话）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

/// sandbox 规则（`sandbox` 设置换算后的结果）。执行环境按平台翻译：macOS 用
/// `sandbox-exec` 的 profile，Linux 用 bwrap 的参数；没有后端的平台按 [`SandboxPolicy::required`]
/// 决定拒绝执行还是不套 sandbox 直接执行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPolicy {
    pub filesystem: FilesystemPolicy,
    pub network: NetworkPolicy,
    /// 没有可用后端时拒绝执行（设置 `sandbox.failIfUnavailable`）。
    #[serde(default)]
    pub required: bool,
}

/// 文件系统规则：默认可读、不可写。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemPolicy {
    /// 可写的目录（含子路径）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_write: Vec<EnvPath>,
    /// 可写目录里仍然禁止写的路径。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_write: Vec<EnvPath>,
    /// 禁止读的路径。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_read: Vec<EnvPath>,
}

/// 网络规则。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// 不限制。
    #[default]
    Unrestricted,
    /// 只能连本机（包括 `allow_local_binding` 时的监听）。
    LocalOnly {
        #[serde(default)]
        allow_local_binding: bool,
    },
    /// 按域名放行。
    Domains {
        allowed: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        denied: Vec<String>,
    },
}

/// hook 命令的输入输出（docs/specs/rust-cli/hooks.md §5.1）。有值时：`stdin` 写进标准输入后关闭
/// （没有时标准输入是 /dev/null）；stdout 与 stderr 分开，stderr 在终态之前作为
/// [`crate::env::OutputStream::Stderr`] 的输出事件送达；`env` 加在白名单环境之上。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookIo {
    pub stdin: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// 后台命令的终态通知。`seq` 在会话内单调递增；内核把通知写进会话后调用
/// [`crate::machine::Shell::ack_shell_notice`]，没有 ack 的通知在重连后再次送达。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellNotice {
    pub seq: u64,
    pub operation: OperationId,
    pub task: ShellTaskId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use: Option<ToolUseId>,
    pub description: String,
    /// 输出文件（模型可以用 Read 读它）。
    pub output: EnvPath,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// 后台命令的种类（工作线 F83 新增，缺省 Bash）。
    #[serde(default, skip_serializing_if = "BackgroundKind::is_bash")]
    pub kind: BackgroundKind,
    /// 输出文件的字节数（工作线 F83 新增：monitor 的终态文字分“有没有输出”）。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output_bytes: u64,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde 的 skip_serializing_if 按引用传字段"
)]
fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// 会话里的一条后台命令（TaskStop 按它查与停，docs/specs/rust-cli/shell-permissions.md §2.6）。
/// 只找这个会话自己的命令。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundTask {
    pub session: SessionId,
    pub task: ShellTaskId,
}

/// 后台命令的状态：在跑，或已结束、通知还没 ack 时的终态（与通知的 `<status>` 同一规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

impl BackgroundStatus {
    /// 终态的状态：退出码 0 是 completed；其余正常结束或失败是 failed；取消、中断等是 killed。
    pub fn ended(outcome: &Outcome, exit_code: Option<i32>) -> Self {
        match (outcome, exit_code) {
            (Outcome::Completed, Some(0)) => Self::Completed,
            (Outcome::Completed | Outcome::Failed { .. }, _) => Self::Failed,
            _ => Self::Killed,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Killed => "killed",
        }
    }
}

/// 后台命令的登记（read）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundInfo {
    /// 命令原文（TaskStop 结果的 `command`）。
    pub command: String,
    pub status: BackgroundStatus,
}

/// 停止后台命令的结果（mutate）。终态不在这里：照常经 `watch_command` / `operation_status` 给出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackgroundStop {
    /// 已发出停止；这条命令不会再有终态通知，也不再算后台工作。
    Stopped { command: String },
    /// 已经结束、通知还没 ack。
    NotRunning { status: BackgroundStatus },
    /// 不认识、已被停止或通知已 ack。
    NotFound,
}

#[cfg(test)]
#[path = "env_shell_tests.rs"]
mod tests;
