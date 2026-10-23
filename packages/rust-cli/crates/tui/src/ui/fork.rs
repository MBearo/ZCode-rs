//! `/fork` 的 checkpoint 选择器（docs/specs/rust-cli/session-lifecycle.md §6.5.4）：文字取 Node `buildCheckpointSelection`
//! 的原文（中文是本工程的翻译）；两步都复用列表视图：checkpoint 列表 → 文件的处理。
use rust_cli_kernel_api::session_api::ForkFiles;
use rust_cli_kernel_api::store::WorkspaceCheckpoint;

use super::app::{Action, App, Overlay};
use super::i18n::Lang;
use super::list::{Item, ListView, Purpose, SEP};

/// 选择器最多列出的条数（Node `listCheckpoints({limit: 50})`）。
const LIMIT: usize = 50;

/// 一个 checkpoint：内核清单里的一项 + 回合输入的文字。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub checkpoint: WorkspaceCheckpoint,
    pub text: String,
}

/// Node `shortId`：前 8 个字符。
fn short(id: &str) -> String {
    let id = id.strip_prefix("checkpoint_").unwrap_or(id);
    id.chars().take(8).collect()
}

/// 写入时间（assistant 消息 ID 里的毫秒）离现在多久。
fn ago(lang: Lang, created_ms: Option<u64>, now_ms: u64) -> String {
    let Some(created) = created_ms else {
        return String::new();
    };
    let minutes = now_ms.saturating_sub(created) / 60_000;
    match (lang, minutes) {
        (Lang::Zh, 0) => "刚刚".to_owned(),
        (Lang::Zh, m) if m < 60 => format!("{m} 分钟前"),
        (Lang::Zh, m) if m < 60 * 24 => format!("{} 小时前", m / 60),
        (Lang::Zh, m) => format!("{} 天前", m / (60 * 24)),
        (Lang::En, 0) => "just now".to_owned(),
        (Lang::En, m) if m < 60 => format!("{m}m ago"),
        (Lang::En, m) if m < 60 * 24 => format!("{}h ago", m / 60),
        (Lang::En, m) => format!("{}d ago", m / (60 * 24)),
    }
}

impl App {
    /// 打开 checkpoint 列表（新的在前）。
    pub fn open_fork(&mut self, entries: Vec<Entry>, now_ms: u64) {
        let zh = self.lang == Lang::Zh;
        let items = entries
            .into_iter()
            .take(LIMIT)
            .map(|entry| {
                let preview = entry.text.split_whitespace().collect::<Vec<_>>().join(" ");
                let created = entry.checkpoint.assistant.created_ms();
                let files = if zh { "1 个文件" } else { "1 file" };
                Item {
                    label: if preview.is_empty() {
                        format!("message {}", short(entry.checkpoint.prompt.as_str()))
                    } else {
                        preview
                    },
                    description: format!(
                        "{} · {files} · {}",
                        short(&entry.checkpoint.id),
                        ago(self.lang, created, now_ms)
                    ),
                    value: entry.checkpoint.id,
                    ..Item::default()
                }
            })
            .collect();
        let (title, empty) = if zh {
            ("从 checkpoint 分叉", "还没有可用的工作区 checkpoint。")
        } else {
            (
                "Fork From Checkpoint",
                "No workspace checkpoints are available yet.",
            )
        };
        let subtitle = if zh {
            "选择要分叉的 checkpoint。"
        } else {
            "Choose a checkpoint to fork from."
        };
        let list = ListView::new(Purpose::Fork, title.to_owned(), items, empty.to_owned())
            .with_subtitle(subtitle.to_owned())
            .filterable();
        self.overlay = Some(Overlay::List(list));
    }

    /// 选中 checkpoint 后：文件怎么处理。值是 `restore|keep SEP checkpoint`，`Never mind` 为空。
    pub(super) fn fork_chosen(&mut self, checkpoint: &str) -> Option<Action> {
        let zh = self.lang == Lang::Zh;
        let option = |label: &str, description: &str, value: String| Item {
            label: label.to_owned(),
            description: description.to_owned(),
            value,
            ..Item::default()
        };
        let items = if zh {
            vec![
                option(
                    "分叉并恢复文件",
                    "把这个 checkpoint 改过的文件恢复成改动前的内容",
                    format!("restore{SEP}{checkpoint}"),
                ),
                option(
                    "只分叉对话",
                    "文件保持现状",
                    format!("keep{SEP}{checkpoint}"),
                ),
                option("算了", "", String::new()),
            ]
        } else {
            vec![
                option(
                    "Fork and restore files",
                    "Restore the files this checkpoint changed to their content before the change",
                    format!("restore{SEP}{checkpoint}"),
                ),
                option(
                    "Fork conversation only",
                    "Leave the files as they are",
                    format!("keep{SEP}{checkpoint}"),
                ),
                option("Never mind", "", String::new()),
            ]
        };
        let title = if zh {
            "从 checkpoint 分叉"
        } else {
            "Fork From Checkpoint"
        };
        self.overlay = Some(Overlay::List(ListView::new(
            Purpose::ForkConfirm,
            title.to_owned(),
            items,
            String::new(),
        )));
        None
    }

    pub(super) fn fork_confirmed(&mut self, value: &str) -> Option<Action> {
        // 列表给含 SEP 的值再接一个 SEP（档位位置，list.rs `chosen`）：按段取。
        let mut parts = value.split(SEP);
        let files = parts.next()?;
        let checkpoint = parts.next().filter(|id| !id.is_empty())?;
        let files = match files {
            "restore" => ForkFiles::Restore,
            "keep" => ForkFiles::Keep,
            _ => return None,
        };
        Some(Action::ForkCheckpoint {
            checkpoint: Some(checkpoint.to_owned()),
            files,
        })
    }
}
