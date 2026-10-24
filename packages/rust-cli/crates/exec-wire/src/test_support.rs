//! 每种请求与回复各一个样本（单测与兼容测试共用）。

use std::collections::BTreeMap;

use crate::Blob;
use crate::dto::command::{
    BackgroundCommand, BackgroundKind, CwdScope, FilesystemPolicy, HookIo, MonitorSocket,
    NetworkPolicy, SandboxPolicy, ShellKind, StartCommand, TimeoutBackground,
};
use crate::dto::core::{
    EnvFacts, OperationContext, Origin, Outcome, PathFacts, PathStyle, Workspace,
};
use crate::dto::files::{
    DirEntry, FileContent, FileKind, FileStat, FileWritten, GlobFiles, GlobMatches, GrepFileLines,
    GrepLine, GrepLineKind, GrepMode, GrepOutcome, GrepSearch, GrepSubmatch, PdfRender, PutFile,
    ReadText, RenderPdfPages, TextRead, TextSlice, WritePrecondition,
};
use crate::dto::rewind::{RestoreFile, RewindConflict, RewindPlan, RewindStatus};
use crate::dto::shell::{
    BackgroundInfo, BackgroundStatus, BackgroundStop, BackgroundTask, ShellState,
};
use crate::reply::Reply;
use crate::request::Request;

pub(crate) fn workspace() -> Workspace {
    Workspace {
        environment: "local".to_owned(),
        root: "/w".to_owned(),
    }
}

pub(crate) fn context(operation: &str) -> OperationContext {
    OperationContext {
        environment: "local".to_owned(),
        generation: "g1".to_owned(),
        owner_epoch: 1,
        operation: operation.to_owned(),
        origin: Origin::Agent {
            session: "sess_1".to_owned(),
            run: "run_1".to_owned(),
        },
    }
}

fn blob(at: u64, len: u64) -> Blob {
    Blob { at, len }
}

fn task() -> BackgroundTask {
    BackgroundTask {
        session: "sess_1".to_owned(),
        task: "b1".to_owned(),
    }
}

pub(crate) fn start_command() -> StartCommand {
    let background = BackgroundCommand {
        description: "watch".to_owned(),
        tool_use: Some("toolu_1".to_owned()),
        kind: BackgroundKind::Monitor,
    };
    StartCommand {
        command: "echo hi".to_owned(),
        cwd: "/w".to_owned(),
        timeout_ms: Some(1000),
        task: "b1".to_owned(),
        shell: ShellKind::Bash,
        cwd_scope: CwdScope::Session {
            roots: vec!["/w".to_owned()],
        },
        background: Some(background.clone()),
        sandbox: Some(SandboxPolicy {
            filesystem: FilesystemPolicy {
                allow_write: vec!["/w".to_owned()],
                deny_write: Vec::new(),
                deny_read: vec!["/w/.env".to_owned()],
            },
            network: NetworkPolicy::Domains {
                allowed: vec!["example.com".to_owned()],
                denied: Vec::new(),
            },
            required: true,
        }),
        hook: Some(HookIo {
            stdin: blob(0, 2),
            env: BTreeMap::from([("A".to_owned(), "1".to_owned())]),
        }),
        on_timeout: Some(TimeoutBackground {
            command: background,
            deadline_ms: None,
        }),
        socket: Some(MonitorSocket {
            url: "ws://127.0.0.1:1/".to_owned(),
            protocols: Vec::new(),
        }),
    }
}

/// 29 种请求各一个。
pub(crate) fn requests() -> Vec<Request> {
    let ws = workspace;
    vec![
        Request::Facts { workspace: ws() },
        Request::PathFacts { workspace: ws() },
        Request::ReadFile {
            workspace: ws(),
            path: "/w/a".to_owned(),
        },
        Request::WriteFile {
            workspace: ws(),
            context: context("op_1"),
            path: "/w/a".to_owned(),
            bytes: blob(0, 3),
            precondition: WritePrecondition::Version("v1".to_owned()),
        },
        Request::Stat {
            workspace: ws(),
            path: "/w/a".to_owned(),
        },
        Request::ListDir {
            workspace: ws(),
            path: "/w".to_owned(),
        },
        Request::ReadText {
            workspace: ws(),
            request: ReadText {
                path: "/w/a".to_owned(),
                start_line: 2,
                max_lines: Some(10),
                max_bytes: None,
            },
        },
        Request::PutFile {
            workspace: ws(),
            context: context("op_2"),
            request: PutFile {
                path: "/w/b".to_owned(),
                bytes: blob(0, 0),
                precondition: WritePrecondition::Absent,
                create_parents: true,
            },
        },
        Request::Glob {
            workspace: ws(),
            request: GlobFiles {
                root: "/w".to_owned(),
                pattern: "**/*.rs".to_owned(),
                limit: 100,
                hidden: true,
                respect_ignore: false,
            },
        },
        Request::ListFiles {
            workspace: ws(),
            cwd: "/w".to_owned(),
        },
        Request::Grep {
            workspace: ws(),
            request: GrepSearch {
                root: "/w".to_owned(),
                pattern: "fn".to_owned(),
                case_insensitive: true,
                multiline: false,
                globs: vec!["!target".to_owned()],
                file_type: Some("rust".to_owned()),
                hidden: false,
                mode: GrepMode::Content {
                    before: 1,
                    after: 2,
                },
            },
        },
        Request::StartCommand {
            workspace: ws(),
            context: context("op_3"),
            request: Box::new(start_command()),
        },
        Request::WatchCommand {
            operation: "op_3".to_owned(),
            cursor: 7,
        },
        Request::Cancel {
            operation: "op_3".to_owned(),
        },
        Request::OperationStatus {
            operation: "op_3".to_owned(),
        },
        Request::CloseSession {
            session: "sess_1".to_owned(),
            forget: true,
        },
        Request::ShellState {
            workspace: ws(),
            session: "sess_1".to_owned(),
        },
        Request::ShellNotices {
            session: "sess_1".to_owned(),
            after: 3,
        },
        Request::AckShellNotice {
            session: "sess_1".to_owned(),
            seq: 4,
        },
        Request::SetShellCwd {
            session: "sess_1".to_owned(),
            cwd: "/w/sub".to_owned(),
        },
        Request::BackgroundCommand { task: task() },
        Request::BackgroundElapsed { task: task() },
        Request::StopBackgroundCommand {
            context: context("op_4"),
            task: task(),
        },
        Request::RewindPrepare {
            workspace: ws(),
            context: context("tok_1"),
            files: vec![
                RestoreFile {
                    path: "/w/a".to_owned(),
                    expected: Some("ab".to_owned()),
                    restore: Some(blob(0, 1)),
                },
                RestoreFile {
                    path: "/w/c".to_owned(),
                    expected: None,
                    restore: None,
                },
            ],
        },
        Request::RewindApply {
            token: "tok_1".to_owned(),
        },
        Request::RewindStatus {
            token: "tok_1".to_owned(),
        },
        Request::RewindFinish {
            token: "tok_1".to_owned(),
        },
        Request::PdfPageCount {
            workspace: ws(),
            path: "/w/a.pdf".to_owned(),
        },
        Request::RenderPdfPages {
            workspace: ws(),
            request: RenderPdfPages {
                path: "/w/a.pdf".to_owned(),
                first_page: 1,
                last_page: Some(2),
            },
        },
    ]
}

/// 27 种回复各一个（两个流没有回复）。
pub(crate) fn replies() -> Vec<Reply> {
    let conflict = RewindConflict {
        path: "/w/a".to_owned(),
        current: None,
        message: Some("gone".to_owned()),
    };
    vec![
        Reply::Facts(EnvFacts {
            cwd: "/w".to_owned(),
            platform: "darwin".to_owned(),
            os_version: "Darwin 24".to_owned(),
            shell: "zsh".to_owned(),
            is_git_repo: true,
            additional_directories: vec!["/x".to_owned()],
            home: Some("/home/dev".to_owned()),
        }),
        Reply::PathFacts(PathFacts {
            home: None,
            style: PathStyle::Windows,
        }),
        Reply::ReadFile(FileContent {
            bytes: blob(0, 3),
            version: "v1".to_owned(),
        }),
        Reply::WriteFile {
            version: "v2".to_owned(),
        },
        Reply::Stat(Some(FileStat {
            kind: FileKind::Other,
            size: 3,
            mtime_ms: 9,
            version: "v1".to_owned(),
        })),
        Reply::ListDir(vec![DirEntry {
            name: "a".to_owned(),
            kind: FileKind::Directory,
        }]),
        Reply::ReadText(TextRead::Text {
            slice: TextSlice {
                content: blob(0, 2),
                line_count: 1,
                total_lines: 1,
                total_bytes: 2,
                read_bytes: 2,
                mtime_ms: 5,
                version: "v1".to_owned(),
            },
        }),
        Reply::PutFile(FileWritten {
            version: "v3".to_owned(),
            mtime_ms: 6,
            created: true,
        }),
        Reply::Glob(GlobMatches {
            files: vec!["/w/a.rs".to_owned()],
            total: 4,
        }),
        Reply::ListFiles {
            files: vec!["a.rs".to_owned()],
        },
        Reply::Grep(GrepOutcome::Content {
            files: vec![GrepFileLines {
                path: "/w/a.rs".to_owned(),
                lines: vec![GrepLine {
                    kind: GrepLineKind::Match,
                    line_number: 3,
                    text: "fn main() {}\n".to_owned(),
                    submatches: vec![GrepSubmatch {
                        start: 0,
                        text: "fn".to_owned(),
                    }],
                }],
                binary_offset: None,
            }],
        }),
        Reply::StartCommand,
        Reply::Cancel,
        Reply::OperationStatus(Outcome::Failed {
            code: Some("timeout".to_owned()),
            message: "timed out".to_owned(),
        }),
        Reply::CloseSession,
        Reply::ShellState(ShellState {
            cwd: "/w".to_owned(),
            outputs: Some("/tmp/o".to_owned()),
            background: vec!["b1".to_owned()],
        }),
        Reply::AckShellNotice,
        Reply::SetShellCwd,
        Reply::BackgroundCommand(Some(BackgroundInfo {
            command: "sleep 9".to_owned(),
            status: BackgroundStatus::Killed,
        })),
        Reply::BackgroundElapsed(None),
        Reply::StopBackgroundCommand(BackgroundStop::NotRunning {
            status: BackgroundStatus::Completed,
        }),
        Reply::RewindPrepare(RewindPlan::Conflicts {
            files: vec![conflict.clone()],
        }),
        Reply::RewindApply(RewindStatus::Failed {
            message: "late change".to_owned(),
            conflicts: vec![conflict],
        }),
        Reply::RewindStatus(RewindStatus::Unknown),
        Reply::RewindFinish,
        Reply::PdfPageCount(Some(12)),
        Reply::RenderPdfPages(PdfRender::Rendered {
            pages: vec![blob(0, 1), blob(1, 1)],
        }),
    ]
}
