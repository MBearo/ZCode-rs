//! 双引号、`$` 开头的各种展开、反引号和 ANSI-C 字符串（readDoubleQuoted / readDollar 等）。

use super::{
    AT, BACKSLASH, BACKTICK, BANG, DASH, DOLLAR, DQUOTE, HASH, LBRACE, LPAREN, Lexer, NL, PIPE,
    QUESTION, RBRACE, RPAREN, SPACE, SQUOTE, STAR, TAB, is_digit, is_id_continue, is_id_start,
};
use crate::parts::Part;

impl Lexer<'_> {
    /// `readDoubleQuoted`：调用时 `pos` 已越过开头的 `"`。
    pub(super) fn read_double_quoted(&mut self) {
        let len = self.src_end;
        let content_start = self.pos;
        if !self.bp {
            let mut p = self.pos;
            while p < len {
                let c = self.c(p);
                if c == DQUOTE {
                    self.dq_text = self.slice(content_start, p).to_vec();
                    self.pos = p + 1;
                    self.dq_has_exp = false;
                    self.dq_dynamic = false;
                    return;
                }
                if c == DOLLAR || c == BACKTICK || c == BACKSLASH {
                    break;
                }
                p += 1;
            }
        }
        let mut text = Vec::new();
        let mut has_exp = false;
        let mut dynamic = false;
        while self.pos < len && self.c(self.pos) != DQUOTE {
            let run = self.pos;
            while self.pos < len
                && !matches!(self.c(self.pos), DQUOTE | BACKSLASH | DOLLAR | BACKTICK)
            {
                self.pos += 1;
            }
            text.extend_from_slice(self.slice(run, self.pos));
            if self.pos >= len || self.c(self.pos) == DQUOTE {
                break;
            }
            match self.c(self.pos) {
                BACKSLASH => {
                    self.pos += 1;
                    if self.pos < len {
                        let next = self.c(self.pos);
                        if next == NL {
                            self.pos += 1;
                            continue;
                        }
                        if !matches!(next, DOLLAR | BACKTICK | DQUOTE | BACKSLASH) {
                            text.push(b'\\');
                        }
                        text.extend_from_slice(self.unit(self.pos));
                        self.pos += 1;
                    }
                }
                DOLLAR => {
                    // 双引号内的 `$"` 是字面 `$` 加闭合引号，不是 locale 字符串
                    if self.pos + 1 < len && self.c(self.pos + 1) == DQUOTE {
                        text.push(b'$');
                        self.pos += 1;
                        continue;
                    }
                    self.read_dollar();
                    text.extend_from_slice(&self.result_text);
                    has_exp |= self.result_has_exp;
                    // 只有展开类部件是双引号的子节点；`$'..'` 等被当作字面量并入文本
                    if matches!(self.result_part.take(), Some(Part::Dynamic(_))) {
                        dynamic = true;
                    }
                }
                _ => {
                    self.read_backtick_expansion();
                    text.extend_from_slice(&self.result_text);
                    has_exp = true;
                    dynamic = true;
                }
            }
        }
        if self.pos < len {
            self.pos += 1;
        } else {
            self.errors += 1; // unterminated double quote
        }
        self.dq_text = text;
        self.dq_has_exp = has_exp;
        self.dq_dynamic = dynamic;
    }

    fn set_result(&mut self, text: Vec<u8>, has_exp: bool, part: Option<Part>) {
        self.result_text = text;
        self.result_has_exp = has_exp;
        self.result_part = if self.bp { part } else { None };
    }

    /// `readDollar`：调用时 `pos` 指向 `$`。
    pub(super) fn read_dollar(&mut self) {
        let dollar = self.pos;
        self.pos += 1;
        let len = self.src_end;
        if self.pos >= len {
            self.set_result(b"$".to_vec(), false, None);
            return;
        }
        let ch = self.c(self.pos);
        match ch {
            LPAREN if self.pos + 1 < len && self.c(self.pos + 1) == LPAREN => {
                let body = self.scan_arithmetic_body();
                let mut text = b"$((".to_vec();
                text.extend_from_slice(&body);
                text.extend_from_slice(b"))");
                self.set_result(text.clone(), false, Some(Part::Dynamic(text)));
            }
            LPAREN => {
                self.pos += 1;
                self.extract_balanced();
                let text = self.slice(dollar, self.pos).to_vec();
                self.set_result(text.clone(), true, Some(Part::Dynamic(text)));
            }
            LBRACE => {
                let after = if self.pos + 1 < len {
                    self.c(self.pos + 1)
                } else {
                    0
                };
                if matches!(after, SPACE | TAB | NL) {
                    self.read_brace_substitution(b"${ ", 1);
                } else if after == PIPE {
                    self.read_brace_substitution(b"${| ", 2);
                } else {
                    self.read_parameter_expansion();
                }
            }
            SQUOTE => {
                self.pos += 1;
                let value = self.read_ansi_c_quoted();
                self.set_result(value.clone(), false, Some(Part::AnsiC(value)));
            }
            DQUOTE => {
                self.pos += 1;
                self.read_double_quoted();
                let text = self.dq_text.clone();
                let dynamic = self.dq_dynamic;
                self.set_result(
                    text.clone(),
                    self.dq_has_exp,
                    Some(Part::Quoted { text, dynamic }),
                );
            }
            AT | STAR | HASH | QUESTION | DASH | DOLLAR | BANG => {
                self.simple_expansion(dollar, self.pos + 1)
            }
            _ if is_digit(ch) => self.simple_expansion(dollar, self.pos + 1),
            _ if is_id_start(ch) => {
                let mut end = self.pos;
                while end < len && is_id_continue(self.c(end)) {
                    end += 1;
                }
                self.simple_expansion(dollar, end);
            }
            _ => self.set_result(b"$".to_vec(), false, None),
        }
    }

    fn simple_expansion(&mut self, dollar: usize, end: usize) {
        self.pos = end;
        let text = self.slice(dollar, end).to_vec();
        self.set_result(text.clone(), false, Some(Part::Dynamic(text)));
    }

    /// `scanArithmeticBody`：`pos` 指向 `((`，返回到匹配 `))` 之前的正文。
    pub(super) fn scan_arithmetic_body(&mut self) -> Vec<u8> {
        self.pos += 2;
        let len = self.src_end;
        let start = self.pos;
        let mut depth = 1u32;
        while self.pos < len && depth > 0 {
            let c = self.c(self.pos);
            let pair = self.pos + 1 < len && self.c(self.pos + 1) == c;
            if c == LPAREN && pair {
                depth += 1;
                self.pos += 2;
            } else if c == RPAREN && pair {
                depth -= 1;
                self.pos += 2;
            } else {
                self.pos += 1;
            }
        }
        self.slice(start, self.pos.saturating_sub(2)).to_vec()
    }

    /// `readBraceSubstitution`：`${ cmd; }` 与 `${| cmd; }`，`pos` 指向 `{`。
    fn read_brace_substitution(&mut self, prefix: &[u8], skip: usize) {
        self.pos += skip;
        let len = self.src_end;
        let start = self.pos;
        let mut depth = 1u32;
        while self.pos < len {
            match self.c(self.pos) {
                LBRACE => depth += 1,
                RBRACE => {
                    depth -= 1;
                    if depth == 0 {
                        self.pos += 1;
                        break;
                    }
                }
                SQUOTE => {
                    self.pos += 1;
                    self.skip_sq();
                    continue;
                }
                DQUOTE => {
                    self.pos += 1;
                    self.skip_dq();
                    continue;
                }
                BACKSLASH => self.pos += 1,
                _ => {}
            }
            self.pos += 1;
        }
        let raw_inner = self.slice(start, self.pos.saturating_sub(1));
        let mut text = prefix.to_vec();
        text.extend_from_slice(&crate::js::trim_bytes(raw_inner));
        text.extend_from_slice(b" }");
        self.set_result(text.clone(), true, Some(Part::Dynamic(text)));
    }

    /// `readBacktickExpansion`：`pos` 指向开头的反引号；结果文本是去掉转义后的内部命令。
    pub(super) fn read_backtick_expansion(&mut self) {
        self.pos += 1;
        let len = self.src_end;
        let start = self.pos;
        while self.pos < len && !matches!(self.c(self.pos), BACKTICK | BACKSLASH) {
            self.pos += 1;
        }
        let mut inner = self.slice(start, self.pos).to_vec();
        while self.pos < len && self.c(self.pos) != BACKTICK {
            if self.c(self.pos) == BACKSLASH {
                self.pos += 1;
                if self.pos < len {
                    if !matches!(self.c(self.pos), DOLLAR | BACKTICK | BACKSLASH) {
                        inner.push(b'\\');
                    }
                    inner.extend_from_slice(self.unit(self.pos));
                    self.pos += 1;
                }
            } else {
                let run = self.pos;
                while self.pos < len && !matches!(self.c(self.pos), BACKTICK | BACKSLASH) {
                    self.pos += 1;
                }
                inner.extend_from_slice(self.slice(run, self.pos));
            }
        }
        if self.pos < len {
            self.pos += 1;
        } else {
            self.errors += 1; // unterminated backtick
        }
        let raw = self.slice(start - 1, self.pos).to_vec();
        self.set_result(inner, true, Some(Part::Dynamic(raw)));
    }

    /// `readParameterExpansion`：`pos` 指向 `${` 的 `{`。展开内部结构不影响权限判定（任何参数
    /// 展开都算动态），因此这里只复刻扫描边界，不解析 operator/operand。
    fn read_parameter_expansion(&mut self) {
        let len = self.src_end;
        let start = self.pos;
        self.pos += 1;
        let mut depth = 1u32;
        while self.pos < len && depth > 0 {
            let ch = self.c(self.pos);
            if ch == LBRACE && self.pos > 0 && self.c(self.pos - 1) == DOLLAR {
                depth += 1;
            } else if ch == RBRACE {
                depth -= 1;
                if depth == 0 {
                    self.pos += 1;
                    break;
                }
            } else if ch == BACKSLASH {
                self.pos += 1;
            } else if ch == SQUOTE {
                self.pos += 1;
                self.skip_sq();
                continue;
            } else if ch == DQUOTE {
                self.pos += 1;
                self.skip_dq();
                continue;
            }
            self.pos += 1;
        }
        let text = self.slice(start - 1, self.pos).to_vec();
        self.set_result(text.clone(), false, Some(Part::Dynamic(text)));
    }

    /// `readAnsiCQuoted`：`pos` 已越过 `$'`。只翻译 `\n \t \r \\ \' \" \a \b \e \E \f \v`，
    /// 其余转义原样保留（`$'\x41'` 得到 `\x41`）。
    pub(super) fn read_ansi_c_quoted(&mut self) -> Vec<u8> {
        let len = self.src_end;
        let mut text = Vec::new();
        while self.pos < len && self.c(self.pos) != SQUOTE {
            if self.c(self.pos) == BACKSLASH && self.pos + 1 < len {
                self.pos += 1;
                let unit = self.unit(self.pos);
                match unit {
                    b"n" => text.push(b'\n'),
                    b"t" => text.push(b'\t'),
                    b"r" => text.push(b'\r'),
                    b"\\" => text.push(b'\\'),
                    b"'" => text.push(b'\''),
                    b"\"" => text.push(b'"'),
                    b"a" => text.push(0x07),
                    b"b" => text.push(0x08),
                    b"e" | b"E" => text.push(0x1b),
                    b"f" => text.push(0x0c),
                    b"v" => text.push(0x0b),
                    _ => {
                        text.push(b'\\');
                        text.extend_from_slice(unit);
                    }
                }
                self.pos += 1;
            } else {
                let run = self.pos;
                while self.pos < len && !matches!(self.c(self.pos), SQUOTE | BACKSLASH) {
                    self.pos += 1;
                }
                if self.pos == run {
                    // unbash bug：串尾孤立的反斜杠既不满足转义分支也无法前进，JS 在此死循环。
                    // Node 永远得不到分析结果；这里停止扫描并标记，由分析层按 parse error 处理。
                    self.hung = true;
                    self.pos = len;
                    return text;
                }
                text.extend_from_slice(self.slice(run, self.pos));
            }
        }
        if self.pos < len {
            self.pos += 1;
        }
        text
    }
}
