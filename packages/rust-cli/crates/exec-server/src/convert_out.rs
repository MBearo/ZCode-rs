//! kernel-api 的结果 → 线上的类型（服务端的换算）。字节字段追加到二进制段。

use rust_cli_exec_wire::BodyBuilder;
use rust_cli_exec_wire::dto::command as wc;
use rust_cli_exec_wire::dto::core as w;
use rust_cli_exec_wire::dto::files as wf;
use rust_cli_exec_wire::dto::rewind as wr;
use rust_cli_exec_wire::dto::shell as ws;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, FileContent, Outcome, OutputStream,
};
use rust_cli_kernel_api::env_files::{
    DirEntry, FileKind, FileStat, FileWritten, GlobMatches, GrepLineKind, GrepOutcome, PathFacts,
    PathStyle, TextRead,
};
use rust_cli_kernel_api::env_pdf::PdfRender;
use rust_cli_kernel_api::env_rewind::{RestoreAction, RewindConflict, RewindPlan, RewindStatus};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundKind, BackgroundStatus, BackgroundStop, ShellNotice, ShellState,
};

pub(crate) fn error(error: EnvError) -> WireError {
    match error {
        EnvError::NotFound { path } => WireError::NotFound { path },
        EnvError::PreconditionFailed { path } => WireError::PreconditionFailed { path },
        EnvError::PermissionDenied { message } => WireError::PermissionDenied { message },
        EnvError::Stale { message } => WireError::Stale { message },
        EnvError::OperationConflict { operation } => WireError::OperationConflict {
            operation: operation.as_str().to_owned(),
        },
        EnvError::Io { message } => WireError::Io { message },
        EnvError::Unsupported { what } => WireError::Unsupported { what },
    }
}

pub(crate) fn outcome(outcome: Outcome) -> w::Outcome {
    match outcome {
        Outcome::Completed => w::Outcome::Completed,
        Outcome::Failed { code, message } => w::Outcome::Failed { code, message },
        Outcome::Rejected { message } => w::Outcome::Rejected { message },
        Outcome::Running => w::Outcome::Running,
        Outcome::Unknown => w::Outcome::Unknown,
        Outcome::Interrupted => w::Outcome::Interrupted,
        Outcome::Cancelled => w::Outcome::Cancelled,
    }
}

pub(crate) fn facts(facts: EnvFacts) -> w::EnvFacts {
    w::EnvFacts {
        cwd: facts.cwd.as_str().to_owned(),
        platform: facts.platform,
        os_version: facts.os_version,
        shell: facts.shell,
        is_git_repo: facts.is_git_repo,
        additional_directories: facts
            .additional_directories
            .into_iter()
            .map(|dir| dir.as_str().to_owned())
            .collect(),
        home: facts.home.map(|home| home.as_str().to_owned()),
    }
}

pub(crate) fn path_style(style: PathStyle) -> w::PathStyle {
    match style {
        PathStyle::Posix => w::PathStyle::Posix,
        PathStyle::Windows => w::PathStyle::Windows,
    }
}

pub(crate) fn path_facts(facts: PathFacts) -> w::PathFacts {
    w::PathFacts {
        home: facts.home.map(|home| home.as_str().to_owned()),
        style: path_style(facts.style),
    }
}

pub(crate) fn file_content(content: FileContent, body: &mut BodyBuilder) -> wf::FileContent {
    wf::FileContent {
        bytes: body.push(&content.bytes),
        version: content.version.as_str().to_owned(),
    }
}

fn kind(kind: FileKind) -> wf::FileKind {
    match kind {
        FileKind::File => wf::FileKind::File,
        FileKind::Directory => wf::FileKind::Directory,
        FileKind::Other => wf::FileKind::Other,
    }
}

/// `files.stat_many` 的一项（R3）。
pub(crate) fn stat_entry(found: Result<Option<FileStat>, EnvError>) -> wf::StatEntry {
    match found {
        Ok(stat) => wf::StatEntry {
            stat: stat.map(file_stat),
            error: None,
        },
        Err(failure) => wf::StatEntry {
            stat: None,
            error: Some(error(failure)),
        },
    }
}

pub(crate) fn file_stat(stat: FileStat) -> wf::FileStat {
    wf::FileStat {
        kind: kind(stat.kind),
        size: stat.size,
        mtime_ms: stat.mtime_ms,
        version: stat.version.as_str().to_owned(),
    }
}

pub(crate) fn dir_entry(entry: DirEntry) -> wf::DirEntry {
    wf::DirEntry {
        name: entry.name,
        kind: kind(entry.kind),
    }
}

pub(crate) fn text_read(read: TextRead, body: &mut BodyBuilder) -> wf::TextRead {
    match read {
        TextRead::Text(slice) => wf::TextRead::Text {
            slice: wf::TextSlice {
                content: body.push(slice.content.as_bytes()),
                line_count: slice.line_count,
                total_lines: slice.total_lines,
                total_bytes: slice.total_bytes,
                read_bytes: slice.read_bytes,
                mtime_ms: slice.mtime_ms,
                version: slice.version.as_str().to_owned(),
            },
        },
        TextRead::Directory => wf::TextRead::Directory,
        TextRead::TooLarge { size } => wf::TextRead::TooLarge { size },
    }
}

pub(crate) fn file_written(written: FileWritten) -> wf::FileWritten {
    wf::FileWritten {
        version: written.version.as_str().to_owned(),
        mtime_ms: written.mtime_ms,
        created: written.created,
    }
}

pub(crate) fn glob_matches(matches: GlobMatches) -> wf::GlobMatches {
    wf::GlobMatches {
        files: matches
            .files
            .into_iter()
            .map(|file| file.as_str().to_owned())
            .collect(),
        total: matches.total as u64,
    }
}

pub(crate) fn grep_outcome(outcome: GrepOutcome) -> wf::GrepOutcome {
    match outcome {
        GrepOutcome::Files { files } => wf::GrepOutcome::Files {
            files: files
                .into_iter()
                .map(|hit| wf::GrepFileHit {
                    path: hit.path.as_str().to_owned(),
                    mtime_ms: hit.mtime_ms,
                })
                .collect(),
        },
        GrepOutcome::Counts { counts } => wf::GrepOutcome::Counts {
            counts: counts
                .into_iter()
                .map(|count| wf::GrepCount {
                    path: count.path.as_str().to_owned(),
                    count: count.count,
                })
                .collect(),
        },
        GrepOutcome::Content { files } => wf::GrepOutcome::Content {
            files: files
                .into_iter()
                .map(|file| wf::GrepFileLines {
                    path: file.path.as_str().to_owned(),
                    lines: file
                        .lines
                        .into_iter()
                        .map(|line| wf::GrepLine {
                            kind: match line.kind {
                                GrepLineKind::Match => wf::GrepLineKind::Match,
                                GrepLineKind::Context => wf::GrepLineKind::Context,
                            },
                            line_number: line.line_number,
                            text: line.text,
                            submatches: line
                                .submatches
                                .into_iter()
                                .map(|sub| wf::GrepSubmatch {
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
        GrepOutcome::Rejected { message } => wf::GrepOutcome::Rejected { message },
    }
}

pub(crate) fn shell_state(state: ShellState) -> ws::ShellState {
    ws::ShellState {
        cwd: state.cwd.as_str().to_owned(),
        outputs: state.outputs.map(|outputs| outputs.as_str().to_owned()),
        background: state
            .background
            .into_iter()
            .map(|task| task.as_str().to_owned())
            .collect(),
    }
}

fn background_kind(kind: BackgroundKind) -> wc::BackgroundKind {
    match kind {
        BackgroundKind::Bash => wc::BackgroundKind::Bash,
        BackgroundKind::Monitor => wc::BackgroundKind::Monitor,
    }
}

fn background_status(status: BackgroundStatus) -> ws::BackgroundStatus {
    match status {
        BackgroundStatus::Running => ws::BackgroundStatus::Running,
        BackgroundStatus::Completed => ws::BackgroundStatus::Completed,
        BackgroundStatus::Failed => ws::BackgroundStatus::Failed,
        BackgroundStatus::Killed => ws::BackgroundStatus::Killed,
    }
}

pub(crate) fn background_info(info: BackgroundInfo) -> ws::BackgroundInfo {
    ws::BackgroundInfo {
        command: info.command,
        status: background_status(info.status),
    }
}

pub(crate) fn background_stop(stop: BackgroundStop) -> ws::BackgroundStop {
    match stop {
        BackgroundStop::Stopped { command } => ws::BackgroundStop::Stopped { command },
        BackgroundStop::NotRunning { status } => ws::BackgroundStop::NotRunning {
            status: background_status(status),
        },
        BackgroundStop::NotFound => ws::BackgroundStop::NotFound,
    }
}

fn conflict(conflict: RewindConflict) -> wr::RewindConflict {
    wr::RewindConflict {
        path: conflict.path.as_str().to_owned(),
        current: conflict.current,
        message: conflict.message,
    }
}

pub(crate) fn rewind_plan(plan: RewindPlan) -> wr::RewindPlan {
    match plan {
        RewindPlan::Ready => wr::RewindPlan::Ready,
        RewindPlan::Conflicts { files } => wr::RewindPlan::Conflicts {
            files: files.into_iter().map(conflict).collect(),
        },
    }
}

pub(crate) fn rewind_status(status: RewindStatus) -> wr::RewindStatus {
    match status {
        RewindStatus::Prepared => wr::RewindStatus::Prepared,
        RewindStatus::Applying => wr::RewindStatus::Applying,
        RewindStatus::Applied { files } => wr::RewindStatus::Applied {
            files: files
                .into_iter()
                .map(|file| wr::RestoredFile {
                    path: file.path.as_str().to_owned(),
                    action: match file.action {
                        RestoreAction::Restore => wr::RestoreAction::Restore,
                        RestoreAction::Delete => wr::RestoreAction::Delete,
                    },
                })
                .collect(),
        },
        RewindStatus::Failed { message, conflicts } => wr::RewindStatus::Failed {
            message,
            conflicts: conflicts.into_iter().map(conflict).collect(),
        },
        RewindStatus::Unknown => wr::RewindStatus::Unknown,
    }
}

pub(crate) fn pdf_render(render: PdfRender, body: &mut BodyBuilder) -> wf::PdfRender {
    match render {
        PdfRender::Unavailable => wf::PdfRender::Unavailable,
        PdfRender::Failed { exit_code, stderr } => wf::PdfRender::Failed { exit_code, stderr },
        PdfRender::Rendered { pages } => wf::PdfRender::Rendered {
            pages: pages.iter().map(|page| body.push(page)).collect(),
        },
    }
}

pub(crate) fn command_event(event: CommandEvent, body: &mut BodyBuilder) -> wc::CommandEvent {
    match event {
        CommandEvent::Started { output } => wc::CommandEvent::Started {
            output: output.as_str().to_owned(),
        },
        CommandEvent::Output {
            stream,
            data,
            cursor,
        } => wc::CommandEvent::Output {
            stream: match stream {
                OutputStream::Stdout => wc::OutputStream::Stdout,
                OutputStream::Stderr => wc::OutputStream::Stderr,
            },
            data: body.push(&data),
            cursor,
        },
        CommandEvent::Backgrounded => wc::CommandEvent::Backgrounded,
        CommandEvent::Finished {
            outcome: ended,
            exit_code,
            timed_out,
            cwd_reset,
            output_bytes,
            output_limit,
        } => wc::CommandEvent::Finished {
            outcome: outcome(ended),
            exit_code,
            timed_out,
            cwd_reset: cwd_reset.map(|cwd| cwd.as_str().to_owned()),
            output_bytes,
            output_limit,
        },
    }
}

pub(crate) fn shell_notice(notice: ShellNotice) -> ws::ShellNotice {
    ws::ShellNotice {
        seq: notice.seq,
        operation: notice.operation.as_str().to_owned(),
        task: notice.task.as_str().to_owned(),
        tool_use: notice.tool_use.map(|tool| tool.as_str().to_owned()),
        description: notice.description,
        output: notice.output.as_str().to_owned(),
        outcome: outcome(notice.outcome),
        exit_code: notice.exit_code,
        kind: background_kind(notice.kind),
        output_bytes: notice.output_bytes,
    }
}
