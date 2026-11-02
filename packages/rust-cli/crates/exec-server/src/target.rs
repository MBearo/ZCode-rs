//! 一个请求在服务端的去处（remote-exec.md §18.3）：入口按请求只分一次，穷举——加一种消息时必须在这里决定它交给哪一类。
//! 四类：注入的 `MachineEnv` 的端口（dispatch，按端口分文件）、起命令（started）、MCP 的管道（mcp）、环境变量推送
//! （env）。各类拿到的是自己那一类的参数，不再按名字回头判断。

use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::command::StartCommand;
use rust_cli_exec_wire::dto::core::{OperationContext, Workspace};
use rust_cli_exec_wire::dto::env::{EnvMode, EnvVars};
use rust_cli_exec_wire::dto::mcp::{McpDeclared, McpTransport};
use rust_cli_exec_wire::request::Request;
use rust_cli_kernel_api::machine::MachineEnv;

use crate::dispatch::{
    PortCall, background, call, call_with_body, checkpoints, commands, files, git, info, pdf,
    search, shell,
};

pub(crate) enum Target {
    Port(Port),
    Start(Start),
    Mcp(Mcp),
    Env(EnvPush),
}

/// 端口的一次调用（参数已经绑好），或端口的一个流。
pub(crate) enum Port {
    Call(PortCall),
    /// `commands.watch_command`：同一个客户端实例的第一个 `(op, 0)` 先接走 `start_command` 留下的流（F119）。
    Watch {
        operation: String,
        cursor: u64,
    },
    /// `shell.shell_notices`。
    Notices {
        session: String,
        after: u64,
    },
}

/// `commands.start_command`：执行器拿着端口给的流，等线上的 `watch_command` 接走（started.rs）。
pub(crate) struct Start {
    pub workspace: Workspace,
    pub context: OperationContext,
    pub request: Box<StartCommand>,
    pub body: Body,
}

/// MCP 的管道（R5，§22.4）：连接表与客户端实例在 mcp.rs。
pub(crate) enum Mcp {
    Call(McpCall),
    /// 这条连接上的事件（流）。
    Events {
        connection: String,
    },
}

/// MCP 的一次调用。
pub(crate) enum McpCall {
    Connect {
        workspace: Workspace,
        server: String,
        transport: McpTransport,
    },
    Send {
        connection: String,
        message: serde_json::Value,
    },
    Close {
        connection: String,
    },
    Declare {
        workspace: Workspace,
        servers: Vec<McpDeclared>,
    },
}

/// `env.update`（F96，§23.5）：在读循环里当场生效。
pub(crate) struct EnvPush {
    pub mode: EnvMode,
    pub vars: EnvVars,
    pub injected: Vec<String>,
}

impl Target {
    /// 分一次。端口的调用拿一份 `machine` 与二进制段（都是共享的句柄）。
    pub(crate) fn of(request: Request, machine: &MachineEnv, body: &Body) -> Self {
        let m = machine.clone();
        let port = |work: PortCall| Self::Port(Port::Call(work));
        match request {
            Request::Facts { workspace } => port(call(info::facts(m, workspace))),
            Request::PathFacts { workspace } => port(call(info::path_facts(m, workspace))),
            Request::ReadFile { workspace, path } => {
                port(call_with_body(files::read_file(m, workspace, path)))
            }
            Request::WriteFile {
                workspace,
                context,
                path,
                bytes,
                precondition,
            } => port(call(files::write_file(
                m,
                (workspace, context),
                (path, bytes, precondition),
                body.clone(),
            ))),
            Request::Stat { workspace, path } => port(call(files::stat(m, workspace, path))),
            Request::StatMany { workspace, paths } => {
                port(call(files::stat_many(m, workspace, paths)))
            }
            Request::ListDir { workspace, path } => port(call(files::list_dir(m, workspace, path))),
            Request::ReadText { workspace, request } => {
                port(call_with_body(files::read_text(m, workspace, request)))
            }
            Request::PutFile {
                workspace,
                context,
                request,
            } => port(call(files::put_file(
                m,
                (workspace, context),
                request,
                body.clone(),
            ))),
            Request::EnsureDir { workspace, path } => {
                port(call(files::ensure_dir(m, workspace, path)))
            }
            Request::Glob { workspace, request } => port(call(search::glob(m, workspace, request))),
            Request::ListFiles { workspace, cwd } => {
                port(call(search::list_files(m, workspace, cwd)))
            }
            Request::Grep { workspace, request } => port(call(search::grep(m, workspace, request))),
            Request::StartCommand {
                workspace,
                context,
                request,
            } => Self::Start(Start {
                workspace,
                context,
                request,
                body: body.clone(),
            }),
            Request::WatchCommand { operation, cursor } => {
                Self::Port(Port::Watch { operation, cursor })
            }
            Request::Cancel { operation } => port(call(commands::cancel(m, operation))),
            Request::CloseSession { session, forget } => {
                port(call(commands::close_session(m, session, forget)))
            }
            Request::ShellState { workspace, session } => {
                port(call(shell::shell_state(m, workspace, session)))
            }
            Request::ShellNotices { session, after } => {
                Self::Port(Port::Notices { session, after })
            }
            Request::AckShellNotice { session, seq } => {
                port(call(shell::ack_shell_notice(m, session, seq)))
            }
            Request::SetShellCwd { session, cwd } => {
                port(call(shell::set_shell_cwd(m, session, cwd)))
            }
            Request::BackgroundCommand { task } => {
                port(call(background::background_command(m, task)))
            }
            Request::BackgroundElapsed { task } => {
                port(call(background::background_elapsed(m, task)))
            }
            Request::StopBackgroundCommand { context, task } => {
                port(call(background::stop_background_command(m, context, task)))
            }
            Request::RewindPrepare {
                workspace,
                context,
                files,
            } => port(call(checkpoints::rewind_prepare(
                m,
                (workspace, context),
                files,
                body.clone(),
            ))),
            Request::RewindApply { token } => port(call(checkpoints::rewind_apply(m, token))),
            Request::RewindFinish { token } => port(call(checkpoints::rewind_finish(m, token))),
            Request::PdfPageCount { workspace, path } => {
                port(call(pdf::pdf_page_count(m, workspace, path)))
            }
            Request::RenderPdfPages { workspace, request } => {
                port(call_with_body(pdf::render_pdf_pages(m, workspace, request)))
            }
            Request::McpConnect {
                workspace,
                server,
                transport,
            } => Self::Mcp(Mcp::Call(McpCall::Connect {
                workspace,
                server,
                transport,
            })),
            Request::McpSend {
                connection,
                message,
            } => Self::Mcp(Mcp::Call(McpCall::Send {
                connection,
                message,
            })),
            Request::McpEvents { connection } => Self::Mcp(Mcp::Events { connection }),
            Request::McpClose { connection } => Self::Mcp(Mcp::Call(McpCall::Close { connection })),
            Request::McpDeclare { workspace, servers } => {
                Self::Mcp(Mcp::Call(McpCall::Declare { workspace, servers }))
            }
            Request::EnvUpdate {
                mode,
                vars,
                injected,
            } => Self::Env(EnvPush {
                mode,
                vars,
                injected,
            }),
            Request::GitStatus { workspace, cwd } => port(call(git::status(m, workspace, cwd))),
            Request::GitDiff { workspace, cwd } => port(call(git::diff(m, workspace, cwd))),
            Request::WorktreeCreate { context, request } => {
                port(call(git::worktree_create(m, context, request)))
            }
            Request::WorktreeList { cwd } => port(call(git::worktree_list(m, cwd))),
            Request::WorktreeChanges { request } => port(call(git::worktree_changes(m, request))),
            Request::WorktreeRemove { context, request } => {
                port(call(git::worktree_remove(m, context, request)))
            }
        }
    }

    /// 结果是不是带 `seq` 的流（与 `Op::kind()` 的 `Stream` 一致，测试核对）。
    #[cfg(test)]
    pub(crate) fn is_stream(&self) -> bool {
        matches!(
            self,
            Self::Port(Port::Watch { .. } | Port::Notices { .. }) | Self::Mcp(Mcp::Events { .. })
        )
    }
}

#[cfg(test)]
#[path = "target_tests.rs"]
mod tests;
