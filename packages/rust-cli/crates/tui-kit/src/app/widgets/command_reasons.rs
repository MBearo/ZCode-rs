//! 命令现在不能运行的原因（docs/puffin-tui.md §4.3「命令」；tui-kit.md §4.2「斜杠命令弹出」）：两套界面共用一条规则。
//! 移植自 Puffin `shell/input/commands.rs` 的 `unavailable` 与 `shell/rewind.rs` 的 `rewind_blocked`；入参是中性的事实
//! （[`CommandFacts`]），各个界面按自己的状态回答。英文是 Puffin 的原话（[`Reason::en`]），别的语言由调用方翻译。

/// 规则要问的事实。是方法而不是字段：命令架每一行都问一次，有的事实要扫 transcript，只在用到时才算（与 Puffin 原来相同）。
pub trait CommandFacts {
    /// 已经有会话（Puffin `state.current.is_some()`）。
    fn session(&self) -> bool;
    /// 回合在跑。
    fn busy(&self) -> bool;
    /// 有还没发出的消息（Puffin `!state.outgoing.is_empty()`）。
    fn sending(&self) -> bool;
    /// 在等用户回答请求（权限、提问）。
    fn asking(&self) -> bool;
    /// 正用着计划模式。
    fn plan(&self) -> bool;
    /// 终端接得住复制（Terminal.app 接不住 OSC 52，docs/puffin-tui.md §4.4）。
    fn clipboard(&self) -> bool;
    fn nothing_to_copy(&self) -> bool;
    fn nothing_to_export(&self) -> bool;
    /// 没有可以回退的点（或正在换会话）。
    fn nothing_to_rewind(&self) -> bool;
    /// 入口给了机器服务。
    fn machines(&self) -> bool;
}

/// 不能运行的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    NothingToCompact,
    WaitForTurn,
    StopTurnFirst,
    WaitForMessage,
    AlreadyNew,
    AnswerFirst,
    NothingToRewind,
    LeavePlanMode,
    SendFirst,
    CannotCopy,
    NothingToCopy,
    NothingToExport,
    NoSessionToName,
    NoMachines,
}

impl Reason {
    /// Puffin 的原话。
    pub const fn en(self) -> &'static str {
        match self {
            Self::NothingToCompact => "Nothing to compact yet.",
            Self::WaitForTurn => "Wait for this turn to end.",
            Self::StopTurnFirst => "Stop the turn first (esc)",
            Self::WaitForMessage => "Wait for your message to go out.",
            Self::AlreadyNew => "This is already a new session.",
            Self::AnswerFirst => "Answer the request first.",
            Self::NothingToRewind => "Nothing to rewind yet.",
            Self::LeavePlanMode => "Leave plan mode first (⇧⇥)",
            Self::SendFirst => "Send a message first.",
            Self::CannotCopy => "This terminal can't copy.",
            Self::NothingToCopy => "Nothing to copy yet.",
            Self::NothingToExport => "Nothing to export yet.",
            Self::NoSessionToName => "There is no session to name yet. Send a message first.",
            Self::NoMachines => "Remote machines aren't available here.",
        }
    }
}

/// 命令 `name` 现在不能运行的原因；`None` 是能运行。次序就是 Puffin 的次序：先对上的那一条说话。
pub fn unavailable(name: &str, f: &dyn CommandFacts) -> Option<Reason> {
    use Reason::*;
    Some(match name {
        "compact" if !f.session() => NothingToCompact,
        "compact" if f.busy() || f.sending() => WaitForTurn,
        "new" if f.busy() => StopTurnFirst,
        "new" if f.sending() => WaitForMessage,
        "new" if !f.session() => AlreadyNew,
        "model" if f.busy() => WaitForTurn,
        "model" if f.sending() => WaitForMessage,
        "resume" if f.busy() => StopTurnFirst,
        "resume" if f.sending() => WaitForMessage,
        "rewind" => return rewind_blocked(f),
        "settings" | "memory" | "mcp" | "skills" | "hooks" | "login" | "usage" | "keys"
            if f.asking() =>
        {
            AnswerFirst
        }
        // agent 不让目标与计划模式同时打开（docs/puffin-tui.md §4.2「目标」）。
        "goal" if f.plan() => LeavePlanMode,
        "stats" if !f.session() => SendFirst,
        "copy" if !f.clipboard() => CannotCopy,
        "copy" if f.nothing_to_copy() => NothingToCopy,
        "export" if f.nothing_to_export() => NothingToExport,
        "rename" if !f.session() => NoSessionToName,
        // 入口没有给机器服务：照样列出，说明为什么用不了（docs/specs/rust-cli/remote-exec.md §21.3）。
        "machines" if !f.machines() => NoMachines,
        _ => return None,
    })
}

/// 现在不能回退的原因（Puffin `rewind_blocked`：命令架、esc esc 与回退列表都用它）。
pub fn rewind_blocked(f: &dyn CommandFacts) -> Option<Reason> {
    Some(if f.busy() {
        Reason::WaitForTurn
    } else if f.sending() {
        Reason::WaitForMessage
    } else if f.asking() {
        Reason::AnswerFirst
    } else if f.nothing_to_rewind() {
        Reason::NothingToRewind
    } else {
        return None;
    })
}

#[cfg(test)]
#[path = "command_reasons_tests.rs"]
mod tests;
