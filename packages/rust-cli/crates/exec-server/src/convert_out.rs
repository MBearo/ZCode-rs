//! kernel-api 的结果 → 线上的类型（服务端的换算）。字节字段追加到二进制段。
//!
//! 结构一律穷举解构（F128，审查 X1）：kernel-api 加字段时这里编译失败，必须决定它上不上线；不上线的写成 `_` 并注明理由。

use rust_cli_exec_wire::BodyBuilder;
use rust_cli_exec_wire::dto::command as wc;
use rust_cli_exec_wire::dto::core as w;
use rust_cli_exec_wire::dto::files as wf;
use rust_cli_exec_wire::dto::git as wg;
use rust_cli_exec_wire::dto::mcp as wm;
use rust_cli_exec_wire::dto::rewind as wr;
use rust_cli_exec_wire::dto::shell as ws;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, FileContent, Outcome, OutputStream,
};
use rust_cli_kernel_api::env_files::{
    DirEntry, FileKind, FileStat, FileWritten, GlobMatches, GrepCount, GrepFileHit, GrepFileLines,
    GrepLine, GrepLineKind, GrepOutcome, GrepSubmatch, PathFacts, PathStyle, TextRead, TextSlice,
};
use rust_cli_kernel_api::env_git::{GitDiff, GitSnapshot, GitStatus};
use rust_cli_kernel_api::env_pdf::PdfRender;
use rust_cli_kernel_api::env_rewind::{
    RestoreAction, RestoredFile, RewindConflict, RewindPlan, RewindStatus,
};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundKind, BackgroundStatus, BackgroundStop, ShellNotice, ShellState,
};
use rust_cli_kernel_api::env_worktree::{
    WorktreeChanges, WorktreeCreated, WorktreeEntry, WorktreeList,
};
use rust_cli_kernel_api::mcp::{McpError, McpTransportEvent};

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
    let EnvFacts {
        cwd,
        platform,
        os_version,
        shell,
        is_git_repo,
        home,
        uid,
    } = facts;
    w::EnvFacts {
        cwd: cwd.as_str().to_owned(),
        platform,
        os_version,
        shell,
        is_git_repo,
        home: home.map(|home| home.as_str().to_owned()),
        uid,
    }
}

pub(crate) fn path_style(style: PathStyle) -> w::PathStyle {
    match style {
        PathStyle::Posix => w::PathStyle::Posix,
        PathStyle::Windows => w::PathStyle::Windows,
    }
}

pub(crate) fn path_facts(facts: PathFacts) -> w::PathFacts {
    let PathFacts { home, style } = facts;
    w::PathFacts {
        home: home.map(|home| home.as_str().to_owned()),
        style: path_style(style),
    }
}

pub(crate) fn file_content(content: FileContent, body: &mut BodyBuilder) -> wf::FileContent {
    let FileContent { bytes, version } = content;
    wf::FileContent {
        bytes: body.push(&bytes),
        version: version.as_str().to_owned(),
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
    let FileStat {
        kind: file_kind,
        size,
        mtime_ms,
        version,
    } = stat;
    wf::FileStat {
        kind: kind(file_kind),
        size,
        mtime_ms,
        version: version.as_str().to_owned(),
    }
}

pub(crate) fn dir_entry(entry: DirEntry) -> wf::DirEntry {
    let DirEntry {
        name,
        kind: entry_kind,
    } = entry;
    wf::DirEntry {
        name,
        kind: kind(entry_kind),
    }
}

pub(crate) fn text_read(read: TextRead, body: &mut BodyBuilder) -> wf::TextRead {
    match read {
        TextRead::Text(TextSlice {
            content,
            line_count,
            total_lines,
            total_bytes,
            read_bytes,
            mtime_ms,
            version,
        }) => wf::TextRead::Text {
            slice: wf::TextSlice {
                content: body.push(content.as_bytes()),
                line_count,
                total_lines,
                total_bytes,
                read_bytes,
                mtime_ms,
                version: version.as_str().to_owned(),
            },
        },
        TextRead::Directory => wf::TextRead::Directory,
        TextRead::TooLarge { size } => wf::TextRead::TooLarge { size },
    }
}

pub(crate) fn file_written(written: FileWritten) -> wf::FileWritten {
    let FileWritten {
        version,
        mtime_ms,
        created,
    } = written;
    wf::FileWritten {
        version: version.as_str().to_owned(),
        mtime_ms,
        created,
    }
}

pub(crate) fn glob_matches(matches: GlobMatches) -> wf::GlobMatches {
    let GlobMatches { files, total } = matches;
    wf::GlobMatches {
        files: files
            .into_iter()
            .map(|file| file.as_str().to_owned())
            .collect(),
        total: total as u64,
    }
}

fn grep_hit(hit: GrepFileHit) -> wf::GrepFileHit {
    let GrepFileHit { path, mtime_ms } = hit;
    wf::GrepFileHit {
        path: path.as_str().to_owned(),
        mtime_ms,
    }
}

fn grep_count(count: GrepCount) -> wf::GrepCount {
    let GrepCount { path, count } = count;
    wf::GrepCount {
        path: path.as_str().to_owned(),
        count,
    }
}

fn grep_line(line: GrepLine) -> wf::GrepLine {
    let GrepLine {
        kind,
        line_number,
        text,
        submatches,
    } = line;
    wf::GrepLine {
        kind: match kind {
            GrepLineKind::Match => wf::GrepLineKind::Match,
            GrepLineKind::Context => wf::GrepLineKind::Context,
        },
        line_number,
        text,
        submatches: submatches
            .into_iter()
            .map(|GrepSubmatch { start, text }| wf::GrepSubmatch { start, text })
            .collect(),
    }
}

fn grep_lines(file: GrepFileLines) -> wf::GrepFileLines {
    let GrepFileLines {
        path,
        lines,
        binary_offset,
    } = file;
    wf::GrepFileLines {
        path: path.as_str().to_owned(),
        lines: lines.into_iter().map(grep_line).collect(),
        binary_offset,
    }
}

pub(crate) fn grep_outcome(outcome: GrepOutcome) -> wf::GrepOutcome {
    match outcome {
        GrepOutcome::Files { files } => wf::GrepOutcome::Files {
            files: files.into_iter().map(grep_hit).collect(),
        },
        GrepOutcome::Counts { counts } => wf::GrepOutcome::Counts {
            counts: counts.into_iter().map(grep_count).collect(),
        },
        GrepOutcome::Content { files } => wf::GrepOutcome::Content {
            files: files.into_iter().map(grep_lines).collect(),
        },
        GrepOutcome::Rejected { message } => wf::GrepOutcome::Rejected { message },
    }
}

pub(crate) fn shell_state(state: ShellState) -> ws::ShellState {
    let ShellState {
        cwd,
        outputs,
        background,
    } = state;
    ws::ShellState {
        cwd: cwd.as_str().to_owned(),
        outputs: outputs.map(|outputs| outputs.as_str().to_owned()),
        background: background
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
    let BackgroundInfo {
        command,
        status,
        kind,
        description,
    } = info;
    ws::BackgroundInfo {
        command,
        status: background_status(status),
        kind: background_kind(kind),
        description,
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
    let RewindConflict {
        path,
        current,
        message,
    } = conflict;
    wr::RewindConflict {
        path: path.as_str().to_owned(),
        current,
        message,
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
                .map(|RestoredFile { path, action }| wr::RestoredFile {
                    path: path.as_str().to_owned(),
                    action: match action {
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
        // 线上没有原因（远端不支持按要求转后台，tui-input.md D31）：照超时转后台发。
        CommandEvent::Backgrounded | CommandEvent::BackgroundedBy { reason: _ } => {
            wc::CommandEvent::Backgrounded
        }
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
    let ShellNotice {
        seq,
        operation,
        task,
        tool_use,
        description,
        output,
        outcome: ended,
        exit_code,
        kind,
        output_bytes,
    } = notice;
    ws::ShellNotice {
        seq,
        operation: operation.as_str().to_owned(),
        task: task.as_str().to_owned(),
        tool_use: tool_use.map(|tool| tool.as_str().to_owned()),
        description,
        output: output.as_str().to_owned(),
        outcome: outcome(ended),
        exit_code,
        kind: background_kind(kind),
        output_bytes,
    }
}

/// 执行器的 `McpError` → 线上的错误（§22.4）：`Connect` 与 `Io` 是 `io`，`Closed` 是 `not_found`，原文不变。
pub(crate) fn mcp_error(error: McpError) -> WireError {
    match error {
        McpError::Connect { message } | McpError::Io { message } => WireError::Io { message },
        McpError::Closed { message } => WireError::NotFound { path: message },
        McpError::Unsupported { what } => WireError::Unsupported { what },
    }
}

pub(crate) fn mcp_event(event: McpTransportEvent) -> wm::McpEvent {
    match event {
        McpTransportEvent::Message { message } => wm::McpEvent::Message { message },
        McpTransportEvent::Closed {
            reason,
            session_expired,
        } => wm::McpEvent::Closed {
            reason,
            session_expired,
        },
    }
}

pub(crate) fn git_status(status: GitStatus) -> wg::GitStatus {
    match status {
        GitStatus::NotRepository => wg::GitStatus::NotRepository,
        GitStatus::Repository(GitSnapshot {
            branch,
            default_branch,
            status,
            recent_commits,
            user_name,
        }) => wg::GitStatus::Repository(wg::GitSnapshot {
            branch,
            default_branch,
            status,
            recent_commits,
            user_name,
        }),
    }
}

pub(crate) fn git_diff(diff: GitDiff) -> wg::GitDiff {
    match diff {
        GitDiff::NotRepository => wg::GitDiff::NotRepository,
        GitDiff::Repository {
            numstat,
            patch,
            untracked,
        } => wg::GitDiff::Repository {
            numstat,
            patch,
            untracked,
        },
    }
}

pub(crate) fn worktree_created(created: WorktreeCreated) -> wg::WorktreeCreated {
    match created {
        WorktreeCreated::Ready {
            path,
            branch,
            head_commit,
            existed,
        } => wg::WorktreeCreated::Ready {
            path: path.as_str().to_owned(),
            branch,
            head_commit,
            existed,
        },
        WorktreeCreated::NotARepository => wg::WorktreeCreated::NotARepository,
        WorktreeCreated::Failed { message } => wg::WorktreeCreated::Failed { message },
    }
}

pub(crate) fn worktree_list(list: WorktreeList) -> wg::WorktreeList {
    let WorktreeList { root, entries } = list;
    wg::WorktreeList {
        root: root.map(|root| root.as_str().to_owned()),
        entries: entries
            .into_iter()
            .map(|WorktreeEntry { path, branch }| wg::WorktreeEntry {
                path: path.as_str().to_owned(),
                branch,
            })
            .collect(),
    }
}

pub(crate) fn worktree_changes(changes: WorktreeChanges) -> wg::WorktreeChanges {
    let WorktreeChanges {
        changed_files,
        commits,
    } = changes;
    wg::WorktreeChanges {
        changed_files,
        commits,
    }
}
