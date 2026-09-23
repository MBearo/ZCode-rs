//! 单词扫描：`readWord` / `readWordText` / `buildWordParts`。

use super::{
    BACKSLASH, BACKTICK, Ctx, DOLLAR, DQUOTE, EQ, GT, LBRACE, LBRACKET, LPAREN, LT, Lexer, NL,
    PLUS, RPAREN, SQUOTE, Tk, is_extglob_prefix, is_id_continue, is_id_start, is_meta, is_special,
    reserved_word,
};
use crate::parts::Part;

fn flush(parts: &mut Vec<Part>, lit: &mut Vec<u8>) {
    if !lit.is_empty() {
        parts.push(Part::Literal(std::mem::take(lit)));
    }
}

/// 只有多于一个部件、或唯一部件不是 Literal 时才保留结构（否则 `parts` 为 undefined，value 取 text）。
fn structured(parts: Vec<Part>) -> Option<Vec<Part>> {
    let keep = parts.len() > 1
        || parts
            .first()
            .is_some_and(|part| !matches!(part, Part::Literal(_)));
    keep.then_some(parts)
}

/// `isAssignmentWord`：作用在去引号后的单词文本上。
fn is_assignment_word(text: &[u8]) -> bool {
    let Some(eq) = text.iter().position(|&b| b == b'=') else {
        return false;
    };
    if eq == 0 || !is_id_start(u32::from(text[0])) {
        return false;
    }
    let mut c = u32::from(text[0]);
    let mut i = 1;
    while i < eq {
        c = u32::from(text[i]);
        if !is_id_continue(c) {
            break;
        }
        i += 1;
    }
    if i == eq || (c == PLUS && i + 1 == eq) {
        return true;
    }
    if c == LBRACKET
        && let Some(offset) = text[i + 1..].iter().position(|&b| b == b']')
    {
        let rb = i + 1 + offset;
        return rb + 1 == eq || (text.get(rb + 1) == Some(&b'+') && rb + 2 == eq);
    }
    false
}

/// `Number.parseInt(digits, 10)`；Rust 接口是 `u32`，超出范围时饱和。
fn parse_fd(digits: &[u8]) -> u32 {
    let value = digits.iter().fold(0u64, |acc, &b| {
        acc.saturating_mul(10).saturating_add(u64::from(b - b'0'))
    });
    u32::try_from(value).unwrap_or(u32::MAX)
}

impl Lexer<'_> {
    pub(super) fn read_word(&mut self, slot: usize, ctx: Ctx, start: usize) {
        self.read_word_text();
        let text = self.word_text.clone();
        let plain = !self.word_has_exp && !self.word_quoted;
        let word_end = self.pos;
        if ctx == Ctx::CommandStart {
            if plain {
                if let Some(kind) = reserved_word(&text) {
                    self.set_token(slot, kind, &text, start, word_end);
                    return;
                }
                if text == b"[[" {
                    self.set_token(slot, Tk::DblLBracket, &text, start, word_end);
                    return;
                }
            }
            if is_assignment_word(&text) {
                self.set_token(slot, Tk::Assignment, &text, start, word_end);
                return;
            }
        }
        if plain && text == b"]]" {
            self.set_token(slot, Tk::DblRBracket, &text, start, word_end);
            return;
        }
        // 紧跟 `<`/`>` 的全数字单词是 fd 前缀，`{name}` 是变量 fd；判断用的是去引号后的文本，
        // 所以 `"2">f` 也会被当作 fd 2。
        if !self.word_has_exp && self.pos < self.src_end {
            let nc = self.c(self.pos);
            if nc == LT || nc == GT {
                if !text.is_empty() && text.iter().all(u8::is_ascii_digit) {
                    self.read_redirection(slot, start);
                    self.slots[slot].fd = Some(parse_fd(&text));
                    return;
                }
                if text.len() > 2 && text[0] == b'{' && text[text.len() - 1] == b'}' {
                    self.read_redirection(slot, start);
                    return;
                }
            }
        }
        self.set_token(slot, Tk::Word, &text, start, word_end);
    }

    pub(super) fn read_word_text(&mut self) {
        let len = self.src_end;
        let mut pos = self.pos;
        let fast_start = pos;
        while pos < len && !is_special(self.c(pos)) {
            pos += 1;
        }
        let exit = if pos < len { self.c(pos) } else { 0 };
        let extglob_open = exit == LPAREN && pos > fast_start && is_extglob_prefix(self.c(pos - 1));
        if pos >= len || (is_meta(exit) && !extglob_open) {
            self.pos = pos;
            self.word_text = self.slice(fast_start, pos).to_vec();
            self.word_quoted = false;
            self.word_has_exp = false;
            if self.bp {
                self.word_parts = None;
            }
            return;
        }
        let bp = self.bp;
        let mut text = self.slice(fast_start, pos).to_vec();
        let mut quoted = false;
        let mut has_exp = false;
        let mut parts = Vec::new();
        let mut lit = if bp { text.clone() } else { Vec::new() };
        while pos < len {
            let ch = self.c(pos);
            if !is_special(ch) {
                let run = pos;
                pos += 1;
                while pos < len && !is_special(self.c(pos)) {
                    pos += 1;
                }
                let chunk = self.slice(run, pos);
                text.extend_from_slice(chunk);
                if bp {
                    lit.extend_from_slice(chunk);
                }
                continue;
            }
            if is_meta(ch) {
                let Some(&last) = text.last() else { break };
                if ch != LPAREN || !is_extglob_prefix(u32::from(last)) {
                    break;
                }
                // extglob（或 `=(`，数组赋值）：只按括号深度配对，不理会引号
                pos += 1;
                let inner_start = pos;
                let mut depth = 1u32;
                while pos < len && depth > 0 {
                    let c = self.c(pos);
                    if c == LPAREN {
                        depth += 1;
                    } else if c == RPAREN {
                        depth -= 1;
                    }
                    pos += 1;
                }
                let mut glob = b"(".to_vec();
                glob.extend_from_slice(self.slice(inner_start, pos));
                text.extend_from_slice(&glob);
                if bp && u32::from(last) != EQ {
                    if !lit.is_empty() {
                        lit.pop();
                        flush(&mut parts, &mut lit);
                    }
                    glob.insert(0, last);
                    parts.push(Part::Dynamic(glob));
                } else if bp {
                    lit.extend_from_slice(&glob);
                }
                continue;
            }
            match ch {
                BACKSLASH => {
                    pos += 1;
                    if pos < len {
                        if self.c(pos) == NL {
                            pos += 1;
                        } else {
                            quoted = true;
                            let unit = self.unit(pos);
                            text.extend_from_slice(unit);
                            if bp {
                                lit.extend_from_slice(unit);
                            }
                            pos += 1;
                        }
                    }
                }
                SQUOTE => {
                    quoted = true;
                    pos += 1;
                    let start = pos;
                    while pos < len && self.c(pos) != SQUOTE {
                        pos += 1;
                    }
                    let value = self.slice(start, pos);
                    text.extend_from_slice(value);
                    if pos < len {
                        pos += 1;
                    } else {
                        self.errors += 1; // unterminated single quote
                    }
                    if bp {
                        flush(&mut parts, &mut lit);
                        parts.push(Part::SingleQuoted(value.to_vec()));
                    }
                }
                DQUOTE => {
                    quoted = true;
                    self.pos = pos + 1;
                    self.read_double_quoted();
                    pos = self.pos;
                    text.extend_from_slice(&self.dq_text);
                    has_exp |= self.dq_has_exp;
                    if bp {
                        flush(&mut parts, &mut lit);
                        parts.push(Part::Quoted {
                            text: self.dq_text.clone(),
                            dynamic: self.dq_dynamic,
                        });
                    }
                }
                DOLLAR => {
                    self.pos = pos;
                    self.read_dollar();
                    pos = self.pos;
                    text.extend_from_slice(&self.result_text);
                    has_exp |= self.result_has_exp;
                    if bp {
                        match self.result_part.take() {
                            Some(part) => {
                                flush(&mut parts, &mut lit);
                                parts.push(part);
                            }
                            None => lit.extend_from_slice(&self.result_text),
                        }
                    }
                }
                BACKTICK => {
                    self.pos = pos;
                    self.read_backtick_expansion();
                    pos = self.pos;
                    text.extend_from_slice(&self.result_text);
                    has_exp = true;
                    if bp {
                        flush(&mut parts, &mut lit);
                        parts.push(self.result_part.take().unwrap_or(Part::Dynamic(Vec::new())));
                    }
                }
                LBRACE => {
                    if let Some(end) = self.scan_brace_expansion(pos, len) {
                        let brace = self.slice(pos, end);
                        text.extend_from_slice(brace);
                        if bp {
                            flush(&mut parts, &mut lit);
                            parts.push(Part::Dynamic(brace.to_vec()));
                        }
                        pos = end;
                    } else {
                        text.push(b'{');
                        if bp {
                            lit.push(b'{');
                        }
                        pos += 1;
                    }
                }
                _ => pos += 1,
            }
        }
        if bp {
            flush(&mut parts, &mut lit);
        }
        self.pos = pos;
        self.word_text = text;
        self.word_quoted = quoted;
        self.word_has_exp = has_exp;
        if bp {
            self.word_parts = structured(parts);
        }
    }

    /// `buildWordParts`：在 `[start, src_end)` 内重新扫描一个单词并构建结构化部件。
    pub(crate) fn build_word_parts(&mut self, start: usize) -> Option<Vec<Part>> {
        self.pos = start;
        let ch = self.c(start);
        if (ch == LT || ch == GT) && start + 1 < self.src_end && self.c(start + 1) == LPAREN {
            self.pos = start + 2;
            self.extract_balanced();
            let substitution = Part::Dynamic(self.slice(start, self.pos).to_vec());
            if self.pos >= self.src_end {
                return Some(vec![substitution]);
            }
            self.read_word_text();
            let mut parts = self.word_parts.take().unwrap_or_default();
            parts.insert(0, substitution);
            return Some(parts);
        }
        self.read_word_text();
        self.word_parts.take()
    }
}
