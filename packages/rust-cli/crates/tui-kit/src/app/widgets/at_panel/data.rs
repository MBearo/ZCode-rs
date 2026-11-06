//! `@` 面板画的数据（docs/puffin-tui.md §4.2「提及（@）」、§5 mentions；docs/specs/rust-cli/tui-kit.md §4.4）：路径的预览、
//! 已开启的子代理与引用的会话。是纯的 serde 结构，从 Puffin 的投影（host/projection/mentions.rs）
//! 搬来。形状来自 packages/tui/host/src/contract-mentions.ts。
use serde::{Deserialize, Serialize};

/// 一个路径的预览：文件的行数（只数到 8 MB，`more` 说后面还有）、大小与前 4 行；文件夹在索引里直接的项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PathPeek {
    File {
        lines: u64,
        more: bool,
        binary: bool,
        bytes: u64,
        head: Vec<String>,
    },
    Folder {
        entries: u64,
        names: Vec<String>,
    },
    Missing,
}

/// 一个已开启的子代理；`tools` 为 None 是全部工具，`pluginName` 只在插件的子代理上不为 ""。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRef {
    pub name: String,
    pub description: String,
    pub source: SubagentSource,
    #[serde(default)]
    pub plugin_name: String,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubagentSource {
    Project,
    User,
    Plugin,
    Builtin,
}

/// `@` 引用的一个会话（`conversations`）：最近的会话，按更新时间从新到旧。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ConversationRef {
    pub session_id: String,
    pub title: String,
    pub updated_at: u64,
}
