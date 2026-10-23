//! rewind 与 resume 的选择器（docs/specs/rust-cli/session-lifecycle.md §5.4、§6.2）。文字照
//! 选择器的原文；两步都复用列表视图：消息列表 → 确认选项。

use std::path::Path;

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::session_api::{FileChange, RewindOption, RewindScope};

use super::app::{Action, App, Overlay};
use super::list::{Item, ListView, Purpose, SEP};

/// 一条可选的 user 消息：内核给的改动统计 + 消息文字。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub option: RewindOption,
    pub text: String,
}

fn basename(path: &str) -> String {
    Path::new(path).file_name().map_or_else(
        || path.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn totals(files: &[FileChange]) -> (u64, u64) {
    files
        .iter()
        .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions))
}

/// 列表第二行：`<文件名> +a -d` / `N files changed +a -d` / `No code changes`。
fn summary(files: &[FileChange]) -> String {
    let (additions, deletions) = totals(files);
    match files {
        [] => "No code changes".to_owned(),
        [one] => format!("{} +{additions} -{deletions}", basename(&one.path)),
        many => format!("{} files changed +{additions} -{deletions}", many.len()),
    }
}

/// 第一行：消息压成一行。
fn preview(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 从这条消息起到最后的全部改动（确认页的统计）。
fn changes_from(entries: &[Entry], message: &MessageId) -> Vec<FileChange> {
    let start = entries
        .iter()
        .position(|e| &e.option.message == message)
        .unwrap_or(entries.len());
    let mut out: Vec<FileChange> = Vec::new();
    for change in entries[start..].iter().flat_map(|e| e.option.files.iter()) {
        match out.iter_mut().find(|c| c.path == change.path) {
            Some(seen) => {
                seen.additions += change.additions;
                seen.deletions += change.deletions;
            }
            None => out.push(change.clone()),
        }
    }
    out
}

/// 恢复代码的确认：`The code will be restored +a -d in <文件>.`
fn code_text(files: &[FileChange]) -> String {
    let (additions, deletions) = totals(files);
    let label = match files {
        [] => return "The code has not changed (nothing will be restored).".to_owned(),
        [one] => basename(&one.path),
        [a, b] => format!("{} and {}", basename(&a.path), basename(&b.path)),
        [first, rest @ ..] => format!("{} and {} other files", basename(&first.path), rest.len()),
    };
    format!("The code will be restored +{additions} -{deletions} in {label}.")
}

fn option(label: &str, description: String, value: String) -> Item {
    Item {
        label: label.to_owned(),
        description,
        value,
        ..Item::default()
    }
}

impl App {
    /// 消息列表；没有可选消息时显示 `Nothing to rewind to yet.`。
    pub fn open_rewind(&mut self, entries: Vec<Entry>) {
        let mut items: Vec<Item> = entries
            .iter()
            .map(|entry| Item {
                label: preview(&entry.text),
                description: summary(&entry.option.files),
                value: entry.option.message.as_str().to_owned(),
                ..Item::default()
            })
            .collect();
        if !items.is_empty() {
            items.push(Item {
                label: "(current)".to_owned(),
                current: true,
                ..Item::default()
            });
        }
        self.rewind = entries;
        let list = ListView::new(
            Purpose::Rewind,
            "Rewind".to_owned(),
            items,
            "Nothing to rewind to yet.".to_owned(),
        )
        .with_subtitle("Restore the code and/or conversation to the point before…".to_owned());
        self.overlay = Some(Overlay::List(list));
    }

    /// 选中一条消息：打开确认选项（`(current)` 只关闭）。
    pub(super) fn rewind_chosen(&mut self, value: &str) -> Option<Action> {
        let entry = self
            .rewind
            .iter()
            .find(|e| e.option.message.as_str() == value)?
            .clone();
        let files = changes_from(&self.rewind, &entry.option.message);
        let code = entry.option.code && !files.is_empty();
        let id = entry.option.message.as_str();
        let value = |scope: &str| format!("{scope}{SEP}{id}");
        let forked = "The conversation will be forked.".to_owned();
        let mut items = Vec::new();
        if code {
            let both = format!("{forked} {}", code_text(&files));
            items.push(option("Restore code and conversation", both, value("both")));
            let only = format!("{forked} The code will be unchanged.");
            items.push(option("Restore conversation", only, value("conversation")));
            let unchanged = format!("The conversation will be unchanged. {}", code_text(&files));
            items.push(option("Restore code", unchanged, value("code")));
        } else {
            let only = format!("{forked} The code will be unchanged.");
            items.push(option("Restore conversation", only, value("conversation")));
        }
        items.push(option("Never mind", String::new(), String::new()));
        let mut subtitle = format!(
            "Confirm you want to restore {}to the point before you sent this message:\n  {}",
            if code { "" } else { "the conversation " },
            preview(&entry.text)
        );
        if code {
            subtitle.push_str("\n⚠ Rewinding does not affect files edited manually or via bash.");
        }
        let list = ListView::new(
            Purpose::RewindConfirm,
            "Rewind".to_owned(),
            items,
            String::new(),
        )
        .with_subtitle(subtitle);
        self.overlay = Some(Overlay::List(list));
        None
    }

    /// 确认：`Never mind` 回到消息列表；其余发回退。
    pub(super) fn rewind_confirmed(&mut self, value: &str) -> Option<Action> {
        // 列表给含 SEP 的值末尾再补一个 SEP（模型列表的档位位置），这里去掉。
        let value = value.trim_end_matches(SEP);
        let Some((scope, id)) = value.split_once(SEP) else {
            let entries = std::mem::take(&mut self.rewind);
            self.open_rewind(entries);
            return None;
        };
        let scope = match scope {
            "both" => RewindScope::Both,
            "code" => RewindScope::Code,
            _ => RewindScope::Conversation,
        };
        let text = self
            .rewind
            .iter()
            .find(|e| e.option.message.as_str() == id)
            .map(|e| e.text.clone())
            .unwrap_or_default();
        Some(Action::Rewind {
            target: MessageId::new(id),
            scope,
            text,
        })
    }
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
