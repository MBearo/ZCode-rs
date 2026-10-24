//! 线上的结果 → kernel-api 的类型（客户端的换算）。字节字段从二进制段取；越界是 `io` 错误。

use rust_cli_exec_client::ClientError;
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::command as wc;
use rust_cli_exec_wire::dto::core as w;
use rust_cli_exec_wire::dto::files as wf;
use rust_cli_exec_wire::dto::rewind as wr;
use rust_cli_exec_wire::dto::shell as ws;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, FileVersion, Outcome, OutputStream,
};
use rust_cli_kernel_api::env_files::{
    DirEntry, FileKind, FileStat, FileWritten, GlobMatches, GrepCount, GrepFileHit, GrepFileLines,
    GrepLine, GrepLineKind, GrepOutcome, GrepSubmatch, PathFacts, PathStyle, TextRead, TextSlice,
};
use rust_cli_kernel_api::env_pdf::PdfRender;
use rust_cli_kernel_api::env_rewind::{
    RestoreAction, RestoredFile, RewindConflict, RewindPlan, RewindStatus,
};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundKind, BackgroundStatus, BackgroundStop, ShellNotice, ShellState,
};
use rust_cli_kernel_api::ids::{OperationId, ShellTaskId, ToolUseId};

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

/// 连接层的错误 → `EnvError`：执行器没有这个端口是 `Unsupported`，其余是 `Io`。
pub(crate) fn client_error(error: ClientError) -> EnvError {
    match error {
        ClientError::Unsupported { what } => EnvError::Unsupported { what },
        other => EnvError::Io {
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
        additional_directories: facts.additional_directories.into_iter().map(path).collect(),
        home: facts.home.map(path),
    }
}

pub(crate) fn path_facts(facts: w::PathFacts) -> PathFacts {
    PathFacts {
        home: facts.home.map(path),
        style: match facts.style {
            w::PathStyle::Posix => PathStyle::Posix,
            w::PathStyle::Windows => PathStyle::Windows,
        },
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
