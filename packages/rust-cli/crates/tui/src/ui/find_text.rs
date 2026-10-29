//! 搜索的匹配规则（docs/specs/rust-cli/tui-input.md §2.3.3，修复线 F114）：主界面的搜索模式与详细视图的 `/` 共用。
//! 不区分大小写（每个字素各自转小写）、按字素算（匹配的两端都在字素边界上）、一行里从左往右不重叠；列按显示宽度算，
//! 与选区和反色（`selection.rs`）用同一种列。
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// 一行文字的搜索索引：每个字素转小写后接起来的文字，与每个字素在里面的字节起点和显示列。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Folded {
    text: String,
    /// （转小写后的字节起点，显示列）；最后多一项（文字末尾，整行的宽度）。
    marks: Vec<(usize, usize)>,
}

impl Folded {
    pub fn new(line: &str) -> Self {
        let mut text = String::with_capacity(line.len());
        let mut marks = Vec::new();
        let mut col = 0;
        for grapheme in line.graphemes(true) {
            marks.push((text.len(), col));
            text.extend(grapheme.chars().flat_map(char::to_lowercase));
            col += grapheme
                .chars()
                .map(|c| c.width().unwrap_or(0))
                .sum::<usize>();
        }
        marks.push((text.len(), col));
        Self { text, marks }
    }

    /// `needle`（[`fold`] 过的查询）在这一行的出现：（起始列，结束列（不含））。空查询没有出现。
    pub fn find(&self, needle: &str) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        if needle.is_empty() {
            return out;
        }
        let mut from = 0;
        while let Some(at) = self.text[from..].find(needle) {
            let start = from + at;
            let end = start + needle.len();
            match (self.column(start), self.column(end)) {
                (Some(left), Some(right)) => {
                    out.push((left, right));
                    from = end;
                }
                // 落在字素中间：从下一个字符接着找。
                _ => {
                    from = start + self.text[start..].chars().next().map_or(1, char::len_utf8);
                }
            }
        }
        out
    }

    /// 字节位置是字素边界时的显示列。
    fn column(&self, byte: usize) -> Option<usize> {
        self.marks
            .binary_search_by_key(&byte, |(at, _)| *at)
            .ok()
            .map(|index| self.marks[index].1)
    }
}

/// 查询按同样的规则转小写（逐个字符转，不看上下文，与 [`Folded::new`] 相同）。
pub fn fold(query: &str) -> String {
    query.chars().flat_map(char::to_lowercase).collect()
}

/// 删掉最后一个字素（搜索框的 backspace）。
pub fn pop_grapheme(text: &mut String) {
    if let Some((at, _)) = text.grapheme_indices(true).next_back() {
        text.truncate(at);
    }
}

#[cfg(test)]
#[path = "find_text_tests.rs"]
mod tests;
