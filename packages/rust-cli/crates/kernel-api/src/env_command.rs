//! 执行环境端口里跑命令的 DTO（从 `env` 拆出，路径经 `crate::env` 重新导出）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::env::{EnvPath, Outcome};
use crate::env_shell::{
    BackgroundCommand, CwdScope, HookIo, MonitorSocket, SandboxPolicy, ShellKind, TimeoutBackground,
};
use crate::ids::ShellTaskId;

/// 加在子进程环境之后的变量（远端执行 F96，docs/specs/rust-cli/remote-exec.md §23.5）：值为 `None` 时去掉这个变量。
/// 只由执行器的服务端按请求所属的客户端实例填（`env.update` 与 SSH 代理带来的 `hello.session_env`）；工具与本机的组装
/// 都不写，缺省是空的。保护的名字已经在服务端拒绝。
pub type EnvOverrides = BTreeMap<String, Option<String>>;

/// 启动一条 shell 命令。前台、后台都经这里；输出用 [`crate::machine::Commands::watch_command`] 按游标读。
///
/// stdout 与 stderr 写进同一个输出文件（合并的 fd），事件流里都标为 stdout。
/// `task` 之后的字段是 Bash 工具加的（docs/specs/rust-cli/shell-permissions.md §3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartCommand {
    pub command: String,
    pub cwd: EnvPath,
    pub timeout_ms: Option<u64>,
    /// 任务 ID：输出文件名与后台通知都用它。
    pub task: ShellTaskId,
    #[serde(default)]
    pub shell: ShellKind,
    #[serde(default)]
    pub cwd_scope: CwdScope,
    /// 有值时是后台命令：不随本次工具调用结束，终态经 [`crate::machine::Shell::shell_notices`] 送达。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<BackgroundCommand>,
    /// 有值时在 sandbox 里执行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxPolicy>,
    /// 有值时是 hook 命令：输入经 stdin，stdout 与 stderr 分开（docs/specs/rust-cli/hooks.md §5.1）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook: Option<HookIo>,
    /// 有值时前台命令到了 `timeout_ms` 不结束，转到后台继续跑（docs/specs/rust-cli/shell-permissions.md §1.2.1，
    /// 工作线 F69 新增）。没有时照旧按超时结束。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_timeout: Option<TimeoutBackground>,
    /// 有值时不跑 `command`，开这个 WebSocket（Monitor 的 ws 来源，工作线 F83 新增，tools-more.md §19.7）：
    /// 只和后台的 `Monitor` 种类一起用；`command` 记 URL（TaskStop 的结果用它）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket: Option<MonitorSocket>,
    /// 投影之后再加上（或去掉）的环境变量（[`EnvOverrides`]，F96）：只在执行器里由服务端填；不进序列化，所以也不进
    /// env-local 的去重指纹（同一个操作重放时不因它算作参数不同）。
    #[serde(skip)]
    pub env: EnvOverrides,
}

/// 前台命令为什么被转到后台（修复线 L-A，tui-input.md §11）：用户按了转后台的键，或运行中来了一条低延迟投递的输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundReason {
    User,
    DeliverMessage,
}

/// 命令输出事件。`cursor` 是已送达的字节位置，断线后从这里续读。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandEvent {
    /// 第一个事件：命令已登记，输出写到 `output`（执行环境里的路径，Read 能读）。
    Started { output: EnvPath },
    Output {
        stream: OutputStream,
        data: Vec<u8>,
        cursor: u64,
    },
    /// 前台命令到了超时、转到了后台（[`StartCommand::on_timeout`]，工作线 F69 新增）：只发一次，之后照常有输出
    /// 与终态（命令真正结束时）。
    Backgrounded,
    /// 前台命令被要求转到了后台（[`crate::machine::Background::background_foreground`]，修复线 L-A，
    /// docs/specs/rust-cli/tui-input.md §11）：只发一次，之后同 [`CommandEvent::Backgrounded`]。
    BackgroundedBy { reason: BackgroundReason },
    /// 终态。超时：`outcome` 为 `Failed { code: "timeout" }`，`timed_out` 为 true，退出码 143；
    /// 取消：`Cancelled`。`cwd_reset` 是 [`CwdScope::Session`] 下结束目录越界、被重置到的目录。
    /// 输出文件超过执行环境的上限被结束时：`Cancelled`、退出码 137，`output_limit` 是说明文字。
    Finished {
        outcome: Outcome,
        exit_code: Option<i32>,
        #[serde(default)]
        timed_out: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd_reset: Option<EnvPath>,
        /// 输出文件的总字节数。
        #[serde(default)]
        output_bytes: u64,
        /// 输出文件超过执行环境的上限、命令因此被结束时的说明（文字
        /// `Command killed: output file exceeded 5GB`；docs/specs/rust-cli/shell-permissions.md §2.2）。
        /// 前台 Bash 把它放在输出前面；不是这种结束时为 None。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_limit: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}
