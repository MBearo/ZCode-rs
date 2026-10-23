// 复制自 apps/zcode-host/crates/tui/src/ui/textarea.rs（feat/rs 79b6691c55）。
//! 输入框的编辑区（docs/specs/rust-tui.md §8.5）：一个字符串加光标（字节下标，总在字符边界上）。
//! 支持 Emacs 风格的移动与删除、kill ring（ctrl+y 粘回、alt+y 轮换）、撤销、按宽度折行后的
//! 上下移动，以及鼠标选区（§8.7）：输入与粘贴替换选区，删除键删掉选区，移动键取消选区。
use std::time::{Duration, Instant};

/// kill ring 最多保留的条数。
const RING: usize = 10;
/// 撤销最多的步数。
const UNDO: usize = 50;
/// 间隔不到这么久的连续输入算一步撤销。
const COALESCE: Duration = Duration::from_secs(1);

#[derive(Default, Clone)]
pub struct TextArea {
    text: String,
    cursor: usize,
    /// kill 过的内容，最新的在最后。
    ring: Vec<String>,
    /// 刚粘回的内容：（起点，ring 下标）；之后的任何编辑都清掉它（alt+y 只紧跟 yank 生效）。
    yanked: Option<(usize, usize)>,
    /// 撤销栈：（文字，光标）。
    undo: Vec<(String, usize)>,
    /// 上一次记录撤销点的时间与它是不是普通输入。
    last_edit: Option<(Instant, bool)>,
    /// 上下移动时保持的目标列。
    goal: Option<usize>,
    /// 鼠标选区：字节范围（start < end），光标在其中一端。
    selection: Option<(usize, usize)>,
}

/// 折行后的一行：在 `text` 里的字节范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub start: usize,
    pub end: usize,
}

fn prev_boundary(text: &str, at: usize) -> usize {
    text[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}

fn next_boundary(text: &str, at: usize) -> usize {
    text[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}

impl TextArea {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.text.len();
        self.goal = None;
        self.yanked = None;
        self.selection = None;
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        self.selection
    }

    /// 鼠标选择：选中 `[start, end)`，光标放在 `cursor`；空范围只放光标（单击落在图片的占位文字里时移到较近的
    /// 一端，F26）。
    pub fn select(&mut self, start: usize, end: usize, cursor: usize) {
        self.selection = (start < end).then_some((start, end));
        self.cursor = if start < end {
            cursor
        } else {
            self.snap_image(cursor)
        };
        self.goal = None;
        self.yanked = None;
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// 删掉选区，光标停在原来的起点；没有选区时返回 None。
    fn delete_selection(&mut self) -> Option<(usize, usize)> {
        let (start, end) = self.selection.take()?;
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.goal = None;
        self.yanked = None;
        Some((start, end))
    }

    /// 把光标放到字节位置 `at`（落在字符中间时往前取边界）。
    pub fn set_cursor(&mut self, at: usize) {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        self.cursor = at;
        self.goal = None;
        self.selection = None;
    }

    pub fn clear(&mut self) {
        self.set_text("");
    }

    /// 把 `[start, end)` 换成 `with`，光标放在新文字之后（`@` 补全的接受，tui-input.md §1.2）。
    pub fn replace_range(&mut self, start: usize, end: usize, with: &str) {
        let end = end.min(self.text.len());
        let start = start.min(end);
        self.text.replace_range(start..end, with);
        self.cursor = start + with.len();
        self.goal = None;
        self.yanked = None;
        self.selection = None;
    }

    pub fn insert_str(&mut self, text: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.goal = None;
        self.yanked = None;
    }

    /// 编辑前记一个撤销点；`typing` 为普通输入。间隔不到 1 秒的连续输入只记第一次。
    pub fn checkpoint(&mut self, now: Instant, typing: bool) {
        let coalesce = typing
            && self
                .last_edit
                .is_some_and(|(at, was_typing)| was_typing && now.duration_since(at) < COALESCE);
        self.last_edit = Some((now, typing));
        if coalesce {
            return;
        }
        let state = (self.text.clone(), self.cursor);
        if self.undo.last() != Some(&state) {
            self.undo.push(state);
            if self.undo.len() > UNDO {
                self.undo.remove(0);
            }
        }
    }

    /// ctrl+_：回到上一个撤销点；没有时返回 false。
    pub fn undo(&mut self) -> bool {
        while let Some((text, cursor)) = self.undo.pop() {
            if text != self.text || cursor != self.cursor {
                self.text = text;
                self.cursor = cursor;
                self.goal = None;
                self.yanked = None;
                self.selection = None;
                self.last_edit = None;
                return true;
            }
        }
        false
    }

    pub fn insert(&mut self, c: char) {
        let mut buffer = [0u8; 4];
        self.insert_str(c.encode_utf8(&mut buffer));
    }

    /// 往左一步的位置：前面紧挨着占位文字时跳过整块（F26）。
    fn step_left(&self, at: usize) -> usize {
        self.chip_ending_at(at)
            .map_or_else(|| prev_boundary(&self.text, at), |(start, _)| start)
    }

    /// 往右一步的位置：后面紧挨着占位文字时跳过整块（F26）。
    fn step_right(&self, at: usize) -> usize {
        self.chip_starting_at(at)
            .map_or_else(|| next_boundary(&self.text, at), |(_, end)| end)
    }

    pub fn backspace(&mut self) {
        if self.delete_selection().is_some() {
            return;
        }
        self.yanked = None;
        if self.cursor > 0 {
            // 缺陷修复（修复线 F26）：原来一次删一个字符，`[Image #N]` 会剩下半个、发送时对不上图片。
            let start = self.step_left(self.cursor);
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
        }
        self.goal = None;
    }

    pub fn delete(&mut self) {
        if self.delete_selection().is_some() {
            return;
        }
        self.yanked = None;
        let end = self.step_right(self.cursor);
        self.text.replace_range(self.cursor..end, "");
        self.goal = None;
    }

    pub fn left(&mut self) {
        self.cursor = self.step_left(self.cursor);
        self.goal = None;
        self.selection = None;
    }

    pub fn right(&mut self) {
        self.cursor = self.step_right(self.cursor);
        self.goal = None;
        self.selection = None;
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
        self.goal = None;
        self.selection = None;
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
        self.goal = None;
        self.selection = None;
    }

    /// 光标前的上一个词首。
    fn word_start(&self) -> usize {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches(|c: char| !c.is_alphanumeric());
        trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| !c.is_alphanumeric())
            .map_or(0, |(i, c)| i + c.len_utf8())
    }

    /// 光标后的下一个词尾。
    fn word_end(&self) -> usize {
        let after = &self.text[self.cursor..];
        let skip = after.len()
            - after
                .trim_start_matches(|c: char| !c.is_alphanumeric())
                .len();
        let rest = &after[skip..];
        let word = rest
            .find(|c: char| !c.is_alphanumeric())
            .unwrap_or(rest.len());
        self.cursor + skip + word
    }

    /// alt+b：到上一个词首（不停在占位文字里，F26）。
    pub fn word_left(&mut self) {
        self.cursor = self.snap(self.word_start(), true);
        self.goal = None;
        self.selection = None;
    }

    /// alt+f：到下一个词尾（不停在占位文字里，F26）。
    pub fn word_right(&mut self) {
        self.cursor = self.snap(self.word_end(), false);
        self.goal = None;
        self.selection = None;
    }

    /// 删掉 `[start, end)` 放进 kill ring；返回删掉的字符数。有选区时 kill 类的键都只删掉选区。
    fn kill(&mut self, start: usize, end: usize) -> usize {
        // 两端移出占位文字（F26）：删掉的是整块，不剩半个。
        let (start, end) = (self.snap(start, true), self.snap(end, false));
        let (start, end) = self.selection.take().unwrap_or((start, end));
        self.goal = None;
        self.yanked = None;
        if start >= end {
            return 0;
        }
        let killed = self.text[start..end].to_owned();
        let count = killed.chars().count();
        self.ring.push(killed);
        if self.ring.len() > RING {
            self.ring.remove(0);
        }
        self.text.replace_range(start..end, "");
        self.cursor = start;
        count
    }

    /// ctrl+k：删到行尾；已在行尾时删掉换行。
    pub fn kill_to_end(&mut self) -> usize {
        let end = self.line_end();
        let end = if end == self.cursor {
            next_boundary(&self.text, end)
        } else {
            end
        };
        self.kill(self.cursor, end)
    }

    /// ctrl+u：删到行首。
    pub fn kill_to_start(&mut self) -> usize {
        self.kill(self.line_start(), self.cursor)
    }

    /// ctrl+w：删掉前一个词。
    pub fn kill_word(&mut self) -> usize {
        self.kill(self.word_start(), self.cursor)
    }

    /// alt+d：删掉后一个词。
    pub fn kill_word_forward(&mut self) -> usize {
        self.kill(self.cursor, self.word_end())
    }

    /// ctrl+y：粘回最近一次 kill 的内容。
    pub fn yank(&mut self) {
        let Some(latest) = self.ring.last().cloned() else {
            return;
        };
        // 有选区时粘回的内容替换选区。
        let start = self.selection.map_or(self.cursor, |(start, _)| start);
        self.insert_str(&latest);
        self.yanked = Some((start, self.ring.len() - 1));
    }

    /// alt+y：紧跟 yank 时把刚粘回的内容换成 ring 里更早的一条（到头后回到最新）。
    pub fn yank_pop(&mut self) -> bool {
        let Some((start, index)) = self.yanked else {
            return false;
        };
        let next = (index + self.ring.len() - 1) % self.ring.len();
        let replacement = self.ring[next].clone();
        self.text.replace_range(start..self.cursor, &replacement);
        self.cursor = start + replacement.len();
        self.yanked = Some((start, next));
        true
    }
}

#[path = "textarea_layout.rs"]
mod layout;

#[path = "textarea_chips.rs"]
pub mod chips;

#[cfg(test)]
#[path = "textarea_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "textarea_chips_tests.rs"]
mod chip_tests;
