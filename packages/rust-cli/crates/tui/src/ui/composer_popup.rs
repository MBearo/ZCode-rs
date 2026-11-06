// 复制自 apps/zcode-host/crates/tui/src/ui/composer_popup.rs（feat/rs 79b6691c55）；命令架的匹配、次序与开关改照 Puffin。
//! 命令架（docs/puffin-tui.md §4.3「命令」；Puffin `shell/shelf.rs` 的 `shelf_open` / `shelf_matches` 与 `shell/input.rs` 的
//! `settle_shelf`）：草稿是打出的 `/词`、光标还在词里时出现；匹配是名字或别名的开头，次序固定（agent 的内置命令、TUI 的命令、
//! 内核的其余 skill）；词变了时选中回到第一行，正好打完一个名字时选它；esc 关上之后草稿空了才再开，从历史调回的草稿不开。
//! 以前只输入 `/` 时把用得最多的 5 个提前、完全匹配与别名提前、没有前缀匹配时按子序列（tui-kit.md §6）。
use super::{Composer, Popup};
use crate::ui::commands::{self, Command, Entry};

/// agent 的内置命令在命令架最前，按这个次序（Puffin `AGENT_COMMANDS`）；`init` 是内核给出的 skill。
const AGENT_COMMANDS: [&str; 4] = ["goal", "compact", "init", "plan"];

impl Composer {
    /// 草稿是 `/词` 且光标还在词里时的那个词（命令架按它过滤）。
    fn shelf_word(&self) -> Option<&str> {
        let text = self.area.text();
        let before = &text[..self.area.cursor()];
        if before.starts_with('/') && !before.contains(char::is_whitespace) {
            rust_cli_tui_kit::app::widgets::shelf::word(text.split_whitespace().next()?)
        } else {
            None
        }
    }

    /// 输入变化后重新判断面板：命令架（见模块说明），否则 `@` 文件。
    pub fn refresh_popup(&mut self) {
        // 一个键改过文字之后：整块的提及跟上（`textarea_marks.rs`）。
        self.area.sync_marks();
        if self.area.is_empty() {
            self.shelf_closed = false;
        }
        if let Some(word) = self.shelf_word().map(str::to_owned) {
            if self.shelf_closed {
                self.popup = None;
                return;
            }
            let selected = match (&self.popup, &self.shelf_typed) {
                (Some(Popup::Slash { selected }), Some(was)) if *was == word => *selected,
                _ => {
                    // `/mode` 也是 `/model` 的前缀：正好打完一个名字时选中它，否则 ⏎ 会运行排在前面的 `/model`。
                    let name =
                        commands::find(&word).map_or(word.to_lowercase(), |c| c.name.to_owned());
                    self.slash_matches()
                        .iter()
                        .position(|entry| entry.name() == name)
                        .unwrap_or(0)
                }
            };
            self.shelf_typed = Some(word);
            self.popup = Some(Popup::Slash { selected });
            return;
        }
        self.shelf_typed = None;
        // `$` 与 `@` 是不同的词，不会同时开（Puffin 的 `shelf` 先问 `$`）。
        if self.refresh_skills() {
            return;
        }
        if self.refresh_files() {
            return;
        }
        self.popup = None;
    }

    /// 命令架上与输入匹配的：agent 的内置命令（`goal`、`compact`、内核的 `init`、`plan`），TUI 的命令，然后是内核的其余
    /// skill（tui.md §6）。
    pub fn slash_matches(&self) -> Vec<Entry<'_>> {
        let typed = self.shelf_word().unwrap_or("").to_owned();
        let builtins: Vec<&'static Command> = commands::matching(&typed);
        let skills = commands::matching_skills(&self.skills, &typed);
        let init = skills.iter().find(|skill| skill.name == "init").copied();
        let agent = |name: &str| -> Option<Entry<'_>> {
            if name == "init" {
                return init.map(Entry::Skill);
            }
            builtins
                .iter()
                .find(|c| c.name == name)
                .map(|c| Entry::Builtin(c))
        };
        let mut entries: Vec<Entry<'_>> = AGENT_COMMANDS.iter().filter_map(|n| agent(n)).collect();
        entries.extend(
            builtins
                .iter()
                .filter(|c| !AGENT_COMMANDS.contains(&c.name))
                .map(|c| Entry::Builtin(c)),
        );
        entries.extend(
            skills
                .into_iter()
                .filter(|skill| skill.name != "init")
                .map(Entry::Skill),
        );
        entries
    }

    pub fn popup_move(&mut self, down: bool) {
        let len = match &self.popup {
            Some(Popup::Slash { .. }) => self.slash_matches().len(),
            Some(Popup::Files { .. } | Popup::Skills { .. }) | None => 0,
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
            Some(Popup::Skills { .. }) | None => {}
        }
    }

    /// esc 关上命令架：草稿留着，空了之前不再打开（Puffin `shelf_key` 的 esc）。
    pub fn close_shelf(&mut self) {
        self.popup = None;
        self.shelf_closed = true;
    }

    /// 选中斜杠命令：把命令名放进输入框，返回命令名（是否需要参数由调用方决定）。命令收词时名字后面加一个空格（Puffin
    /// `complete`：`words` 为 `Never` 的不加，[`Command::takes_words`]）。
    pub fn accept_slash(&mut self) -> Option<String> {
        let Some(Popup::Slash { selected }) = self.popup else {
            return None;
        };
        let entry = *self.slash_matches().get(selected)?;
        let name = entry.name().to_owned();
        let typed = self.area.text().trim_start_matches('/').trim().to_owned();
        // 完整输入了这个命令的别名（`/clear`、`/cost`）：保留别名（`/clear 名字` 照旧）。
        let alias =
            commands::find(&name).is_some_and(|command| command.aliases.contains(&typed.as_str()));
        let name = if alias { typed } else { name };
        let space = match entry {
            Entry::Builtin(command) if !command.takes_words() => "",
            _ => " ",
        };
        self.area.set_text(&format!("/{name}{space}"));
        self.popup = None;
        Some(name)
    }
}
