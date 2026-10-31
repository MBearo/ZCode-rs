// 复制自 apps/zcode-host/crates/tui/src/ui/textarea.rs（feat/rs 79b6691c55），按 400 行上限拆出。
//! 编辑区按宽度折行后的位置计算：行、光标位置、上下移动、点击定位与按单位选择。
use unicode_width::UnicodeWidthChar;

use super::{Row, TextArea};
use crate::ui::mouse::{DOUBLE_CLICK, TRIPLE_CLICK};

impl TextArea {
    /// 按宽度折行：优先在空白处断开，单词比一行宽时硬断；换行符各起一行。
    pub fn rows(&self, width: usize) -> Vec<Row> {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut start = 0;
        for line in self.text.split('\n') {
            let line_start = start;
            let mut row_start = line_start;
            let mut used = 0;
            let mut last_space: Option<usize> = None;
            for (offset, c) in line.char_indices() {
                let at = line_start + offset;
                let w = c.width().unwrap_or(0);
                if used + w > width && at > row_start {
                    let cut = last_space.filter(|space| *space > row_start).unwrap_or(at);
                    rows.push(Row {
                        start: row_start,
                        end: cut,
                    });
                    row_start = cut;
                    used = self.text[row_start..at]
                        .chars()
                        .map(|c| c.width().unwrap_or(0))
                        .sum();
                    last_space = None;
                }
                used += w;
                if c == ' ' {
                    last_space = Some(at + 1);
                }
            }
            rows.push(Row {
                start: row_start,
                end: line_start + line.len(),
            });
            start = line_start + line.len() + 1;
        }
        rows
    }

    /// 光标在折行后的（行，列）。列按显示宽度计。
    pub fn cursor_position(&self, width: usize) -> (usize, usize) {
        let rows = self.rows(width);
        let index = rows
            .iter()
            .rposition(|row| row.start <= self.cursor && self.cursor <= row.end)
            .unwrap_or(0);
        let row = rows[index];
        let column = self.text[row.start..self.cursor]
            .chars()
            .map(|c| c.width().unwrap_or(0))
            .sum();
        (index, column)
    }

    /// 在折行后的行之间移动；已在首行 / 末行时返回 false（调用方改为浏览历史）。
    pub fn vertical(&mut self, width: usize, down: bool) -> bool {
        let rows = self.rows(width);
        let (index, column) = self.cursor_position(width);
        let target = if down {
            index + 1
        } else {
            index.wrapping_sub(1)
        };
        let Some(row) = rows.get(target) else {
            return false;
        };
        self.selection = None;
        let goal = *self.goal.get_or_insert(column);
        let mut used = 0;
        let mut at = row.start;
        for (offset, c) in self.text[row.start..row.end].char_indices() {
            let w = c.width().unwrap_or(0);
            if used + w > goal {
                break;
            }
            used += w;
            at = row.start + offset + c.len_utf8();
        }
        // 落在图片的占位文字里时移到较近的一端（tui-input.md §3.1，F26）。
        self.cursor = self.snap_image(at);
        true
    }

    /// 折行后第 `row` 行、显示列 `column` 处的字节位置（超出时取行尾；行号超出时取最后一行）。
    pub fn offset_at(&self, width: usize, row: usize, column: usize) -> usize {
        let rows = self.rows(width);
        let Some(row) = rows.get(row.min(rows.len().saturating_sub(1))) else {
            return 0;
        };
        let mut used = 0;
        let mut at = row.start;
        for (offset, c) in self.text[row.start..row.end].char_indices() {
            let w = c.width().unwrap_or(0);
            if used + w > column || c == '\n' {
                break;
            }
            used += w;
            at = row.start + offset + c.len_utf8();
        }
        at
    }

    /// 鼠标多击的范围：1 次为 `at` 处，2 次为 `at` 处的词（字母、数字与 `_` 连成一个词，其他字符
    /// 各自是一个词），3 次为所在的逻辑行（含换行）。
    pub fn unit_range(&self, at: usize, clicks: u8) -> (usize, usize) {
        let text = &self.text;
        match clicks {
            DOUBLE_CLICK => {
                let word = |c: char| c.is_alphanumeric() || c == '_';
                match text[at..].chars().next() {
                    Some(c) if word(c) => {
                        let start = text[..at]
                            .char_indices()
                            .rev()
                            .find(|(_, c)| !word(*c))
                            .map_or(0, |(i, c)| i + c.len_utf8());
                        let end = text[at..]
                            .find(|c: char| !word(c))
                            .map_or(text.len(), |i| at + i);
                        (start, end)
                    }
                    Some(c) => (at, at + c.len_utf8()),
                    None => (at, at),
                }
            }
            TRIPLE_CLICK => {
                let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
                let end = text[at..].find('\n').map_or(text.len(), |i| at + i + 1);
                (start, end)
            }
            _ => (at, at),
        }
    }

    pub fn at_first_row(&self, width: usize) -> bool {
        self.cursor_position(width).0 == 0
    }

    pub fn at_last_row(&self, width: usize) -> bool {
        self.cursor_position(width).0 + 1 >= self.rows(width).len()
    }
}
