//! `@` 面板（docs/puffin-tui.md §4.2「提及（@）」；docs/specs/rust-cli/tui-kit.md §4.4）：面板列出的项（文件与文件夹、
//! 过去的会话、子代理）、它的组与这个对话碰过的文件；行与预览在 [`rows`]，数据在 [`data`]。纯的；壳持有 `@` 词、排序、
//! host 的回答与面板的状态，把它们整理成 [`AtPanel`] 与 [`rows::Ctx`]。
//! 移植自 Puffin 的 shell/at.rs（`Touch`、`AtItem`、`AtGroup`、`AtPanel`、`name`、`dir`、`title_of`、
//! `name_of`）与 at/rows.rs（At.scala 第 311–657 行）。
pub mod data;
pub mod rows;
pub mod words;

use data::{ConversationRef, SubagentRef};

/// 没有标题的会话的名字。
pub const UNTITLED: &str = "Untitled";

/// 这个对话对一个文件做了什么：它的工具行改过或读过它，或 git 说它改了没提交。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touch {
    Edited,
    Read,
    Changed,
}

/// 面板列出的一项。路径相对 cwd；文件夹的以 `/` 结尾。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtItem {
    Path { path: String, folder: bool },
    Conversation(ConversationRef),
    Agent(SubagentRef),
}

/// 面板的一组：标签、它的项与还有多少。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AtGroup {
    pub label: String,
    pub items: Vec<AtItem>,
    pub more: usize,
    pub more_word: String,
    pub loading: bool,
}

impl AtGroup {
    pub fn of(label: &str, items: Vec<AtItem>) -> AtGroup {
        AtGroup {
            label: label.to_owned(),
            items,
            ..AtGroup::default()
        }
    }

    pub fn looking(label: &str) -> AtGroup {
        AtGroup {
            label: label.to_owned(),
            loading: true,
            ..AtGroup::default()
        }
    }
}

/// 一个 `@` 词的面板：`under` 是打出的路径（带 `/`），`q` 是它之后的字。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtPanel {
    pub groups: Vec<AtGroup>,
    pub under: String,
    pub q: String,
}

impl AtPanel {
    pub fn entries(&self) -> Vec<&AtItem> {
        self.groups.iter().flat_map(|g| g.items.iter()).collect()
    }

    /// 打出了路径：只有一组，滚动。
    pub fn single(&self) -> bool {
        !self.under.is_empty()
    }

    pub fn loading(&self) -> bool {
        self.groups.iter().any(|g| g.loading)
    }
}

/// 路径的最后一段，文件夹不带 `/`。
pub fn name(path: &str) -> &str {
    let p = path.strip_suffix('/').unwrap_or(path);
    &p[p.rfind('/').map_or(0, |i| i + 1)..]
}

/// 路径所在的文件夹；顶上为 ""。
pub fn dir(path: &str) -> &str {
    let p = path.strip_suffix('/').unwrap_or(path);
    p.rfind('/').map_or("", |i| &p[..i])
}

/// 会话的标题，空的是 [`UNTITLED`]。
pub fn title_of(c: &ConversationRef) -> String {
    let t = c.title.trim();
    if t.is_empty() {
        UNTITLED.to_owned()
    } else {
        t.to_owned()
    }
}

/// 一行显示的名字。
pub fn name_of(it: &AtItem) -> String {
    match it {
        AtItem::Path { path, folder } => {
            format!("{}{}", name(path), if *folder { "/" } else { "" })
        }
        AtItem::Conversation(c) => title_of(c),
        AtItem::Agent(a) => a.name.clone(),
    }
}
