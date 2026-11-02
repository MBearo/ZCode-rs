//! 每种消息的名字、所属端口（能力位）与种类（只读、有副作用、流）（docs/specs/rust-cli/remote-exec.md §18.3、§18.5）。

use crate::handshake::Capability;

/// 每种消息对应 `MachineEnv` 的一个方法（R3 加了 `files.stat_many`，remote-exec.md §20.4；R5 加了 MCP 管道的四种与
/// 期望集合 `mcp.declare`，§22.4、§22.5）。
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
    RewindFinish,
    PdfPageCount,
    RenderPdfPages,
    McpConnect,
    McpSend,
    McpEvents,
    McpClose,
    McpDeclare,
    /// 按客户端实例推一组环境变量（F96，remote-exec.md §23.5）。
    EnvUpdate,
    /// git 与 worktree（F110，deployment.md §19.1）。
    GitStatus,
    GitDiff,
    WorktreeCreate,
    WorktreeList,
    WorktreeChanges,
    WorktreeRemove,
    /// 建目录与上级目录（F110）。
    EnsureDir,
}

/// 一种消息的种类（remote-exec.md §18.5、§19.7）：决定服务端记不记账（文件版账本只把有副作用的写进日志）、执行器换了
/// 实例后补不补发、调用失败时给模型的说明（F96）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpKind {
    /// 只读或幂等：重跑没有副作用，执行器换了实例照常补发。
    Read,
    /// 有副作用：执行器换了实例时不补发，返回 `interrupted`；文件版账本写进日志。
    Mutation,
    /// 结果是带 `seq` 的事件流（`watch_command`、`shell_notices`、`mcp.events`），不是一条回复；消费方按自己的游标
    /// 重新订阅。
    Stream,
}

impl Op {
    pub const ALL: [Self; 41] = [
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
        Self::RewindFinish,
        Self::PdfPageCount,
        Self::RenderPdfPages,
        Self::McpConnect,
        Self::McpSend,
        Self::McpEvents,
        Self::McpClose,
        Self::McpDeclare,
        Self::EnvUpdate,
        Self::GitStatus,
        Self::GitDiff,
        Self::WorktreeCreate,
        Self::WorktreeList,
        Self::WorktreeChanges,
        Self::WorktreeRemove,
        Self::EnsureDir,
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
            Self::RewindFinish => "checkpoints.rewind_finish",
            Self::PdfPageCount => "pdf.pdf_page_count",
            Self::RenderPdfPages => "pdf.render_pdf_pages",
            Self::McpConnect => "mcp.connect",
            Self::McpSend => "mcp.send",
            Self::McpEvents => "mcp.events",
            Self::McpClose => "mcp.close",
            Self::McpDeclare => "mcp.declare",
            Self::EnvUpdate => "env.update",
            Self::GitStatus => "git.status",
            Self::GitDiff => "git.diff",
            Self::WorktreeCreate => "git.worktree_create",
            Self::WorktreeList => "git.worktree_list",
            Self::WorktreeChanges => "git.worktree_changes",
            Self::WorktreeRemove => "git.worktree_remove",
            Self::EnsureDir => "dirs.ensure_dir",
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
            Self::StartCommand | Self::WatchCommand | Self::Cancel | Self::CloseSession => {
                Capability::Commands
            }
            Self::ShellState | Self::ShellNotices | Self::AckShellNotice | Self::SetShellCwd => {
                Capability::Shell
            }
            Self::BackgroundCommand | Self::BackgroundElapsed | Self::StopBackgroundCommand => {
                Capability::Background
            }
            Self::RewindPrepare | Self::RewindApply | Self::RewindFinish => Capability::Checkpoints,
            Self::PdfPageCount | Self::RenderPdfPages => Capability::Pdf,
            Self::McpConnect
            | Self::McpSend
            | Self::McpEvents
            | Self::McpClose
            | Self::McpDeclare => Capability::Mcp,
            Self::EnvUpdate => Capability::Env,
            Self::GitStatus
            | Self::GitDiff
            | Self::WorktreeCreate
            | Self::WorktreeList
            | Self::WorktreeChanges
            | Self::WorktreeRemove => Capability::Git,
            Self::EnsureDir => Capability::Dirs,
        }
    }

    /// 这种消息的种类（[`OpKind`]）。穷举：加一种消息时必须在这里当场决定它是只读、有副作用还是流。
    pub fn kind(self) -> OpKind {
        match self {
            Self::Facts
            | Self::PathFacts
            | Self::ReadFile
            | Self::Stat
            | Self::StatMany
            | Self::ListDir
            | Self::ReadText
            | Self::Glob
            | Self::ListFiles
            | Self::Grep
            | Self::ShellState
            | Self::BackgroundCommand
            | Self::BackgroundElapsed
            | Self::PdfPageCount
            | Self::RenderPdfPages
            | Self::GitStatus
            | Self::GitDiff
            | Self::WorktreeList
            | Self::WorktreeChanges => OpKind::Read,
            // 幂等：再做一次结果相同，换了实例照常补发（F96、F110）。
            Self::EnvUpdate | Self::EnsureDir => OpKind::Read,
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
            | Self::McpConnect
            | Self::McpSend
            | Self::McpClose
            | Self::McpDeclare
            | Self::WorktreeCreate
            | Self::WorktreeRemove => OpKind::Mutation,
            Self::WatchCommand | Self::ShellNotices | Self::McpEvents => OpKind::Stream,
        }
    }
}
