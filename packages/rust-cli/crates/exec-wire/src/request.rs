//! 请求：`MachineEnv` 的每个方法一种（docs/specs/rust-cli/remote-exec.md §18.3）。
//!
//! JSON 是 `{"op": "端口.方法", "args": {…}}`。参数里的路径与 ID 都是字符串，字节字段在二进制段。

use serde::{Deserialize, Serialize};

use crate::Blob;
use crate::dto::command::StartCommand;
use crate::dto::core::{OperationContext, Workspace};
use crate::dto::files::{
    GlobFiles, GrepSearch, PutFile, ReadText, RenderPdfPages, WritePrecondition,
};
use crate::dto::rewind::RestoreFile;
use crate::dto::shell::BackgroundTask;
use crate::op::Op;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "args")]
pub enum Request {
    #[serde(rename = "info.facts")]
    Facts { workspace: Workspace },
    #[serde(rename = "info.path_facts")]
    PathFacts { workspace: Workspace },
    #[serde(rename = "files.read_file")]
    ReadFile { workspace: Workspace, path: String },
    #[serde(rename = "files.write_file")]
    WriteFile {
        workspace: Workspace,
        context: OperationContext,
        path: String,
        bytes: Blob,
        precondition: WritePrecondition,
    },
    #[serde(rename = "files.stat")]
    Stat { workspace: Workspace, path: String },
    #[serde(rename = "files.list_dir")]
    ListDir { workspace: Workspace, path: String },
    #[serde(rename = "files.read_text")]
    ReadText {
        workspace: Workspace,
        request: ReadText,
    },
    #[serde(rename = "files.put_file")]
    PutFile {
        workspace: Workspace,
        context: OperationContext,
        request: PutFile,
    },
    #[serde(rename = "search.glob")]
    Glob {
        workspace: Workspace,
        request: GlobFiles,
    },
    #[serde(rename = "search.list_files")]
    ListFiles { workspace: Workspace, cwd: String },
    #[serde(rename = "search.grep")]
    Grep {
        workspace: Workspace,
        request: GrepSearch,
    },
    #[serde(rename = "commands.start_command")]
    StartCommand {
        workspace: Workspace,
        context: OperationContext,
        request: Box<StartCommand>,
    },
    #[serde(rename = "commands.watch_command")]
    WatchCommand { operation: String, cursor: u64 },
    #[serde(rename = "commands.cancel")]
    Cancel { operation: String },
    #[serde(rename = "commands.operation_status")]
    OperationStatus { operation: String },
    #[serde(rename = "commands.close_session")]
    CloseSession { session: String, forget: bool },
    #[serde(rename = "shell.shell_state")]
    ShellState {
        workspace: Workspace,
        session: String,
    },
    #[serde(rename = "shell.shell_notices")]
    ShellNotices { session: String, after: u64 },
    #[serde(rename = "shell.ack_shell_notice")]
    AckShellNotice { session: String, seq: u64 },
    #[serde(rename = "shell.set_shell_cwd")]
    SetShellCwd { session: String, cwd: String },
    #[serde(rename = "background.background_command")]
    BackgroundCommand { task: BackgroundTask },
    #[serde(rename = "background.background_elapsed")]
    BackgroundElapsed { task: BackgroundTask },
    #[serde(rename = "background.stop_background_command")]
    StopBackgroundCommand {
        context: OperationContext,
        task: BackgroundTask,
    },
    #[serde(rename = "checkpoints.rewind_prepare")]
    RewindPrepare {
        workspace: Workspace,
        context: OperationContext,
        files: Vec<RestoreFile>,
    },
    #[serde(rename = "checkpoints.rewind_apply")]
    RewindApply { token: String },
    #[serde(rename = "checkpoints.rewind_status")]
    RewindStatus { token: String },
    #[serde(rename = "checkpoints.rewind_finish")]
    RewindFinish { token: String },
    #[serde(rename = "pdf.pdf_page_count")]
    PdfPageCount { workspace: Workspace, path: String },
    #[serde(rename = "pdf.render_pdf_pages")]
    RenderPdfPages {
        workspace: Workspace,
        request: RenderPdfPages,
    },
}

impl Request {
    pub fn op(&self) -> Op {
        match self {
            Self::Facts { .. } => Op::Facts,
            Self::PathFacts { .. } => Op::PathFacts,
            Self::ReadFile { .. } => Op::ReadFile,
            Self::WriteFile { .. } => Op::WriteFile,
            Self::Stat { .. } => Op::Stat,
            Self::ListDir { .. } => Op::ListDir,
            Self::ReadText { .. } => Op::ReadText,
            Self::PutFile { .. } => Op::PutFile,
            Self::Glob { .. } => Op::Glob,
            Self::ListFiles { .. } => Op::ListFiles,
            Self::Grep { .. } => Op::Grep,
            Self::StartCommand { .. } => Op::StartCommand,
            Self::WatchCommand { .. } => Op::WatchCommand,
            Self::Cancel { .. } => Op::Cancel,
            Self::OperationStatus { .. } => Op::OperationStatus,
            Self::CloseSession { .. } => Op::CloseSession,
            Self::ShellState { .. } => Op::ShellState,
            Self::ShellNotices { .. } => Op::ShellNotices,
            Self::AckShellNotice { .. } => Op::AckShellNotice,
            Self::SetShellCwd { .. } => Op::SetShellCwd,
            Self::BackgroundCommand { .. } => Op::BackgroundCommand,
            Self::BackgroundElapsed { .. } => Op::BackgroundElapsed,
            Self::StopBackgroundCommand { .. } => Op::StopBackgroundCommand,
            Self::RewindPrepare { .. } => Op::RewindPrepare,
            Self::RewindApply { .. } => Op::RewindApply,
            Self::RewindStatus { .. } => Op::RewindStatus,
            Self::RewindFinish { .. } => Op::RewindFinish,
            Self::PdfPageCount { .. } => Op::PdfPageCount,
            Self::RenderPdfPages { .. } => Op::RenderPdfPages,
        }
    }
}
