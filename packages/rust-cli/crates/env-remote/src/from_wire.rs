//! 线上的结果 → kernel-api 的类型（客户端的换算）。字节字段从二进制段取；越界是 `io` 错误。

use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::command as wc;
use rust_cli_exec_wire::dto::core as w;
use rust_cli_exec_wire::dto::files as wf;
use rust_cli_exec_wire::dto::git as wg;
use rust_cli_exec_wire::dto::mcp as wm;
use rust_cli_exec_wire::dto::rewind as wr;
use rust_cli_exec_wire::dto::shell as ws;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_exec_wire::handshake::Welcome;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, FileVersion, Outcome, OutputStream,
    WorkspaceRef,
};
use rust_cli_kernel_api::env_files::{
    DirEntry, FileKind, FileStat, FileWritten, GlobMatches, GrepCount, GrepFileHit, GrepFileLines,
    GrepLine, GrepLineKind, GrepOutcome, GrepSubmatch, PathFacts, PathStyle, TextRead, TextSlice,
};
use rust_cli_kernel_api::env_git::{GitDiff, GitSnapshot, GitStatus};
use rust_cli_kernel_api::env_pdf::PdfRender;
use rust_cli_kernel_api::env_registry::{MachineFacts, RemoteMachine};
use rust_cli_kernel_api::env_rewind::{
    RestoreAction, RestoredFile, RewindConflict, RewindPlan, RewindStatus,
};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundKind, BackgroundStatus, BackgroundStop, ShellNotice, ShellState,
};
use rust_cli_kernel_api::env_worktree::{
    WorktreeChanges, WorktreeCreated, WorktreeEntry, WorktreeList,
};
use rust_cli_kernel_api::ids::{EnvironmentId, OperationId, ShellTaskId, ToolUseId};
use rust_cli_kernel_api::mcp::{McpError, McpTransportEvent};

/// 线上的错误 → `EnvError`：七种一一对应；协议自己的两种是 `Io`。
pub(crate) fn error(error: WireError) -> EnvError {
    match error {
        WireError::NotFound { path } => EnvError::NotFound { path },
        WireError::PreconditionFailed { path } => EnvError::PreconditionFailed { path },
        WireError::PermissionDenied { message } => EnvError::PermissionDenied { message },
        WireError::Stale { message } => EnvError::Stale { message },
        WireError::OperationConflict { operation } => EnvError::OperationConflict {
            operation: OperationId::new(operation),
        },
        WireError::Io { message } => EnvError::Io { message },
        WireError::Unsupported { what } => EnvError::Unsupported { what },
        other @ (WireError::BadRequest { .. } | WireError::Interrupted { .. }) => EnvError::Io {
            message: other.to_string(),
        },
    }
}

fn path(path: String) -> EnvPath {
    EnvPath::new(path)
}

fn bytes(body: &Body, blob: &rust_cli_exec_wire::Blob) -> Result<Vec<u8>, EnvError> {
    body.to_vec(blob).map_err(error)
}

pub(crate) fn outcome(outcome: w::Outcome) -> Outcome {
    match outcome {
        w::Outcome::Completed => Outcome::Completed,
        w::Outcome::Failed { code, message } => Outcome::Failed { code, message },
        w::Outcome::Rejected { message } => Outcome::Rejected { message },
        w::Outcome::Running => Outcome::Running,
        w::Outcome::Unknown => Outcome::Unknown,
        w::Outcome::Interrupted => Outcome::Interrupted,
        w::Outcome::Cancelled => Outcome::Cancelled,
    }
}

pub(crate) fn facts(facts: w::EnvFacts) -> EnvFacts {
    EnvFacts {
        cwd: path(facts.cwd),
        platform: facts.platform,
        os_version: facts.os_version,
        shell: facts.shell,
        is_git_repo: facts.is_git_repo,
        home: facts.home.map(path),
        uid: facts.uid,
    }
}

pub(crate) fn path_facts(facts: w::PathFacts) -> PathFacts {
    PathFacts {
        home: facts.home.map(path),
        style: path_style(facts.style),
    }
}

pub(crate) fn file_content(content: wf::FileContent, body: &Body) -> Result<FileContent, EnvError> {
    Ok(FileContent {
        bytes: bytes(body, &content.bytes)?,
        version: FileVersion::new(content.version),
    })
}

fn kind(kind: wf::FileKind) -> FileKind {
    match kind {
        wf::FileKind::File => FileKind::File,
        wf::FileKind::Directory => FileKind::Directory,
        wf::FileKind::Other => FileKind::Other,
    }
}

/// `files.stat_many` 的一项（R3）。
pub(crate) fn stat_entry(entry: wf::StatEntry) -> Result<Option<FileStat>, EnvError> {
    match entry.error {
        Some(wire) => Err(error(wire)),
        None => Ok(entry.stat.map(file_stat)),
    }
}

pub(crate) fn file_stat(stat: wf::FileStat) -> FileStat {
    FileStat {
        kind: kind(stat.kind),
        size: stat.size,
        mtime_ms: stat.mtime_ms,
        version: FileVersion::new(stat.version),
    }
}

pub(crate) fn dir_entry(entry: wf::DirEntry) -> DirEntry {
    DirEntry {
        name: entry.name,
        kind: kind(entry.kind),
    }
}

pub(crate) fn text_read(read: wf::TextRead, body: &Body) -> Result<TextRead, EnvError> {
    Ok(match read {
        wf::TextRead::Text { slice } => TextRead::Text(TextSlice {
            content: body.text(&slice.content).map_err(error)?,
            line_count: slice.line_count,
            total_lines: slice.total_lines,
            total_bytes: slice.total_bytes,
            read_bytes: slice.read_bytes,
            mtime_ms: slice.mtime_ms,
            version: FileVersion::new(slice.version),
        }),
        wf::TextRead::Directory => TextRead::Directory,
        wf::TextRead::TooLarge { size } => TextRead::TooLarge { size },
    })
}

pub(crate) fn file_written(written: wf::FileWritten) -> FileWritten {
    FileWritten {
        version: FileVersion::new(written.version),
        mtime_ms: written.mtime_ms,
        created: written.created,
    }
}

pub(crate) fn glob_matches(matches: wf::GlobMatches) -> GlobMatches {
    GlobMatches {
        files: matches.files.into_iter().map(path).collect(),
        total: usize::try_from(matches.total).unwrap_or(usize::MAX),
    }
}

pub(crate) fn grep_outcome(outcome: wf::GrepOutcome) -> GrepOutcome {
    match outcome {
        wf::GrepOutcome::Files { files } => GrepOutcome::Files {
            files: files
                .into_iter()
                .map(|hit| GrepFileHit {
                    path: path(hit.path),
                    mtime_ms: hit.mtime_ms,
                })
                .collect(),
        },
        wf::GrepOutcome::Counts { counts } => GrepOutcome::Counts {
            counts: counts
                .into_iter()
                .map(|count| GrepCount {
                    path: path(count.path),
                    count: count.count,
                })
                .collect(),
        },
        wf::GrepOutcome::Content { files } => GrepOutcome::Content {
            files: files
                .into_iter()
                .map(|file| GrepFileLines {
                    path: path(file.path),
                    lines: file
                        .lines
                        .into_iter()
                        .map(|line| GrepLine {
                            kind: match line.kind {
                                wf::GrepLineKind::Match => GrepLineKind::Match,
                                wf::GrepLineKind::Context => GrepLineKind::Context,
                            },
                            line_number: line.line_number,
                            text: line.text,
                            submatches: line
                                .submatches
                                .into_iter()
                                .map(|sub| GrepSubmatch {
                                    start: sub.start,
                                    text: sub.text,
                                })
                                .collect(),
                        })
                        .collect(),
                    binary_offset: file.binary_offset,
                })
                .collect(),
        },
        wf::GrepOutcome::Rejected { message } => GrepOutcome::Rejected { message },
    }
}

pub(crate) fn shell_state(state: ws::ShellState) -> ShellState {
    ShellState {
        cwd: path(state.cwd),
        outputs: state.outputs.map(path),
        background: state.background.into_iter().map(ShellTaskId::new).collect(),
    }
}

fn background_kind(kind: wc::BackgroundKind) -> BackgroundKind {
    match kind {
        wc::BackgroundKind::Bash => BackgroundKind::Bash,
        wc::BackgroundKind::Monitor => BackgroundKind::Monitor,
    }
}

fn background_status(status: ws::BackgroundStatus) -> BackgroundStatus {
    match status {
        ws::BackgroundStatus::Running => BackgroundStatus::Running,
        ws::BackgroundStatus::Completed => BackgroundStatus::Completed,
        ws::BackgroundStatus::Failed => BackgroundStatus::Failed,
        ws::BackgroundStatus::Killed => BackgroundStatus::Killed,
    }
}

pub(crate) fn background_info(info: ws::BackgroundInfo) -> BackgroundInfo {
    BackgroundInfo {
        command: info.command,
        status: background_status(info.status),
        kind: background_kind(info.kind),
        description: info.description,
    }
}

pub(crate) fn background_stop(stop: ws::BackgroundStop) -> BackgroundStop {
    match stop {
        ws::BackgroundStop::Stopped { command } => BackgroundStop::Stopped { command },
        ws::BackgroundStop::NotRunning { status } => BackgroundStop::NotRunning {
            status: background_status(status),
        },
        ws::BackgroundStop::NotFound => BackgroundStop::NotFound,
    }
}

fn conflict(conflict: wr::RewindConflict) -> RewindConflict {
    RewindConflict {
        path: path(conflict.path),
        current: conflict.current,
        message: conflict.message,
    }
}

pub(crate) fn rewind_plan(plan: wr::RewindPlan) -> RewindPlan {
    match plan {
        wr::RewindPlan::Ready => RewindPlan::Ready,
        wr::RewindPlan::Conflicts { files } => RewindPlan::Conflicts {
            files: files.into_iter().map(conflict).collect(),
        },
    }
}

pub(crate) fn rewind_status(status: wr::RewindStatus) -> RewindStatus {
    match status {
        wr::RewindStatus::Prepared => RewindStatus::Prepared,
        wr::RewindStatus::Applying => RewindStatus::Applying,
        wr::RewindStatus::Applied { files } => RewindStatus::Applied {
            files: files
                .into_iter()
                .map(|file| RestoredFile {
                    path: path(file.path),
                    action: match file.action {
                        wr::RestoreAction::Restore => RestoreAction::Restore,
                        wr::RestoreAction::Delete => RestoreAction::Delete,
                    },
                })
                .collect(),
        },
        wr::RewindStatus::Failed { message, conflicts } => RewindStatus::Failed {
            message,
            conflicts: conflicts.into_iter().map(conflict).collect(),
        },
        wr::RewindStatus::Unknown => RewindStatus::Unknown,
    }
}

pub(crate) fn pdf_render(render: wf::PdfRender, body: &Body) -> Result<PdfRender, EnvError> {
    Ok(match render {
        wf::PdfRender::Unavailable => PdfRender::Unavailable,
        wf::PdfRender::Failed { exit_code, stderr } => PdfRender::Failed { exit_code, stderr },
        wf::PdfRender::Rendered { pages } => PdfRender::Rendered {
            pages: pages
                .iter()
                .map(|page| bytes(body, page))
                .collect::<Result<_, _>>()?,
        },
    })
}

pub(crate) fn command_event(
    event: wc::CommandEvent,
    body: &Body,
) -> Result<CommandEvent, EnvError> {
    Ok(match event {
        wc::CommandEvent::Started { output } => CommandEvent::Started {
            output: path(output),
        },
        wc::CommandEvent::Output {
            stream,
            data,
            cursor,
        } => CommandEvent::Output {
            stream: match stream {
                wc::OutputStream::Stdout => OutputStream::Stdout,
                wc::OutputStream::Stderr => OutputStream::Stderr,
            },
            data: bytes(body, &data)?,
            cursor,
        },
        wc::CommandEvent::Backgrounded => CommandEvent::Backgrounded,
        wc::CommandEvent::Finished {
            outcome: ended,
            exit_code,
            timed_out,
            cwd_reset,
            output_bytes,
            output_limit,
        } => CommandEvent::Finished {
            outcome: outcome(ended),
            exit_code,
            timed_out,
            cwd_reset: cwd_reset.map(path),
            output_bytes,
            output_limit,
        },
    })
}

pub(crate) fn shell_notice(notice: ws::ShellNotice) -> ShellNotice {
    ShellNotice {
        seq: notice.seq,
        operation: OperationId::new(notice.operation),
        task: ShellTaskId::new(notice.task),
        tool_use: notice.tool_use.map(ToolUseId::new),
        description: notice.description,
        output: path(notice.output),
        outcome: outcome(notice.outcome),
        exit_code: notice.exit_code,
        kind: background_kind(notice.kind),
        output_bytes: notice.output_bytes,
    }
}

pub(crate) fn mcp_event(event: wm::McpEvent) -> McpTransportEvent {
    match event {
        wm::McpEvent::Message { message } => McpTransportEvent::Message { message },
        wm::McpEvent::Closed {
            reason,
            session_expired,
        } => McpTransportEvent::Closed {
            reason,
            session_expired,
        },
    }
}

/// 线上错误的原文（执行器的 `Connect` / `Io` 是 `io`、`Closed` 是 `not_found`，§22.4）。
pub(crate) fn mcp_text(error: &WireError) -> String {
    match error {
        WireError::Io { message } => message.clone(),
        WireError::NotFound { path } => path.clone(),
        other => other.to_string(),
    }
}

/// `mcp.send` / `mcp.close` / `mcp.declare` 的错误：连接不在了是 `Closed`，执行器没有这一能力是 `Unsupported`，其余是
/// `Io`。
pub(crate) fn mcp_error(error: WireError) -> McpError {
    match error {
        WireError::NotFound { path } => McpError::Closed { message: path },
        WireError::Unsupported { what } => McpError::Unsupported { what },
        other => McpError::Io {
            message: mcp_text(&other),
        },
    }
}

pub(crate) fn git_status(status: wg::GitStatus) -> GitStatus {
    match status {
        wg::GitStatus::NotRepository => GitStatus::NotRepository,
        wg::GitStatus::Repository(snapshot) => GitStatus::Repository(GitSnapshot {
            branch: snapshot.branch,
            default_branch: snapshot.default_branch,
            status: snapshot.status,
            recent_commits: snapshot.recent_commits,
            user_name: snapshot.user_name,
        }),
    }
}

pub(crate) fn git_diff(diff: wg::GitDiff) -> GitDiff {
    match diff {
        wg::GitDiff::NotRepository => GitDiff::NotRepository,
        wg::GitDiff::Repository {
            numstat,
            patch,
            untracked,
        } => GitDiff::Repository {
            numstat,
            patch,
            untracked,
        },
    }
}

pub(crate) fn worktree_created(created: wg::WorktreeCreated) -> WorktreeCreated {
    match created {
        wg::WorktreeCreated::Ready {
            path,
            branch,
            head_commit,
            existed,
        } => WorktreeCreated::Ready {
            path: EnvPath::new(path),
            branch,
            head_commit,
            existed,
        },
        wg::WorktreeCreated::NotARepository => WorktreeCreated::NotARepository,
        wg::WorktreeCreated::Failed { message } => WorktreeCreated::Failed { message },
    }
}

pub(crate) fn worktree_list(list: wg::WorktreeList) -> WorktreeList {
    WorktreeList {
        root: list.root.map(EnvPath::new),
        entries: list
            .entries
            .into_iter()
            .map(|entry| WorktreeEntry {
                path: EnvPath::new(entry.path),
                branch: entry.branch,
            })
            .collect(),
    }
}

pub(crate) fn worktree_changes(changes: wg::WorktreeChanges) -> WorktreeChanges {
    WorktreeChanges {
        changed_files: changes.changed_files,
        commits: changes.commits,
    }
}

fn path_style(style: w::PathStyle) -> PathStyle {
    match style {
        w::PathStyle::Posix => PathStyle::Posix,
        w::PathStyle::Windows => PathStyle::Windows,
    }
}

/// 握手的结果 → 注册表里的一台机器（remote-exec.md §20.4）：工作目录没写时是远端的 home；两者都没有时 None。
pub fn remote_machine(
    name: &str,
    working_directory: Option<&str>,
    welcome: Welcome,
) -> Option<RemoteMachine> {
    let root = working_directory
        .map(str::to_owned)
        .or_else(|| welcome.home.clone())?;
    Some(RemoteMachine {
        name: name.to_owned(),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new(name),
            root: EnvPath::new(root),
        },
        facts: MachineFacts {
            os: welcome.os,
            arch: welcome.arch,
            shell: welcome.shell.unwrap_or_default(),
            home: welcome.home.map(EnvPath::new),
            path_style: path_style(welcome.path_style),
        },
    })
}
