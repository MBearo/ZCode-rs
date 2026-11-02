//! kernel-api 的请求参数 → 线上的类型（客户端的换算，remote-exec.md §15.1 原则 3：只在这里与 `exec-server`）。
//! 字节字段追加到二进制段。
//!
//! 结构一律穷举解构（F128，审查 X1）：kernel-api 加字段时这里编译失败，必须决定它上不上线；不上线的写成 `_` 并注明理由。

use rust_cli_exec_wire::BodyBuilder;
use rust_cli_exec_wire::dto::command as wc;
use rust_cli_exec_wire::dto::core as w;
use rust_cli_exec_wire::dto::files as wf;
use rust_cli_exec_wire::dto::mcp as wm;
use rust_cli_exec_wire::dto::rewind as wr;
use rust_cli_exec_wire::dto::shell as ws;
use rust_cli_kernel_api::env::{
    CommandKind, OperationContext, Origin, StartCommand, WorkspaceRef, WritePrecondition,
};
use rust_cli_kernel_api::env_files::{GlobFiles, GrepMode, GrepSearch, PutFile, ReadText};
use rust_cli_kernel_api::env_pdf::RenderPdfPages;
use rust_cli_kernel_api::env_rewind::{RestoreFile, RewindPrepare};
use rust_cli_kernel_api::env_shell::{
    BackgroundCommand, BackgroundKind, BackgroundTask, CwdScope, FilesystemPolicy, HookIo,
    MonitorSocket, NetworkPolicy, SandboxPolicy, ShellKind, TimeoutBackground,
};
use rust_cli_kernel_api::mcp::{McpConnect, McpTransportConfig};

pub(crate) fn workspace(workspace: &WorkspaceRef) -> w::Workspace {
    let WorkspaceRef { environment, root } = workspace;
    w::Workspace {
        environment: environment.as_str().to_owned(),
        root: root.as_str().to_owned(),
    }
}

pub(crate) fn context(context: OperationContext) -> w::OperationContext {
    let OperationContext {
        environment,
        generation,
        owner_epoch,
        operation,
        origin,
    } = context;
    w::OperationContext {
        environment: environment.as_str().to_owned(),
        generation: generation.as_str().to_owned(),
        owner_epoch: owner_epoch.0,
        operation: operation.as_str().to_owned(),
        origin: match origin {
            Origin::Agent { session, run } => w::Origin::Agent {
                session: session.as_str().to_owned(),
                run: run.as_str().to_owned(),
            },
            Origin::User { session } => w::Origin::User {
                session: session.map(|session| session.as_str().to_owned()),
            },
        },
    }
}

pub(crate) fn precondition(precondition: WritePrecondition) -> wf::WritePrecondition {
    match precondition {
        WritePrecondition::Version(version) => {
            wf::WritePrecondition::Version(version.as_str().to_owned())
        }
        WritePrecondition::Absent => wf::WritePrecondition::Absent,
        WritePrecondition::Any => wf::WritePrecondition::Any,
    }
}

pub(crate) fn read_text(request: ReadText) -> wf::ReadText {
    let ReadText {
        path,
        start_line,
        max_lines,
        max_bytes,
    } = request;
    wf::ReadText {
        path: path.as_str().to_owned(),
        start_line,
        max_lines,
        max_bytes,
    }
}

pub(crate) fn put_file(request: PutFile, body: &mut BodyBuilder) -> wf::PutFile {
    let PutFile {
        path,
        bytes,
        precondition: written_if,
        create_parents,
    } = request;
    wf::PutFile {
        path: path.as_str().to_owned(),
        bytes: body.push(&bytes),
        precondition: precondition(written_if),
        create_parents,
    }
}

pub(crate) fn glob(request: GlobFiles) -> wf::GlobFiles {
    let GlobFiles {
        root,
        pattern,
        limit,
        hidden,
        respect_ignore,
    } = request;
    wf::GlobFiles {
        root: root.as_str().to_owned(),
        pattern,
        limit: limit as u64,
        hidden,
        respect_ignore,
    }
}

pub(crate) fn grep(request: GrepSearch) -> wf::GrepSearch {
    let GrepSearch {
        root,
        pattern,
        case_insensitive,
        multiline,
        globs,
        file_type,
        hidden,
        mode,
    } = request;
    wf::GrepSearch {
        root: root.as_str().to_owned(),
        pattern,
        case_insensitive,
        multiline,
        globs,
        file_type,
        hidden,
        mode: match mode {
            GrepMode::FilesWithMatches => wf::GrepMode::FilesWithMatches,
            GrepMode::Count => wf::GrepMode::Count,
            GrepMode::Content { before, after } => wf::GrepMode::Content { before, after },
        },
    }
}

fn background_command(command: BackgroundCommand) -> wc::BackgroundCommand {
    let BackgroundCommand {
        description,
        tool_use,
        kind,
    } = command;
    wc::BackgroundCommand {
        description,
        tool_use: tool_use.map(|tool| tool.as_str().to_owned()),
        kind: match kind {
            BackgroundKind::Bash => wc::BackgroundKind::Bash,
            BackgroundKind::Monitor => wc::BackgroundKind::Monitor,
        },
    }
}

fn sandbox(policy: SandboxPolicy) -> wc::SandboxPolicy {
    let paths = |list: Vec<rust_cli_kernel_api::env::EnvPath>| {
        list.into_iter()
            .map(|path| path.as_str().to_owned())
            .collect()
    };
    let SandboxPolicy {
        filesystem:
            FilesystemPolicy {
                allow_write,
                deny_write,
                deny_read,
            },
        network,
        required,
    } = policy;
    wc::SandboxPolicy {
        filesystem: wc::FilesystemPolicy {
            allow_write: paths(allow_write),
            deny_write: paths(deny_write),
            deny_read: paths(deny_read),
        },
        network: match network {
            NetworkPolicy::Unrestricted => wc::NetworkPolicy::Unrestricted,
            NetworkPolicy::LocalOnly {
                allow_local_binding,
            } => wc::NetworkPolicy::LocalOnly {
                allow_local_binding,
            },
            NetworkPolicy::Domains { allowed, denied } => {
                wc::NetworkPolicy::Domains { allowed, denied }
            }
        },
        required,
    }
}

pub(crate) fn start_command(request: StartCommand, body: &mut BodyBuilder) -> wc::StartCommand {
    let StartCommand {
        command,
        cwd,
        timeout_ms,
        task,
        shell,
        cwd_scope,
        sandbox: policy,
        kind,
    } = request;
    // 线上仍是四个可选字段（F128 之前的形状，wire 不随内部 API 变）；服务端的 `convert_in` 拼回种类。
    let (background, hook, on_timeout, socket) = match kind {
        CommandKind::Foreground { on_timeout } => (None, None, on_timeout, None),
        CommandKind::Background { background } => (Some(background), None, None, None),
        CommandKind::Hook { hook } => (None, Some(hook), None, None),
        CommandKind::Socket { background, socket } => (Some(background), None, None, Some(socket)),
    };
    wc::StartCommand {
        command,
        cwd: cwd.as_str().to_owned(),
        timeout_ms,
        task: task.as_str().to_owned(),
        shell: match shell {
            ShellKind::Bash => wc::ShellKind::Bash,
            ShellKind::PowerShell => wc::ShellKind::PowerShell,
        },
        cwd_scope: match cwd_scope {
            CwdScope::Pinned => wc::CwdScope::Pinned,
            CwdScope::Session { roots } => wc::CwdScope::Session {
                roots: roots
                    .into_iter()
                    .map(|root| root.as_str().to_owned())
                    .collect(),
            },
        },
        background: background.map(background_command),
        sandbox: policy.map(sandbox),
        hook: hook.map(|HookIo { stdin, env }| wc::HookIo {
            stdin: body.push(stdin.as_bytes()),
            env,
        }),
        on_timeout: on_timeout.map(
            |TimeoutBackground {
                 command,
                 deadline_ms,
             }| wc::TimeoutBackground {
                command: background_command(command),
                deadline_ms,
            },
        ),
        socket: socket.map(|MonitorSocket { url, protocols }| wc::MonitorSocket { url, protocols }),
    }
}

pub(crate) fn background_task(task: BackgroundTask) -> ws::BackgroundTask {
    let BackgroundTask { session, task } = task;
    ws::BackgroundTask {
        session: session.as_str().to_owned(),
        task: task.as_str().to_owned(),
    }
}

pub(crate) fn rewind_files(prepare: RewindPrepare, body: &mut BodyBuilder) -> Vec<wr::RestoreFile> {
    let RewindPrepare { files } = prepare;
    files
        .into_iter()
        .map(
            |RestoreFile {
                 path,
                 expected,
                 restore,
             }| wr::RestoreFile {
                path: path.as_str().to_owned(),
                expected,
                restore: restore.map(|bytes| body.push(&bytes)),
            },
        )
        .collect()
}

pub(crate) fn render_pdf_pages(request: RenderPdfPages) -> wf::RenderPdfPages {
    let RenderPdfPages {
        path,
        first_page,
        last_page,
    } = request;
    wf::RenderPdfPages {
        path: path.as_str().to_owned(),
        first_page,
        last_page,
    }
}

/// MCP 服务器的配置原样发出（`${VAR}` 由执行器用它自己的环境展开，remote-exec.md §22.7）。
pub(crate) fn mcp_transport(config: McpTransportConfig) -> wm::McpTransport {
    match config {
        McpTransportConfig::Stdio {
            command,
            args,
            env,
            cwd,
        } => wm::McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        },
        McpTransportConfig::Sse { url, headers } => wm::McpTransport::Sse { url, headers },
        McpTransportConfig::Http { url, headers } => wm::McpTransport::Http { url, headers },
    }
}

/// 一台机器上应该有的一个服务器（期望集合的一项，§22.5）。
pub(crate) fn mcp_declared(connect: McpConnect) -> wm::McpDeclared {
    let McpConnect { server, transport } = connect;
    wm::McpDeclared {
        server,
        transport: mcp_transport(transport),
    }
}
