//! 回复：每个非流的方法一种（docs/specs/rust-cli/remote-exec.md §18.3）。JSON 是 `{"op": "端口.方法", "value": …}`，
//! 没有结果的方法只有 `op`。

use serde::{Deserialize, Serialize};

use crate::dto::core::{EnvFacts, Outcome, PathFacts};
use crate::dto::files::{
    DirEntry, FileContent, FileStat, FileWritten, GlobMatches, GrepOutcome, PdfRender, TextRead,
};
use crate::dto::rewind::{RewindPlan, RewindStatus};
use crate::dto::shell::{BackgroundInfo, BackgroundStop, ShellState};
use crate::op::Op;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "value")]
pub enum Reply {
    #[serde(rename = "info.facts")]
    Facts(EnvFacts),
    #[serde(rename = "info.path_facts")]
    PathFacts(PathFacts),
    #[serde(rename = "files.read_file")]
    ReadFile(FileContent),
    /// 写入后的版本（kernel-api `WriteReceipt`）。
    #[serde(rename = "files.write_file")]
    WriteFile { version: String },
    #[serde(rename = "files.stat")]
    Stat(Option<FileStat>),
    #[serde(rename = "files.list_dir")]
    ListDir(Vec<DirEntry>),
    #[serde(rename = "files.read_text")]
    ReadText(TextRead),
    #[serde(rename = "files.put_file")]
    PutFile(FileWritten),
    #[serde(rename = "search.glob")]
    Glob(GlobMatches),
    /// 相对起点目录的文件路径（kernel-api `FileList`）。
    #[serde(rename = "search.list_files")]
    ListFiles { files: Vec<String> },
    #[serde(rename = "search.grep")]
    Grep(GrepOutcome),
    #[serde(rename = "commands.start_command")]
    StartCommand,
    #[serde(rename = "commands.cancel")]
    Cancel,
    #[serde(rename = "commands.operation_status")]
    OperationStatus(Outcome),
    #[serde(rename = "commands.close_session")]
    CloseSession,
    #[serde(rename = "shell.shell_state")]
    ShellState(ShellState),
    #[serde(rename = "shell.ack_shell_notice")]
    AckShellNotice,
    #[serde(rename = "shell.set_shell_cwd")]
    SetShellCwd,
    #[serde(rename = "background.background_command")]
    BackgroundCommand(Option<BackgroundInfo>),
    #[serde(rename = "background.background_elapsed")]
    BackgroundElapsed(Option<u64>),
    #[serde(rename = "background.stop_background_command")]
    StopBackgroundCommand(BackgroundStop),
    #[serde(rename = "checkpoints.rewind_prepare")]
    RewindPrepare(RewindPlan),
    #[serde(rename = "checkpoints.rewind_apply")]
    RewindApply(RewindStatus),
    #[serde(rename = "checkpoints.rewind_status")]
    RewindStatus(RewindStatus),
    #[serde(rename = "checkpoints.rewind_finish")]
    RewindFinish,
    #[serde(rename = "pdf.pdf_page_count")]
    PdfPageCount(Option<u64>),
    #[serde(rename = "pdf.render_pdf_pages")]
    RenderPdfPages(PdfRender),
}

impl Reply {
    /// 这个回复属于哪种消息；流（`watch_command`、`shell_notices`）没有回复。
    pub fn op(&self) -> Op {
        match self {
            Self::Facts(_) => Op::Facts,
            Self::PathFacts(_) => Op::PathFacts,
            Self::ReadFile(_) => Op::ReadFile,
            Self::WriteFile { .. } => Op::WriteFile,
            Self::Stat(_) => Op::Stat,
            Self::ListDir(_) => Op::ListDir,
            Self::ReadText(_) => Op::ReadText,
            Self::PutFile(_) => Op::PutFile,
            Self::Glob(_) => Op::Glob,
            Self::ListFiles { .. } => Op::ListFiles,
            Self::Grep(_) => Op::Grep,
            Self::StartCommand => Op::StartCommand,
            Self::Cancel => Op::Cancel,
            Self::OperationStatus(_) => Op::OperationStatus,
            Self::CloseSession => Op::CloseSession,
            Self::ShellState(_) => Op::ShellState,
            Self::AckShellNotice => Op::AckShellNotice,
            Self::SetShellCwd => Op::SetShellCwd,
            Self::BackgroundCommand(_) => Op::BackgroundCommand,
            Self::BackgroundElapsed(_) => Op::BackgroundElapsed,
            Self::StopBackgroundCommand(_) => Op::StopBackgroundCommand,
            Self::RewindPrepare(_) => Op::RewindPrepare,
            Self::RewindApply(_) => Op::RewindApply,
            Self::RewindStatus(_) => Op::RewindStatus,
            Self::RewindFinish => Op::RewindFinish,
            Self::PdfPageCount(_) => Op::PdfPageCount,
            Self::RenderPdfPages(_) => Op::RenderPdfPages,
        }
    }
}
