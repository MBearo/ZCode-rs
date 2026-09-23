//! heredoc：定界符、正文读取（在下一个 Newline token 时）以及正文的展开扫描。

use super::{
    AT, BACKSLASH, BACKTICK, BANG, DASH, DOLLAR, DQUOTE, HASH, HereDocBody, LBRACE, LPAREN, Lexer,
    PendingHereDoc, QUESTION, SQUOTE, STAR, TAB, UNDERSCORE, is_digit, is_meta,
};
use crate::parts::Part;

/// `parseHereDocBody` 的快速扫描：只有出现反引号或 `$` 后跟可展开字符时才生成正文单词。
fn body_may_expand(body: &[u8]) -> bool {
    let mut i = 0;
    while i < body.len() {
        let c = u32::from(body[i]);
        if c == BACKTICK {
            return true;
        }
        if c == DOLLAR {
            let next = body.get(i + 1).map_or(0, |&b| u32::from(b));
            let expands = matches!(
                next,
                LBRACE | LPAREN | DOLLAR | UNDERSCORE | BANG | HASH | AT | STAR | QUESTION | DASH
            ) || (next < 128 && (next as u8).is_ascii_alphabetic())
                || is_digit(next);
            if expands {
                return true;
            }
        }
        if c == BACKSLASH {
            i += 1;
        }
        i += 1;
    }
    false
}

impl Lexer<'_> {
    /// `readHereDocDelimiter`：结果写入 `here_delim` / `here_quoted`。
    pub(super) fn read_heredoc_delimiter(&mut self) {
        let len = self.src_end;
        let mut delimiter = Vec::new();
        let first = if self.pos < len { self.c(self.pos) } else { 0 };
        if first == SQUOTE {
            self.pos += 1;
            let start = self.pos;
            while self.pos < len && self.c(self.pos) != SQUOTE {
                self.pos += 1;
            }
            delimiter.extend_from_slice(self.slice(start, self.pos));
            if self.pos < len {
                self.pos += 1;
            }
            self.here_quoted = true;
        } else if first == DQUOTE {
            self.pos += 1;
            while self.pos < len && self.c(self.pos) != DQUOTE {
                if self.c(self.pos) == BACKSLASH {
                    self.pos += 1;
                }
                // 反斜杠在串尾时 JS 读到 `src[len]`，拼接出字面量 "undefined"（`cat <<"a\` 的定界符是 `aundefined`）
                delimiter.extend_from_slice(self.unit(self.pos));
                self.pos += 1;
            }
            if self.pos < len {
                self.pos += 1;
            }
            self.here_quoted = true;
        } else if first == BACKSLASH {
            while self.pos < len {
                let c = self.c(self.pos);
                if is_meta(c) {
                    break;
                }
                if c == BACKSLASH {
                    self.pos += 1;
                }
                if self.pos < len {
                    delimiter.extend_from_slice(self.unit(self.pos));
                    self.pos += 1;
                }
            }
            self.here_quoted = true;
        } else {
            let start = self.pos;
            while self.pos < len && !is_meta(self.c(self.pos)) {
                self.pos += 1;
            }
            delimiter.extend_from_slice(self.slice(start, self.pos));
            self.here_quoted = false;
        }
        self.here_delim = delimiter;
    }

    pub(super) fn push_pending_heredoc(&mut self, strip: bool) {
        self.pending.push(PendingHereDoc {
            delimiter: self.here_delim.clone(),
            strip,
            quoted: self.here_quoted,
            target: None,
        });
    }

    /// `registerHereDocTarget`：把重定向绑定到第一个尚未绑定的待读 heredoc。
    pub(crate) fn register_heredoc_target(&mut self, target: usize) {
        if let Some(pending) = self
            .pending
            .iter_mut()
            .find(|pending| pending.target.is_none())
        {
            pending.target = Some(target);
        }
    }

    pub(super) fn consume_pending_heredocs(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for heredoc in pending {
            let body_pos = self.pos;
            let body_end = self.read_heredoc_body(&heredoc.delimiter, heredoc.strip);
            let Some(target) = heredoc.target else {
                continue;
            };
            if heredoc.quoted || body_end <= body_pos {
                continue;
            }
            if body_may_expand(self.slice(body_pos, body_end)) {
                self.heredoc_bodies.push(HereDocBody {
                    target,
                    pos: body_pos,
                    end: body_end,
                });
            }
        }
    }

    /// `readHereDocBody`：返回正文结束位置（正文从调用时的 `pos` 开始）。
    fn read_heredoc_body(&mut self, delimiter: &[u8], strip: bool) -> usize {
        let len = self.src_end;
        while self.pos < len {
            let mut line_start = self.pos;
            let line_end = self.src[self.pos.min(self.src.len())..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(len, |offset| self.pos + offset);
            if strip {
                while line_start < line_end && self.c(line_start) == TAB {
                    line_start += 1;
                }
            }
            let body_end = self.pos;
            self.pos = if line_end < len {
                line_end + 1
            } else {
                line_end
            };
            if line_end - line_start == delimiter.len()
                && self.slice(line_start, line_end) == delimiter
            {
                return body_end;
            }
        }
        self.pos
    }

    /// `buildHereDocParts`：未加引号的 heredoc 正文里只有 `$…` 与反引号有特殊含义。
    pub(crate) fn build_heredoc_parts(
        &mut self,
        body_pos: usize,
        body_end: usize,
    ) -> Option<Vec<Part>> {
        let mut parts = Vec::new();
        let mut lit = Vec::new();
        let flush = |parts: &mut Vec<Part>, lit: &mut Vec<u8>| {
            if !lit.is_empty() {
                parts.push(Part::Literal(std::mem::take(lit)));
            }
        };
        let mut i = body_pos;
        while i < body_end {
            let ch = self.c(i);
            if ch == BACKSLASH {
                if i + 1 < body_end && matches!(self.c(i + 1), DOLLAR | BACKTICK | BACKSLASH) {
                    lit.extend_from_slice(self.unit(i + 1));
                    i += 2;
                } else {
                    lit.push(b'\\');
                    i += 1;
                }
                continue;
            }
            if ch == DOLLAR || ch == BACKTICK {
                flush(&mut parts, &mut lit);
                self.pos = i;
                if ch == DOLLAR {
                    self.read_dollar();
                } else {
                    self.read_backtick_expansion();
                }
                match self.result_part.take() {
                    Some(part) => parts.push(part),
                    None => lit.extend_from_slice(self.slice(i, self.pos)),
                }
                i = self.pos;
                continue;
            }
            lit.extend_from_slice(self.unit(i));
            i += 1;
        }
        flush(&mut parts, &mut lit);
        let keep = parts.len() > 1
            || parts
                .first()
                .is_some_and(|part| !matches!(part, Part::Literal(_)));
        keep.then_some(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::body_may_expand;

    #[test]
    fn quick_scan_matches_unbash() {
        assert!(body_may_expand(b"a $HOME"));
        assert!(body_may_expand(b"`x`"));
        assert!(!body_may_expand(b"\\$HOME"));
        assert!(!body_may_expand(b"$ x $'y'"));
        assert!(body_may_expand(b"$'$x'"));
    }
}
