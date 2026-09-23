//! `skipSQ` 与 `scanBraceExpansion`。

use super::{AMP, BACKSLASH, COMMA, DOT, LBRACE, Lexer, PIPE, RBRACE, SEMI, SPACE, SQUOTE};

const BRACE_NOT_OPEN: usize = usize::MAX;
const BRACE_NO_EXPANSION: usize = usize::MAX - 1;

/// 花括号展开预扫描结果：`entries[p - base]` 为从 `p` 处 `{` 开始扫描的结果。
pub(super) struct BraceMemo {
    base: usize,
    entries: Vec<usize>,
}

impl Lexer<'_> {
    pub(super) fn skip_sq(&mut self) {
        while self.pos < self.src_end && self.c(self.pos) != SQUOTE {
            self.pos += 1;
        }
        if self.pos < self.src_end {
            self.pos += 1;
        }
    }

    /// `scanBraceExpansion`：从 `{` 开始判断是否为 `{a,b}` / `{a..b}` 形式的花括号展开，
    /// 返回展开结束位置（越过 `}`）。
    ///
    /// JS 对每个 `{` 都向后重新扫描，`{{{{…` 这类输入是平方复杂度。这里在首次调用时从该 `{`
    /// 起做一次栈式扫描，为其后所有"被逐字检查到"的 `{` 预先算出与逐个扫描完全相同的结果：
    /// 反斜杠跳读只取决于 `{` 之后开始的反斜杠串，因此各起点的对齐方式一致。其余位置回退到逐个扫描。
    pub(super) fn scan_brace_expansion(&mut self, pos: usize, len: usize) -> Option<usize> {
        let next = if pos + 1 < len { self.c(pos + 1) } else { 0 };
        if next <= SPACE || next == RBRACE {
            return None;
        }
        if self.brace_memo.is_none() && len == self.src_end {
            self.brace_memo = Some(self.build_brace_memo(pos, len));
        }
        if let Some(memo) = &self.brace_memo
            && pos >= memo.base
            && let Some(&entry) = memo.entries.get(pos - memo.base)
        {
            match entry {
                BRACE_NOT_OPEN => {}
                BRACE_NO_EXPANSION => return None,
                end => return Some(end),
            }
        }
        self.scan_brace_direct(pos, len)
    }

    fn build_brace_memo(&self, base: usize, len: usize) -> BraceMemo {
        let mut entries = vec![BRACE_NOT_OPEN; len.saturating_sub(base) + 1];
        entries[0] = BRACE_NO_EXPANSION;
        let mut open: Vec<(usize, bool)> = vec![(base, false)];
        let mut scan = base + 1;
        while scan < len {
            let c = self.c(scan);
            if c == LBRACE {
                open.push((scan, false));
                entries[scan - base] = BRACE_NO_EXPANSION;
            } else if c == RBRACE {
                if let Some((start, has_separator)) = open.pop()
                    && has_separator
                {
                    entries[start - base] = scan + 1;
                }
            } else if c <= SPACE || c == SEMI || c == PIPE || c == AMP {
                open.clear();
            } else if (c == COMMA || (c == DOT && scan + 1 < len && self.c(scan + 1) == DOT))
                && let Some(top) = open.last_mut()
            {
                top.1 = true;
            }
            if c == BACKSLASH {
                scan += 1;
            }
            scan += 1;
        }
        BraceMemo { base, entries }
    }

    fn scan_brace_direct(&self, pos: usize, len: usize) -> Option<usize> {
        let mut depth = 1u32;
        let mut has_separator = false;
        let mut scan = pos + 1;
        while scan < len && depth > 0 {
            let c = self.c(scan);
            if c == LBRACE {
                depth += 1;
            } else if c == RBRACE {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if c <= SPACE || c == SEMI || c == PIPE || c == AMP {
                return None;
            } else if depth == 1
                && (c == COMMA || (c == DOT && scan + 1 < len && self.c(scan + 1) == DOT))
            {
                has_separator = true;
            }
            if c == BACKSLASH {
                scan += 1;
            }
            scan += 1;
        }
        (depth == 0 && has_separator).then_some(scan + 1)
    }
}

#[cfg(test)]
mod tests {
    use crate::lexer::Lexer;

    /// 预扫描结果必须与逐个起点扫描完全一致（确定性伪随机输入）。
    #[test]
    fn brace_memo_matches_direct_scan() {
        const ALPHABET: &[u8] = b"{{{}},,..\\ab ;|&'\"$()<>";
        let mut state = 0x2545_f491_u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for _ in 0..20_000 {
            let len = 1 + (next() % 24) as usize;
            let source: Vec<u8> = (0..len)
                .map(|_| ALPHABET[(next() as usize) % ALPHABET.len()])
                .collect();
            for start in 0..len {
                if source[start] != b'{' {
                    continue;
                }
                let mut memoized = Lexer::bounded(&source, 0, len, false);
                let direct = Lexer::bounded(&source, 0, len, false);
                // 先用更早的 `{` 建立预扫描，再查询当前起点
                if let Some(first) = source.iter().position(|&b| b == b'{') {
                    memoized.scan_brace_expansion(first, len);
                }
                let expected = direct.scan_brace_direct(start, len);
                let next_char = source.get(start + 1).copied().unwrap_or(0);
                let expected = if next_char <= b' ' || next_char == b'}' {
                    None
                } else {
                    expected
                };
                assert_eq!(
                    memoized.scan_brace_expansion(start, len),
                    expected,
                    "{:?} at {start}",
                    String::from_utf8_lossy(&source)
                );
            }
        }
    }
}
