//! `/fork` 的选择器（docs/specs/rust-cli/session-lifecycle.md §6.5.3，工作线 F75）：列出会话里可选的用户输入
//! （与 `/rewind` 同一份，内核给出），每条一行、带序号，旧的在上、焦点在最新的一条；选中即分叉，没有第二步。
//! 选择器与分叉后的提示都随界面语言（工作线 F78）。
use rust_cli_kernel_api::ids::MessageId;
use serde_json::Value;

use super::app::{Action, App, Overlay};
use super::i18n::Lang;
use super::list::{Item, ListView, Purpose};

/// 一条可选的输入：消息 ID 与它的文字。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub message: MessageId,
    pub text: String,
}

/// 分叉完成后的提示（§6.5.3）：英文是内核的回执（Node 原文）加 Node 的 `Switched to forked session <ID>.`；
/// Node 只有英文，中文另配（工作线 F78），消息 ID 与条数取回执的 `message`、`copied`。
pub fn forked_notice(lang: Lang, session: &str, result: &Value) -> String {
    match lang {
        Lang::En => format!(
            "{}\nSwitched to forked session {session}.",
            result["response"].as_str().unwrap_or_default()
        ),
        Lang::Zh => format!(
            "已从消息 {} 分出会话 {session}：复制了 {} 条消息。\n已切换到分出的会话 {session}。",
            result["message"].as_str().unwrap_or_default(),
            result["copied"].as_u64().unwrap_or_default()
        ),
    }
}

impl App {
    /// 打开选择器（条目按会话里的顺序，旧的在前）。
    pub fn open_fork(&mut self, entries: Vec<Entry>) {
        let items: Vec<Item> = entries
            .into_iter()
            .map(|entry| Item {
                label: entry.text.split_whitespace().collect::<Vec<_>>().join(" "),
                value: entry.message.as_str().to_owned(),
                ..Item::default()
            })
            .collect();
        let (title, subtitle, empty) = match self.lang {
            Lang::En => (
                "Fork",
                "Fork the conversation after one of your messages. Files are not changed; use /rewind to restore them.",
                "Nothing to fork from yet.",
            ),
            Lang::Zh => (
                "分叉",
                "从某条输入之后分出新会话。文件不动；要回退文件用 /rewind。",
                "还没有可以分叉的输入。",
            ),
        };
        let mut list = ListView::new(Purpose::Fork, title.to_owned(), items, empty.to_owned())
            .with_subtitle(subtitle.to_owned());
        // 焦点在最新的一条（最后一项）。
        list.selected = list.items.len().saturating_sub(1);
        self.overlay = Some(Overlay::List(list));
    }

    /// 选中一条输入：分叉到这一轮为止。
    pub(super) fn fork_chosen(&mut self, value: &str) -> Option<Action> {
        (!value.is_empty()).then(|| Action::Fork {
            input: MessageId::new(value),
        })
    }
}

#[cfg(test)]
#[path = "fork_tests.rs"]
mod tests;
