//! 本工程独有的 entry 的名字（README §6 规则 9，node-store.md §4.7）：`type` 是 `rust-cli/<种类>`，键是
//! `<会话>:rust-cli/<种类>`（每个会话一条的）或 `<会话>:rust-cli/<种类>:<id>`（一种有多条的）。Node 不认识这个前缀的
//! 记录，原样留着。以前 5 处各自拼或拆键、`"rust-cli/"` 写了十几次（F128，审查 X10）。
//!
//! 内核经 `Fact::Entry` 写的种类在 kernel-api 的 `store::entry_kinds`；下面是 node-store 自己写的种类。

/// 本工程自己的名字的前缀（entry 的 `type`，与 Node 重名的 attachment 来源也加它）。
pub const PREFIX: &str = "rust-cli/";

/// 提交回执的序号（每个会话一条）。
pub const COMMIT: &str = "commit";
/// 内核的权限模式名（dontAsk 在 Node 里没有对应值，每个会话一条）。
pub const PERMISSION_MODE: &str = "permission_mode";
/// 选择的团队项目（工作线 F49，每个会话一条）。
pub const MODEL_CONNECTION: &str = "model_connection";
/// 压缩边界（一种有多条，id 是边界）。
pub const COMPACT_BOUNDARY: &str = "compact_boundary";
/// 远端机器上的 checkpoint 与文件回退（R3，键另有写法，见 rewind.rs）。
pub const REMOTE_CHECKPOINT: &str = "remote_checkpoint";
pub const REMOTE_FILE_REWIND: &str = "remote_file_rewind";

/// 一条本工程的 entry 的键：会话、种类（不带前缀）、同一种类有多条时的 id。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryKey<'a> {
    pub session: &'a str,
    pub kind: &'a str,
    pub id: Option<&'a str>,
}

impl<'a> EntryKey<'a> {
    /// 每个会话一条的种类。
    pub fn single(session: &'a str, kind: &'a str) -> Self {
        Self {
            session,
            kind,
            id: None,
        }
    }

    /// 一种有多条：按 id 区分。
    pub fn item(session: &'a str, kind: &'a str, id: &'a str) -> Self {
        Self {
            session,
            kind,
            id: Some(id),
        }
    }

    /// `session_entry.id`。
    pub fn encode(&self) -> String {
        match self.id {
            Some(id) => format!("{}:{PREFIX}{}:{id}", self.session, self.kind),
            None => format!("{}:{PREFIX}{}", self.session, self.kind),
        }
    }

    /// 拆开一种有多条的键：是这个会话、这个种类的时候给出 id。
    pub fn parse(key: &'a str, session: &'a str, kind: &'a str) -> Option<Self> {
        let id = key
            .strip_prefix(session)?
            .strip_prefix(':')?
            .strip_prefix(PREFIX)?
            .strip_prefix(kind)?
            .strip_prefix(':')?;
        Some(Self::item(session, kind, id))
    }
}

/// 本工程自己的名字 `rust-cli/<名字>`：一种 entry 的 `session_entry.type`，或与 Node 重名的 attachment 来源。
pub fn named(name: &str) -> String {
    format!("{PREFIX}{name}")
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
