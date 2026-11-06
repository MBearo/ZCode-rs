//! 列表的循环移动（架构整理 U12）：以前循环移动（到头绕回）写了 6 份。

/// 列表里循环移动一格：到头绕回；没有条目时停在 0。
pub fn cycle(index: usize, count: usize, down: bool) -> usize {
    match count {
        0 => 0,
        _ if down => (index + 1) % count,
        _ => (index + count - 1) % count,
    }
}
