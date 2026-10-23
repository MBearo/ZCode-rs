// 复制自 apps/zcode-host/crates/tui/src/ui/composer_popup.rs（feat/rs 79b6691c55）。
//! 斜杠命令的弹出列表（docs/specs/rust-tui.md §8.4）：随输入出现与关闭、排序、移动与选中。
use super::{Composer, FREQUENT, Popup};
use crate::ui::commands::{self, Command, Entry};

impl Composer {
    /// 输入变化后重新判断弹出列表：开头是 `/` 且光标还在命令名里时显示斜杠命令。
    pub fn refresh_popup(&mut self) {
        let text = self.area.text();
        let cursor = self.area.cursor();
        let before = &text[..cursor];
        if before.starts_with('/') && !before.contains(char::is_whitespace) {
            let selected = match &self.popup {
                Some(Popup::Slash { selected }) => *selected,
                _ => 0,
            };
            // 没有匹配时列表仍然显示（写 no matches），enter 交给发送，提示命令不存在。
            let count = self.slash_matches().len();
            self.popup = Some(Popup::Slash {
                selected: selected.min(count.saturating_sub(1)),
            });
            return;
        }
        if self.refresh_files() {
            return;
        }
        self.popup = None;
    }

    /// 斜杠命令里与输入匹配的命令，之后是匹配的 skill（tui.md §6）。
    pub fn slash_matches(&self) -> Vec<Entry<'_>> {
        let typed = self
            .area
            .text()
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        let mut entries: Vec<Entry<'_>> = self
            .builtin_matches()
            .into_iter()
            .map(Entry::Builtin)
            .collect();
        entries.extend(
            commands::matching_skills(&self.skills, typed)
                .into_iter()
                .map(Entry::Skill),
        );
        entries
    }

    /// TUI 命令里与输入匹配的。只输入了 `/` 时，历史里用得最多的 5 个排在最前。
    fn builtin_matches(&self) -> Vec<&'static Command> {
        let text = self.area.text();
        let typed = text
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        let mut matches = commands::matching(typed);
        if typed.is_empty() {
            let used = |command: &Command| {
                self.history
                    .iter()
                    .filter(|entry| {
                        entry
                            .strip_prefix('/')
                            .and_then(|rest| rest.split_whitespace().next())
                            == Some(command.name)
                    })
                    .count()
            };
            let mut frequent: Vec<(usize, usize)> = matches
                .iter()
                .enumerate()
                .map(|(index, command)| (index, used(command)))
                .filter(|(_, count)| *count > 0)
                .collect();
            frequent.sort_by_key(|(index, count)| (std::cmp::Reverse(*count), *index));
            let first: Vec<&'static Command> = frequent
                .iter()
                .take(FREQUENT)
                .map(|(index, _)| matches[*index])
                .collect();
            matches.retain(|command| !first.iter().any(|f| std::ptr::eq(*f, *command)));
            matches.splice(0..0, first);
        }
        matches
    }

    pub fn popup_move(&mut self, down: bool) {
        let len = match &self.popup {
            Some(Popup::Slash { .. }) => self.slash_matches().len(),
            Some(Popup::Files { .. }) => self.files.matches.len(),
            None => 0,
        };
        let step = |selected: &mut usize| {
            if len == 0 {
                return;
            }
            *selected = if down {
                (*selected + 1) % len
            } else {
                (*selected + len - 1) % len
            };
        };
        match self.popup.as_mut() {
            Some(Popup::Slash { selected } | Popup::Files { selected }) => step(selected),
            None => {}
        }
    }

    /// 选中斜杠命令：把命令名放进输入框，返回命令名（是否需要参数由调用方决定）。
    pub fn accept_slash(&mut self) -> Option<String> {
        let Some(Popup::Slash { selected }) = self.popup else {
            return None;
        };
        let name = self.slash_matches().get(selected)?.name().to_owned();
        let typed = self.area.text().trim_start_matches('/').trim().to_owned();
        // 完整输入了这个命令的别名（`/stats`、`/cost`）：保留别名。按输入的名字区分（`/stats` 开 Stats 标签页，
        // docs/specs/rust-cli/usage.md §5.1）；以前一律换成命令名，`/stats` 打开的是 Usage 标签页。
        let name = if commands::aliases_of(&name).contains(&typed.as_str()) {
            typed
        } else {
            name
        };
        self.area.set_text(&format!("/{name} "));
        self.popup = None;
        Some(name)
    }
}
