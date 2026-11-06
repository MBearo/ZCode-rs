//! 输入框里整块的提及（tui-input.md §1.2「插入会话」；照 Puffin `shell/draft.rs` 的 `refs`）：`@` 面板插入的会话是一段文字
//! `@标题`，记着它的种类与发出去的写法。←→ 整块跳过、⌫ ⌦ 整块删除（与占位文字同一套，`textarea_chips.rs`）；在它前后编辑时
//! 它跟着移动，编辑落进它里面时它变回普通文字。
//!
//! 位置的跟随不在每个编辑函数里各做一遍：[`TextArea::sync_marks`] 把这一次的文字与上一次同步时的文字比开头与末尾相同的部分，
//! 改过的那一段之前的 mark 不动、之后的挪动、碰到的丢掉（与 Puffin 的规则相同）。没同步时（一个键的中间）不用 mark。
use super::TextArea;

/// 一个整块的提及：`[start, end)`、种类（kit `ink_of_kind` 的 `conversation`）与发出去的写法。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub start: usize,
    pub end: usize,
    pub kind: &'static str,
    pub wire: String,
}

impl TextArea {
    /// 记下刚插入的 `[start, end)` 是一个整块的提及。
    pub fn mark(&mut self, start: usize, end: usize, kind: &'static str, wire: String) {
        self.sync_marks();
        if start < end && end <= self.text.len() {
            self.marks.push(Mark {
                start,
                end,
                kind,
                wire,
            });
            self.marks.sort_by_key(|m| m.start);
        }
    }

    /// 文字改过之后跟上：改过的一段之前的不动，之后的挪动，碰到的变回普通文字。
    pub fn sync_marks(&mut self) {
        if self.marked == self.text {
            return;
        }
        let (old, new) = (self.marked.as_bytes(), self.text.as_bytes());
        let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let room = old.len().min(new.len()) - head;
        let tail = old
            .iter()
            .rev()
            .zip(new.iter().rev())
            .take(room)
            .take_while(|(a, b)| a == b)
            .count();
        let (cut, grown) = (old.len() - tail, new.len() as isize - old.len() as isize);
        self.marks.retain_mut(|m| {
            if m.end <= head {
                true
            } else if m.start >= cut {
                m.start = (m.start as isize + grown) as usize;
                m.end = (m.end as isize + grown) as usize;
                true
            } else {
                false
            }
        });
        self.marked = self.text.clone();
    }

    /// 同步过的整块提及（没同步时没有）。
    pub fn marks(&self) -> &[Mark] {
        if self.marked == self.text {
            &self.marks
        } else {
            &[]
        }
    }

    /// 整块提及的范围，同步过时才有（按键里用：键的中间文字可能已经改了）。
    pub(super) fn mark_ranges(&self) -> Vec<(usize, usize)> {
        self.marks().iter().map(|m| (m.start, m.end)).collect()
    }

    /// 发出去的文字：每个整块提及换成它的写法。
    pub fn wired(&mut self) -> String {
        self.sync_marks();
        let mut out = String::with_capacity(self.text.len());
        let mut at = 0;
        for m in &self.marks {
            out.push_str(&self.text[at..m.start]);
            out.push_str(&m.wire);
            at = m.end;
        }
        out.push_str(&self.text[at..]);
        out
    }
}

#[cfg(test)]
#[path = "textarea_marks_tests.rs"]
mod tests;
