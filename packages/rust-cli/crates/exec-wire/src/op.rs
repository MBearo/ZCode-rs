//! 每种消息的名字、所属端口（能力位）、是不是流、有没有副作用（docs/specs/rust-cli/remote-exec.md §18.3、§18.5）。

use crate::handshake::Capability;

/// `MachineEnv` 的 30 个方法，每个一种消息（R3 加了 `files.stat_many`，remote-exec.md §20.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    Facts,
    PathFacts,
    ReadFile,
    WriteFile,
    Stat,
    StatMany,
    ListDir,
    ReadText,
    PutFile,
    Glob,
    ListFiles,
    Grep,
    StartCommand,
    WatchCommand,
    Cancel,
    OperationStatus,
    CloseSession,
    ShellState,
    ShellNotices,
    AckShellNotice,
    SetShellCwd,
    BackgroundCommand,
    BackgroundElapsed,
    StopBackgroundCommand,
    RewindPrepare,
    RewindApply,
    RewindStatus,
    RewindFinish,
    PdfPageCount,
    RenderPdfPages,
}

impl Op {
    pub const ALL: [Self; 30] = [
        Self::Facts,
        Self::PathFacts,
        Self::ReadFile,
        Self::WriteFile,
        Self::Stat,
        Self::StatMany,
        Self::ListDir,
        Self::ReadText,
        Self::PutFile,
        Self::Glob,
        Self::ListFiles,
        Self::Grep,
        Self::StartCommand,
        Self::WatchCommand,
        Self::Cancel,
        Self::OperationStatus,
        Self::CloseSession,
        Self::ShellState,
        Self::ShellNotices,
        Self::AckShellNotice,
        Self::SetShellCwd,
        Self::BackgroundCommand,
        Self::BackgroundElapsed,
        Self::StopBackgroundCommand,
        Self::RewindPrepare,
        Self::RewindApply,
        Self::RewindStatus,
        Self::RewindFinish,
        Self::PdfPageCount,
        Self::RenderPdfPages,
    ];

    /// 线上的名字（“端口.方法”），与请求、回复里的 `op` 相同。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Facts => "info.facts",
            Self::PathFacts => "info.path_facts",
            Self::ReadFile => "files.read_file",
            Self::WriteFile => "files.write_file",
            Self::Stat => "files.stat",
            Self::StatMany => "files.stat_many",
            Self::ListDir => "files.list_dir",
            Self::ReadText => "files.read_text",
            Self::PutFile => "files.put_file",
            Self::Glob => "search.glob",
            Self::ListFiles => "search.list_files",
            Self::Grep => "search.grep",
            Self::StartCommand => "commands.start_command",
            Self::WatchCommand => "commands.watch_command",
            Self::Cancel => "commands.cancel",
            Self::OperationStatus => "commands.operation_status",
            Self::CloseSession => "commands.close_session",
            Self::ShellState => "shell.shell_state",
            Self::ShellNotices => "shell.shell_notices",
            Self::AckShellNotice => "shell.ack_shell_notice",
            Self::SetShellCwd => "shell.set_shell_cwd",
            Self::BackgroundCommand => "background.background_command",
            Self::BackgroundElapsed => "background.background_elapsed",
            Self::StopBackgroundCommand => "background.stop_background_command",
            Self::RewindPrepare => "checkpoints.rewind_prepare",
            Self::RewindApply => "checkpoints.rewind_apply",
            Self::RewindStatus => "checkpoints.rewind_status",
            Self::RewindFinish => "checkpoints.rewind_finish",
            Self::PdfPageCount => "pdf.pdf_page_count",
            Self::RenderPdfPages => "pdf.render_pdf_pages",
        }
    }

    /// 按线上的名字找；不认识（新客户端的新消息）时 None。
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.as_str() == name)
    }

    pub fn capability(self) -> Capability {
        match self {
            Self::Facts | Self::PathFacts => Capability::Info,
            Self::ReadFile
            | Self::WriteFile
            | Self::Stat
            | Self::StatMany
            | Self::ListDir
            | Self::ReadText
            | Self::PutFile => Capability::Files,
            Self::Glob | Self::ListFiles | Self::Grep => Capability::Search,
            Self::StartCommand
            | Self::WatchCommand
            | Self::Cancel
            | Self::OperationStatus
            | Self::CloseSession => Capability::Commands,
            Self::ShellState | Self::ShellNotices | Self::AckShellNotice | Self::SetShellCwd => {
                Capability::Shell
            }
            Self::BackgroundCommand | Self::BackgroundElapsed | Self::StopBackgroundCommand => {
                Capability::Background
            }
            Self::RewindPrepare | Self::RewindApply | Self::RewindStatus | Self::RewindFinish => {
                Capability::Checkpoints
            }
            Self::PdfPageCount | Self::RenderPdfPages => Capability::Pdf,
        }
    }

    /// 结果是带 `seq` 的事件流（`watch_command`、`shell_notices`），不是一条回复。
    pub fn is_stream(self) -> bool {
        matches!(self, Self::WatchCommand | Self::ShellNotices)
    }

    /// 有副作用：执行器换了实例时不补发，返回 `interrupted`（§18.5）。
    pub fn is_mutation(self) -> bool {
        matches!(
            self,
            Self::WriteFile
                | Self::PutFile
                | Self::StartCommand
                | Self::Cancel
                | Self::CloseSession
                | Self::AckShellNotice
                | Self::SetShellCwd
                | Self::StopBackgroundCommand
                | Self::RewindPrepare
                | Self::RewindApply
                | Self::RewindFinish
        )
    }
}
