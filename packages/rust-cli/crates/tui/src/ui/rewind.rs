//! rewind 与 resume 的选择器（docs/specs/rust-cli/session-lifecycle.md §5.4、§6.2）。文字照
//! 选择器的原文；两步都复用列表视图：消息列表 → 确认选项。

use std::path::Path;

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::session_api::{FileChange, RewindOption, RewindScope};

use super::app::{Action, App, Overlay};
use super::list::{Choice, Item, ListView, Shape};

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

/// 列表与确认页里的文件名：远端的写 `dev-a:README.md`（F98，remote-exec.md §25.4.5，Docker 试用 L8：以前看不出文件在哪
/// 台机器上）。
fn shown(change: &FileChange) -> String {
    match change.machine.remote() {
        Some(machine) => format!("{machine}:{}", basename(&change.path)),
        None => basename(&change.path),
    }
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
        [one] => format!("{} +{additions} -{deletions}", shown(one)),
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
        // 按（机器，路径）合计：两台机器上的同名文件分开算。
        match out
            .iter_mut()
            .find(|c| c.path == change.path && c.machine == change.machine)
        {
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
        [one] => shown(one),
        [a, b] => format!("{} and {}", shown(a), shown(b)),
        [first, rest @ ..] => format!("{} and {} other files", shown(first), rest.len()),
    };
    format!("The code will be restored +{additions} -{deletions} in {label}.")
}

fn option(label: &str, description: String, value: Choice) -> Item {
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
                value: Choice::Rewind(entry.option.message.clone()),
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
            Shape::TwoLines,
            "Rewind".to_owned(),
            items,
            "Nothing to rewind to yet.".to_owned(),
        )
        .with_subtitle("Restore the code and/or conversation to the point before…".to_owned());
        self.overlay = Some(Overlay::List(list));
    }

    /// 选中一条消息：打开确认选项（`(current)` 是 [`Choice::Close`]，只关闭）。
    pub(super) fn rewind_chosen(&mut self, message: &MessageId) -> Option<Action> {
        let entry = self
            .rewind
            .iter()
            .find(|e| e.option.message == *message)?
            .clone();
        let files = changes_from(&self.rewind, &entry.option.message);
        let code = entry.option.code && !files.is_empty();
        let value = |scope: RewindScope| Choice::RewindTo {
            scope,
            target: entry.option.message.clone(),
        };
        let forked = "The conversation will be forked.".to_owned();
        let mut items = Vec::new();
        if code {
            let both = format!("{forked} {}", code_text(&files));
            items.push(option(
                "Restore code and conversation",
                both,
                value(RewindScope::Both),
            ));
            let only = format!("{forked} The code will be unchanged.");
            items.push(option(
                "Restore conversation",
                only,
                value(RewindScope::Conversation),
            ));
            let unchanged = format!("The conversation will be unchanged. {}", code_text(&files));
            items.push(option("Restore code", unchanged, value(RewindScope::Code)));
        } else {
            let only = format!("{forked} The code will be unchanged.");
            items.push(option(
                "Restore conversation",
                only,
                value(RewindScope::Conversation),
            ));
        }
        items.push(option("Never mind", String::new(), Choice::RewindBack));
        let mut subtitle = format!(
            "Confirm you want to restore {}to the point before you sent this message:\n  {}",
            if code { "" } else { "the conversation " },
            preview(&entry.text)
        );
        if code {
            subtitle.push_str("\n⚠ Rewinding does not affect files edited manually or via bash.");
        }
        let list = ListView::new(Shape::OneLine, "Rewind".to_owned(), items, String::new())
            .with_subtitle(subtitle);
        self.overlay = Some(Overlay::List(list));
        None
    }

    /// 确认：发回退（`Never mind` 是 [`Choice::RewindBack`]，回到消息列表）。
    pub(super) fn rewind_confirmed(
        &mut self,
        scope: RewindScope,
        target: MessageId,
    ) -> Option<Action> {
        let text = self
            .rewind
            .iter()
            .find(|e| e.option.message == target)
            .map(|e| e.text.clone())
            .unwrap_or_default();
        Some(Action::Rewind {
            target,
            scope,
            text,
        })
    }
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
