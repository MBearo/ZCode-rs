//! 线上的请求参数 → kernel-api 的类型（服务端的换算，remote-exec.md §15.1 原则 3：只在这里与 `env-remote`）。
//! 字节字段从二进制段取；越界是 `bad_request`。

use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::command as wc;
use rust_cli_exec_wire::dto::core as w;
use rust_cli_exec_wire::dto::files as wf;
use rust_cli_exec_wire::dto::rewind as wr;
use rust_cli_exec_wire::dto::shell as ws;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_kernel_api::env::{
    EnvPath, FileVersion, OperationContext, Origin, StartCommand, WorkspaceRef, WritePrecondition,
};
use rust_cli_kernel_api::env_files::{GlobFiles, GrepMode, GrepSearch, PutFile, ReadText};
use rust_cli_kernel_api::env_pdf::RenderPdfPages;
use rust_cli_kernel_api::env_rewind::{RestoreFile, RewindPrepare};
use rust_cli_kernel_api::env_shell::{
    BackgroundCommand, BackgroundKind, BackgroundTask, CwdScope, FilesystemPolicy, HookIo,
    MonitorSocket, NetworkPolicy, SandboxPolicy, ShellKind, TimeoutBackground,
};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, OwnerEpoch, RunId, SessionId, ShellTaskId, ToolUseId,
};

pub(crate) fn path(path: String) -> EnvPath {
    EnvPath::new(path)
}

pub(crate) fn workspace(workspace: w::Workspace) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new(workspace.environment),
        root: EnvPath::new(workspace.root),
    }
}

pub(crate) fn context(context: w::OperationContext) -> OperationContext {
    OperationContext {
        environment: EnvironmentId::new(context.environment),
        generation: Generation::new(context.generation),
        owner_epoch: OwnerEpoch(context.owner_epoch),
        operation: OperationId::new(context.operation),
        origin: match context.origin {
            w::Origin::Agent { session, run } => Origin::Agent {
                session: SessionId::new(session),
                run: RunId::new(run),
            },
            w::Origin::User { session } => Origin::User {
                session: session.map(SessionId::new),
            },
        },
    }
}

pub(crate) fn precondition(precondition: wf::WritePrecondition) -> WritePrecondition {
    match precondition {
        wf::WritePrecondition::Version(version) => {
            WritePrecondition::Version(FileVersion::new(version))
        }
        wf::WritePrecondition::Absent => WritePrecondition::Absent,
        wf::WritePrecondition::Any => WritePrecondition::Any,
    }
}

pub(crate) fn read_text(request: wf::ReadText) -> ReadText {
    ReadText {
        path: path(request.path),
        start_line: request.start_line,
        max_lines: request.max_lines,
        max_bytes: request.max_bytes,
    }
}

pub(crate) fn put_file(request: wf::PutFile, body: &Body) -> Result<PutFile, WireError> {
    Ok(PutFile {
        path: path(request.path),
        bytes: body.to_vec(&request.bytes)?,
        precondition: precondition(request.precondition),
        create_parents: request.create_parents,
    })
}

pub(crate) fn glob(request: wf::GlobFiles) -> GlobFiles {
    GlobFiles {
        root: path(request.root),
        pattern: request.pattern,
        limit: usize::try_from(request.limit).unwrap_or(usize::MAX),
        hidden: request.hidden,
        respect_ignore: request.respect_ignore,
    }
}

pub(crate) fn grep(request: wf::GrepSearch) -> GrepSearch {
    GrepSearch {
        root: path(request.root),
        pattern: request.pattern,
        case_insensitive: request.case_insensitive,
        multiline: request.multiline,
        globs: request.globs,
        file_type: request.file_type,
        hidden: request.hidden,
        mode: match request.mode {
            wf::GrepMode::FilesWithMatches => GrepMode::FilesWithMatches,
            wf::GrepMode::Count => GrepMode::Count,
            wf::GrepMode::Content { before, after } => GrepMode::Content { before, after },
        },
    }
}

fn background_kind(kind: wc::BackgroundKind) -> BackgroundKind {
    match kind {
        wc::BackgroundKind::Bash => BackgroundKind::Bash,
        wc::BackgroundKind::Monitor => BackgroundKind::Monitor,
    }
}

fn background_command(command: wc::BackgroundCommand) -> BackgroundCommand {
    BackgroundCommand {
        description: command.description,
        tool_use: command.tool_use.map(ToolUseId::new),
        kind: background_kind(command.kind),
    }
}

fn sandbox(policy: wc::SandboxPolicy) -> SandboxPolicy {
    let paths = |list: Vec<String>| list.into_iter().map(path).collect();
    SandboxPolicy {
        filesystem: FilesystemPolicy {
            allow_write: paths(policy.filesystem.allow_write),
            deny_write: paths(policy.filesystem.deny_write),
            deny_read: paths(policy.filesystem.deny_read),
        },
        network: match policy.network {
            wc::NetworkPolicy::Unrestricted => NetworkPolicy::Unrestricted,
            wc::NetworkPolicy::LocalOnly {
                allow_local_binding,
            } => NetworkPolicy::LocalOnly {
                allow_local_binding,
            },
            wc::NetworkPolicy::Domains { allowed, denied } => {
                NetworkPolicy::Domains { allowed, denied }
            }
        },
        required: policy.required,
    }
}

pub(crate) fn start_command(
    request: wc::StartCommand,
    body: &Body,
) -> Result<StartCommand, WireError> {
    let hook = match request.hook {
        Some(hook) => Some(HookIo {
            stdin: body.text(&hook.stdin)?,
            env: hook.env,
        }),
        None => None,
    };
    Ok(StartCommand {
        command: request.command,
        cwd: path(request.cwd),
        timeout_ms: request.timeout_ms,
        task: ShellTaskId::new(request.task),
        shell: match request.shell {
            wc::ShellKind::Bash => ShellKind::Bash,
            wc::ShellKind::PowerShell => ShellKind::PowerShell,
        },
        cwd_scope: match request.cwd_scope {
            wc::CwdScope::Pinned => CwdScope::Pinned,
            wc::CwdScope::Session { roots } => CwdScope::Session {
                roots: roots.into_iter().map(path).collect(),
            },
        },
        background: request.background.map(background_command),
        sandbox: request.sandbox.map(sandbox),
        hook,
        on_timeout: request.on_timeout.map(|moved| TimeoutBackground {
            command: background_command(moved.command),
            deadline_ms: moved.deadline_ms,
        }),
        socket: request.socket.map(|socket| MonitorSocket {
            url: socket.url,
            protocols: socket.protocols,
        }),
    })
}

pub(crate) fn background_task(task: ws::BackgroundTask) -> BackgroundTask {
    BackgroundTask {
        session: SessionId::new(task.session),
        task: ShellTaskId::new(task.task),
    }
}

pub(crate) fn rewind_prepare(
    files: Vec<wr::RestoreFile>,
    body: &Body,
) -> Result<RewindPrepare, WireError> {
    let files = files
        .into_iter()
        .map(|file| {
            Ok(RestoreFile {
                path: path(file.path),
                expected: file.expected,
                restore: file.restore.map(|blob| body.to_vec(&blob)).transpose()?,
            })
        })
        .collect::<Result<_, WireError>>()?;
    Ok(RewindPrepare { files })
}

pub(crate) fn render_pdf_pages(request: wf::RenderPdfPages) -> RenderPdfPages {
    RenderPdfPages {
        path: path(request.path),
        first_page: request.first_page,
        last_page: request.last_page,
    }
}

pub(crate) fn operation(operation: String) -> OperationId {
    OperationId::new(operation)
}

pub(crate) fn session(session: String) -> SessionId {
    SessionId::new(session)
}
