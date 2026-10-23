// 复制自 apps/zcode-host/crates/tui/src/ui/composer.rs（feat/rs 79b6691c55）。
//! 输入框的状态（docs/specs/rust-tui.md §8.3、§8.5）：编辑区、输入历史与搜索、粘贴折叠、斜杠命令
//! 弹出列表与 `@` 文件补全（docs/specs/rust-cli/tui-input.md §1.2，`composer_files.rs`）。历史只在内存里
//! （docs/specs/rust-cli/tui.md §9）。
use super::textarea::TextArea;

/// 超过这么多字符的粘贴在输入框里折叠成占位文字。
pub const PASTE_FOLD_CHARS: usize = 800;
/// 超过这么多行的粘贴也折叠。
const PASTE_FOLD_LINES: usize = 2;
/// 输入框为空时斜杠命令列表最前面列出的常用命令数。
const FREQUENT: usize = 5;

/// 一次粘贴的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pasted {
    Inserted,
    /// 折叠成 `[Pasted text #N …]`。
    Folded,
    /// 同样的内容再粘贴一次：原来的占位文字在原处展开。
    Expanded,
}

/// 输入模式（docs/specs/rust-cli/tui-input.md §2.1；`prompt` / `bash`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InputMode {
    #[default]
    Prompt,
    /// `!` 开头：发送时执行 shell 命令。
    Bash,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Popup {
    Slash {
        selected: usize,
    },
    /// `@` 文件补全（候选在 `Composer::files.matches`）。
    Files {
        selected: usize,
    },
}

#[derive(Default)]
pub struct Composer {
    pub area: TextArea,
    history: Vec<String>,
    browsing: Option<usize>,
    draft: String,
    pastes: Vec<(String, String)>,
    /// 已用过的粘贴编号（占位文字的编号不复用）。
    paste_count: usize,
    /// ctrl+r：（查询，匹配到的历史下标）。
    pub search: Option<(String, Option<usize>)>,
    pub popup: Option<Popup>,
    /// 内核给出的 skill 命令（`Kernel::user_commands`，runner 在启动时与回合结束时取，tui.md §6）。
    pub skills: Vec<rust_cli_kernel_api::user_command::UserCommand>,
    /// 清单至少到过一次（启动时异步取，tui.md §6）；之前提交的命令先留着（`App::held_command`）。
    pub skills_loaded: bool,
    /// `@` 补全的索引与候选（tui-input.md §1.2）。
    pub files: files::Files,
    /// ctrl+s 暂存的草稿（tui-input.md §5）。
    stash: Option<draft::Draft>,
    /// 输入模式（`!` bash 模式，tui-input.md §2.1）。
    pub mode: InputMode,
    /// 粘贴的图片（tui-input.md §3.1）。
    images: Vec<images::Pasted>,
    /// 占位文字被删掉的图片：只留给撤销（F26，`sync_images`）；清空输入框时丢掉。
    detached: Vec<images::Pasted>,
    /// 刚粘贴了一张图片：下一张图片前、下一个键入的字符前按规则补空格（`before_typing`）。
    after_image: bool,
    /// 上一次发送记进历史的那一条（与上一条相同、没有记时为 None）：esc 撤回时去掉它（修复线 F38）。
    submitted: Option<String>,
}

impl Composer {
    pub fn is_empty(&self) -> bool {
        self.area.is_empty()
    }

    /// 输入框里的粘贴占位文字与图片的 `[Image #N]`（显示为青色）。
    pub fn placeholders(&self) -> impl Iterator<Item = &str> {
        self.pastes
            .iter()
            .map(|(placeholder, _)| placeholder.as_str())
            .chain(self.images.iter().map(|image| image.placeholder.as_str()))
    }

    pub fn clear(&mut self) {
        self.after_image = false;
        self.area.clear();
        self.pastes.clear();
        self.images.clear();
        self.detached.clear();
        self.popup = None;
        self.browsing = None;
        self.mode = InputMode::Prompt;
    }

    /// 放进一条历史或放回的输入：`!` 开头时切到 bash 模式并去掉 `!`。
    pub fn load_entry(&mut self, text: &str) {
        match text.strip_prefix('!') {
            Some(command) => {
                self.area.set_text(command);
                self.mode = InputMode::Bash;
            }
            None => {
                self.area.set_text(text);
                self.mode = InputMode::Prompt;
            }
        }
    }

    pub fn set_text(&mut self, text: &str) {
        self.clear();
        self.area.set_text(text);
    }

    /// 普通文字的粘贴：统一换行，tab 换成 4 个空格。超过 [`PASTE_FOLD_CHARS`] 个字符或超过 2 行的
    /// 折叠成 `[Pasted text #N +M lines]`（单行为 `[Pasted text #N]`），发送时展开；同样的内容再粘贴
    /// 一次就在原处展开。
    pub fn paste_text(&mut self, text: &str) -> Pasted {
        // 粘贴文字只在链接粘贴之后补空格（本工程没有），图片的标记随之清掉。
        self.after_image = false;
        let text = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ");
        let folded = self.pastes.iter().position(|(placeholder, content)| {
            *content == text && self.area.text().contains(placeholder.as_str())
        });
        if let Some(index) = folded {
            let (placeholder, content) = self.pastes.remove(index);
            let at = self.area.text().find(placeholder.as_str()).unwrap_or(0);
            let next = self.area.text().replacen(placeholder.as_str(), &content, 1);
            self.area.set_text(&next);
            self.area.set_cursor(at + content.len());
            return Pasted::Expanded;
        }
        let lines = text.split('\n').count();
        if text.chars().count() <= PASTE_FOLD_CHARS && lines <= PASTE_FOLD_LINES {
            self.area.insert_str(&text);
            return Pasted::Inserted;
        }
        self.paste_count += 1;
        let placeholder = if lines > 1 {
            format!("[Pasted text #{} +{lines} lines]", self.paste_count)
        } else {
            format!("[Pasted text #{}]", self.paste_count)
        };
        self.area.insert_str(&placeholder);
        self.pastes.push((placeholder, text));
        Pasted::Folded
    }

    /// 鼠标选择（§8.7）：从按下时的范围 `origin` 选到 `at`，按 `clicks` 取整（1 字符，2 词，3 逻辑行）。
    /// 盖住占位文字的一部分时扩展到整个占位文字，删除或替换后不会剩下半个、发送时展不开；有选区时
    /// 收起弹出列表。
    pub fn select(&mut self, origin: (usize, usize), at: usize, clicks: u8) {
        let (from, to) = self.area.unit_range(at, clicks);
        let (mut start, mut end) = (origin.0.min(from), origin.1.max(to));
        if start < end {
            let text = self.area.text();
            for placeholder in self.placeholders() {
                for (p, _) in text.match_indices(placeholder) {
                    let q = p + placeholder.len();
                    if p < end && q > start {
                        (start, end) = (start.min(p), end.max(q));
                    }
                }
            }
            self.popup = None;
        }
        // 往起点前面拖时光标在选区开头，否则在末尾。
        let cursor = if from < origin.0 { start } else { end };
        self.area.select(start, end, cursor);
    }

    /// 取出要发送的文字（展开粘贴）并清空；非空白的输入记入历史。
    pub fn submit(&mut self) -> String {
        let mut text = self.area.text().to_owned();
        for (placeholder, content) in &self.pastes {
            text = text.replacen(placeholder.as_str(), content, 1);
        }
        let text = text.trim().to_owned();
        // bash 模式的输入在历史里带 `!`。
        let entry = match self.mode {
            InputMode::Bash => format!("!{text}"),
            InputMode::Prompt => text.clone(),
        };
        self.submitted = None;
        if !text.is_empty() && self.history.last() != Some(&entry) {
            self.history.push(entry.clone());
            self.submitted = Some(entry);
        }
        self.clear();
        text
    }

    /// esc 撤回（修复线 F38，tui-input.md §8）：去掉上一次发送时记下的那条历史（只在本进程的历史里；
    /// 发送时与上一条相同、没有记下时不删）。
    pub fn forget_submitted(&mut self) {
        if let Some(entry) = self.submitted.take()
            && self.history.last() == Some(&entry)
        {
            self.history.pop();
        }
    }

    /// 输入框的内容写成历史的样子（bash 模式带 `!`）。
    fn entry_text(&self) -> String {
        match self.mode {
            InputMode::Bash => format!("!{}", self.area.text()),
            InputMode::Prompt => self.area.text().to_owned(),
        }
    }

    /// 去掉最后一条历史（带 key 的 `/login`，tui.md §7.2 D7）。
    pub fn forget_last(&mut self) {
        self.history.pop();
    }

    /// 已有的历史（从旧到新），放在本进程的历史之前。
    pub fn load_history(&mut self, entries: Vec<String>) {
        let mut entries = entries;
        entries.append(&mut self.history);
        self.history = entries;
    }

    /// ctrl+c 清空输入：内容记入历史（↑ 可以取回），不发送。
    pub fn discard(&mut self) {
        let text = self.entry_text().trim().to_owned();
        if !text.is_empty() && text != "!" && self.history.last() != Some(&text) {
            self.history.push(text);
        }
        self.clear();
    }

    /// ↑ / ↓ 浏览历史的条件：输入框为空，或内容就是刚取出的历史且光标在开头或末尾。
    pub fn can_browse(&self) -> bool {
        if self.area.is_empty() {
            return true;
        }
        let at_edge = self.area.cursor() == 0 || self.area.cursor() == self.area.text().len();
        let recalled = self.browsing.is_some_and(|index| {
            self.history.get(index).map(String::as_str) == Some(self.entry_text().as_str())
        });
        recalled && at_edge
    }

    /// 正在浏览的历史：（第几条，共几条），最新的一条是第 1 条。
    pub fn history_position(&self) -> Option<(usize, usize)> {
        self.browsing
            .map(|index| (self.history.len() - index, self.history.len()))
    }

    pub fn history_prev(&mut self) -> bool {
        let next = match self.browsing {
            None if self.history.is_empty() => return false,
            None => {
                self.draft = self.entry_text();
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(index) => index - 1,
        };
        self.browsing = Some(next);
        let text = self.history[next].clone();
        self.load_entry(&text);
        true
    }

    pub fn history_next(&mut self) -> bool {
        let Some(index) = self.browsing else {
            return false;
        };
        if index + 1 < self.history.len() {
            self.browsing = Some(index + 1);
            let text = self.history[index + 1].clone();
            self.load_entry(&text);
        } else {
            self.browsing = None;
            let draft = std::mem::take(&mut self.draft);
            self.load_entry(&draft);
        }
        true
    }

    /// ctrl+r 的查询变化：从新到旧找第一条包含查询的历史。
    pub fn search_update(&mut self, query: String) {
        let found = self
            .history
            .iter()
            .rposition(|entry| entry.contains(query.as_str()));
        self.search = Some((query, found));
    }

    /// ctrl+r 再按一次：找更早的一条。
    pub fn search_older(&mut self) {
        if let Some((query, Some(current))) = self.search.clone() {
            let older = self.history[..current]
                .iter()
                .rposition(|entry| entry.contains(query.as_str()));
            if older.is_some() {
                self.search = Some((query, older));
            }
        }
    }

    pub fn search_match(&self) -> Option<&str> {
        let (_, index) = self.search.as_ref()?;
        index.map(|i| self.history[i].as_str())
    }

    /// 接受搜索结果放进输入框。
    pub fn search_accept(&mut self) {
        if let Some(text) = self.search_match().map(str::to_owned) {
            self.load_entry(&text);
        }
        self.search = None;
    }
}

#[path = "composer_popup.rs"]
mod popup;

#[path = "composer_files.rs"]
pub mod files;

#[path = "composer_draft.rs"]
mod draft;

#[path = "composer_images.rs"]
pub mod images;

#[path = "composer_restore.rs"]
mod restore;

#[cfg(test)]
#[path = "composer_tests.rs"]
mod tests;
