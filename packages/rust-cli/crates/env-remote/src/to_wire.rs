//! kernel-api 的请求参数 → 线上的类型（客户端的换算，remote-exec.md §15.1 原则 3：只在这里与 `exec-server`）。
//! 字节字段追加到二进制段。

use rust_cli_exec_wire::BodyBuilder;
use rust_cli_exec_wire::dto::command as wc;
use rust_cli_exec_wire::dto::core as w;
use rust_cli_exec_wire::dto::files as wf;
use rust_cli_exec_wire::dto::rewind as wr;
use rust_cli_exec_wire::dto::shell as ws;
use rust_cli_kernel_api::env::{
    OperationContext, Origin, StartCommand, WorkspaceRef, WritePrecondition,
};
use rust_cli_kernel_api::env_files::{GlobFiles, GrepMode, GrepSearch, PutFile, ReadText};
use rust_cli_kernel_api::env_pdf::RenderPdfPages;
use rust_cli_kernel_api::env_rewind::RewindPrepare;
use rust_cli_kernel_api::env_shell::{
    BackgroundCommand, BackgroundKind, BackgroundTask, CwdScope, NetworkPolicy, SandboxPolicy,
    ShellKind,
};

pub(crate) fn workspace(workspace: &WorkspaceRef) -> w::Workspace {
    w::Workspace {
        environment: workspace.environment.as_str().to_owned(),
        root: workspace.root.as_str().to_owned(),
    }
}

pub(crate) fn context(context: OperationContext) -> w::OperationContext {
    w::OperationContext {
        environment: context.environment.as_str().to_owned(),
        generation: context.generation.as_str().to_owned(),
        owner_epoch: context.owner_epoch.0,
        operation: context.operation.as_str().to_owned(),
        origin: match context.origin {
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
    wf::ReadText {
        path: request.path.as_str().to_owned(),
        start_line: request.start_line,
        max_lines: request.max_lines,
        max_bytes: request.max_bytes,
    }
}

pub(crate) fn put_file(request: PutFile, body: &mut BodyBuilder) -> wf::PutFile {
    wf::PutFile {
        path: request.path.as_str().to_owned(),
        bytes: body.push(&request.bytes),
        precondition: precondition(request.precondition),
        create_parents: request.create_parents,
    }
}

pub(crate) fn glob(request: GlobFiles) -> wf::GlobFiles {
    wf::GlobFiles {
        root: request.root.as_str().to_owned(),
        pattern: request.pattern,
        limit: request.limit as u64,
        hidden: request.hidden,
        respect_ignore: request.respect_ignore,
    }
}

pub(crate) fn grep(request: GrepSearch) -> wf::GrepSearch {
    wf::GrepSearch {
        root: request.root.as_str().to_owned(),
        pattern: request.pattern,
        case_insensitive: request.case_insensitive,
        multiline: request.multiline,
        globs: request.globs,
        file_type: request.file_type,
        hidden: request.hidden,
        mode: match request.mode {
            GrepMode::FilesWithMatches => wf::GrepMode::FilesWithMatches,
            GrepMode::Count => wf::GrepMode::Count,
            GrepMode::Content { before, after } => wf::GrepMode::Content { before, after },
        },
    }
}

fn background_command(command: BackgroundCommand) -> wc::BackgroundCommand {
    wc::BackgroundCommand {
        description: command.description,
        tool_use: command.tool_use.map(|tool| tool.as_str().to_owned()),
        kind: match command.kind {
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
    wc::SandboxPolicy {
        filesystem: wc::FilesystemPolicy {
            allow_write: paths(policy.filesystem.allow_write),
            deny_write: paths(policy.filesystem.deny_write),
            deny_read: paths(policy.filesystem.deny_read),
        },
        network: match policy.network {
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
        required: policy.required,
    }
}

pub(crate) fn start_command(request: StartCommand, body: &mut BodyBuilder) -> wc::StartCommand {
    wc::StartCommand {
        command: request.command,
        cwd: request.cwd.as_str().to_owned(),
        timeout_ms: request.timeout_ms,
        task: request.task.as_str().to_owned(),
        shell: match request.shell {
            ShellKind::Bash => wc::ShellKind::Bash,
            ShellKind::PowerShell => wc::ShellKind::PowerShell,
        },
        cwd_scope: match request.cwd_scope {
            CwdScope::Pinned => wc::CwdScope::Pinned,
            CwdScope::Session { roots } => wc::CwdScope::Session {
                roots: roots
                    .into_iter()
                    .map(|root| root.as_str().to_owned())
                    .collect(),
            },
        },
        background: request.background.map(background_command),
        sandbox: request.sandbox.map(sandbox),
        hook: request.hook.map(|hook| wc::HookIo {
            stdin: body.push(hook.stdin.as_bytes()),
            env: hook.env,
        }),
        on_timeout: request.on_timeout.map(|moved| wc::TimeoutBackground {
            command: background_command(moved.command),
            deadline_ms: moved.deadline_ms,
        }),
        socket: request.socket.map(|socket| wc::MonitorSocket {
            url: socket.url,
            protocols: socket.protocols,
        }),
    }
}

pub(crate) fn background_task(task: BackgroundTask) -> ws::BackgroundTask {
    ws::BackgroundTask {
        session: task.session.as_str().to_owned(),
        task: task.task.as_str().to_owned(),
    }
}

pub(crate) fn rewind_files(prepare: RewindPrepare, body: &mut BodyBuilder) -> Vec<wr::RestoreFile> {
    prepare
        .files
        .into_iter()
        .map(|file| wr::RestoreFile {
            path: file.path.as_str().to_owned(),
            expected: file.expected,
            restore: file.restore.map(|bytes| body.push(&bytes)),
        })
        .collect()
}

pub(crate) fn render_pdf_pages(request: RenderPdfPages) -> wf::RenderPdfPages {
    wf::RenderPdfPages {
        path: request.path.as_str().to_owned(),
        first_page: request.first_page,
        last_page: request.last_page,
    }
}
