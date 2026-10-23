//! 输入框里的占位文字是一个整体（docs/specs/rust-cli/tui-input.md §3.1，修复线 F26，用户要求）。
//! 占位文字按文字认（`[Image #N]`、`[Pasted text #N]`、`[Pasted text #N +M lines]`，`]` 前可以有若干 `.`），
//! ← / → 整块跳过，backspace / delete 整块删除，词移动与 kill 的范围移出占位文字；↑ / ↓ 与单击落在图片里时移到
//! 较近的一端（只对图片这样做）。
use super::TextArea;

/// `s` 以占位文字开头时它的字节长度。
fn chip_len(s: &str) -> Option<usize> {
    let rest = s.strip_prefix('[')?;
    let rest = rest
        .strip_prefix("Image #")
        .or_else(|| rest.strip_prefix("Pasted text #"))?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let mut rest = &rest[digits..];
    if let Some(after) = rest.strip_prefix(" +") {
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        rest = after[digits..]
            .strip_prefix(" lines")
            .filter(|_| digits > 0)?;
    }
    let rest = rest.trim_start_matches('.').strip_prefix(']')?;
    Some(s.len() - rest.len())
}

/// 文字里全部占位文字的字节范围（不重叠，按位置）。
pub fn chips(text: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = text[from..].find('[') {
        let start = from + offset;
        match chip_len(&text[start..]) {
            Some(len) => {
                found.push((start, start + len));
                from = start + len;
            }
            None => from = start + 1,
        }
    }
    found
}

impl TextArea {
    /// 结束在 `at` 的占位文字。
    pub(super) fn chip_ending_at(&self, at: usize) -> Option<(usize, usize)> {
        chips(&self.text).into_iter().find(|(_, end)| *end == at)
    }

    /// 开始在 `at` 的占位文字。
    pub(super) fn chip_starting_at(&self, at: usize) -> Option<(usize, usize)> {
        chips(&self.text)
            .into_iter()
            .find(|(start, _)| *start == at)
    }

    /// `at` 在里面（不在两端）的占位文字。
    fn chip_containing(&self, at: usize) -> Option<(usize, usize)> {
        chips(&self.text)
            .into_iter()
            .find(|(start, end)| *start < at && at < *end)
    }

    /// 把 `at` 移出占位文字：`start` 为真时移到开头，否则移到末尾。
    pub(super) fn snap(&self, at: usize, start: bool) -> usize {
        match self.chip_containing(at) {
            Some((from, _)) if start => from,
            Some((_, to)) => to,
            None => at,
        }
    }

    /// 落在 `[Image #N]` 里的位置移到较近的一端（光标的修正只看图片）。
    pub(super) fn snap_image(&self, at: usize) -> usize {
        match self.chip_containing(at) {
            Some((from, to)) if self.text[from..].starts_with("[Image") => {
                if at - from < to - at {
                    from
                } else {
                    to
                }
            }
            _ => at,
        }
    }
}
