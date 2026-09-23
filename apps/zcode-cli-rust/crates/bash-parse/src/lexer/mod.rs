//! unbash 4.0.1 `Lexer`（dist/lexer.js）的移植。
//!
//! 只实现权限分析会走到的路径：Normal / CommandStart 上下文、单词及各类引号/展开的扫描、重定向和
//! heredoc。位置一律使用 UTF-8 字节偏移：unbash 只对 ASCII 字符做结构判断，非 ASCII 码元在 JS 与
//! 这里都被当作普通字符，所以字节偏移与 UTF-16 偏移一一对应，切片结果相同。
//!
//! 与 JS 一样，所有"上一次扫描结果"（`word_text`、`redirect_target_pos` 等）都是跨 token 保留的
//! 可变状态；两个 token 槽位 `slots` 轮换使用且 `set_token` 不重置 `target_pos/target_end`。
//! unbash 的若干怪癖正是依赖这些陈旧状态（见 `redirect.rs`），因此必须原样保留。

mod chars;
mod dollar;
mod heredoc;
mod nested;
mod redirect;
mod scan;
mod token;
mod word;

use crate::parts::Part;
pub(crate) use chars::*;
use token::reserved_word;
pub(crate) use token::{Ctx, Tk, Token};

struct PendingHereDoc {
    delimiter: Vec<u8>,
    strip: bool,
    quoted: bool,
    target: Option<usize>,
}

/// heredoc 正文被读取后回填到重定向上的结果（仅未加引号且快速扫描发现展开时存在正文单词）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct HereDocBody {
    pub target: usize,
    pub pos: usize,
    pub end: usize,
}

pub(crate) struct Lexer<'a> {
    src: &'a [u8],
    src_end: usize,
    pos: usize,
    slots: [Token; 2],
    cur: usize,
    has_peek: bool,
    pending: Vec<PendingHereDoc>,
    pub(crate) heredoc_bodies: Vec<HereDocBody>,
    pub(crate) errors: usize,
    /// unbash 在此输入上会死循环（见 `read_ansi_c_quoted`）；Node 永远拿不到结果。
    pub(crate) hung: bool,
    /// 嵌套深度超过 Node 可能栈溢出的范围（见 `nested.rs`），按 parse error 处理。
    pub(crate) overflow: bool,
    bp: bool,
    word_text: Vec<u8>,
    word_quoted: bool,
    word_has_exp: bool,
    word_parts: Option<Vec<Part>>,
    redirect_target_pos: usize,
    result_text: Vec<u8>,
    result_has_exp: bool,
    result_part: Option<Part>,
    dq_text: Vec<u8>,
    dq_has_exp: bool,
    dq_dynamic: bool,
    here_delim: Vec<u8>,
    here_quoted: bool,
    brace_memo: Option<scan::BraceMemo>,
}

impl<'a> Lexer<'a> {
    /// `new Lexer(src)`：从 0 开始，跳过 `#!` shebang 行。
    pub(crate) fn new(src: &'a [u8]) -> Self {
        let mut lexer = Self::bounded(src, 0, src.len(), false);
        if src.starts_with(b"#!") {
            lexer.pos = src
                .iter()
                .position(|&b| b == b'\n')
                .map_or(lexer.src_end, |nl| nl + 1);
        }
        lexer
    }

    /// `new Lexer(src, start, end)`，`build_parts` 对应 `_buildParts`（重新扫描单词结构时为真）。
    pub(crate) fn bounded(src: &'a [u8], start: usize, end: usize, build_parts: bool) -> Self {
        Self {
            src,
            src_end: end,
            pos: start,
            slots: [Token::default(), Token::default()],
            cur: 0,
            has_peek: false,
            pending: Vec::new(),
            heredoc_bodies: Vec::new(),
            errors: 0,
            hung: false,
            overflow: false,
            bp: build_parts,
            word_text: Vec::new(),
            word_quoted: false,
            word_has_exp: false,
            word_parts: None,
            redirect_target_pos: 0,
            result_text: Vec::new(),
            result_has_exp: false,
            result_part: None,
            dq_text: Vec::new(),
            dq_has_exp: false,
            dq_dynamic: false,
            here_delim: Vec::new(),
            here_quoted: false,
            brace_memo: None,
        }
    }

    /// `src.charCodeAt(i)`：读取整个源串（不受 `src_end` 约束），越界为 NaN。
    fn c(&self, i: usize) -> u32 {
        self.src.get(i).map_or(NAN, |&b| u32::from(b))
    }

    /// `src[i]`：越界时 JS 得到 `undefined`，拼接进字符串后就是字面量 "undefined"。
    fn unit(&self, i: usize) -> &'a [u8] {
        let src: &'a [u8] = self.src;
        src.get(i..i + 1).unwrap_or(b"undefined")
    }

    fn slice(&self, start: usize, end: usize) -> &'a [u8] {
        crate::js::slice(self.src, start, end)
    }

    fn set_token(&mut self, slot: usize, kind: Tk, value: &[u8], pos: usize, end: usize) {
        let token = &mut self.slots[slot];
        token.kind = kind;
        token.value.clear();
        token.value.extend_from_slice(value);
        token.pos = pos;
        token.end = end;
        token.fd = None;
        token.content = None;
    }

    pub(crate) fn peek(&mut self, ctx: Ctx) -> &Token {
        let slot = 1 - self.cur;
        if !self.has_peek {
            self.read_next(slot, ctx);
            self.has_peek = true;
        }
        &self.slots[slot]
    }

    pub(crate) fn next(&mut self, ctx: Ctx) -> &Token {
        if self.has_peek {
            self.has_peek = false;
            self.cur = 1 - self.cur;
        } else {
            self.read_next(self.cur, ctx);
        }
        &self.slots[self.cur]
    }

    fn read_next(&mut self, slot: usize, ctx: Ctx) {
        let len = self.src_end;
        loop {
            let mut pos = self.pos;
            while pos < len {
                let ch = self.c(pos);
                if ch == SPACE || ch == TAB {
                    pos += 1;
                } else if ch == BACKSLASH && pos + 1 < len && self.c(pos + 1) == NL {
                    pos += 2;
                } else {
                    break;
                }
            }
            self.pos = pos;
            if pos >= len {
                self.set_token(slot, Tk::Eof, b"", pos, pos);
                return;
            }
            let ch = self.c(pos);
            if ch == HASH {
                // 注释：跳到行尾后重新读取（JS 中是递归调用 readNext）
                while self.pos < len && self.c(self.pos) != NL {
                    self.pos += 1;
                }
                continue;
            }
            if ch == NL {
                self.pos += 1;
                self.consume_pending_heredocs();
                self.set_token(slot, Tk::Newline, b"\n", pos, self.pos);
                return;
            }
            if !self.try_read_operator(slot, ch, ctx, pos) {
                self.read_word(slot, ctx, pos);
            }
            return;
        }
    }

    fn try_read_operator(&mut self, slot: usize, ch: u32, ctx: Ctx, start: usize) -> bool {
        let pos = self.pos;
        let next = if pos + 1 < self.src_end {
            self.c(pos + 1)
        } else {
            0
        };
        let (kind, value, width): (Tk, &[u8], usize) = match ch {
            SEMI if next == SEMI && pos + 2 < self.src_end && self.c(pos + 2) == AMP => {
                (Tk::DoubleSemiAmp, b";;&", 3)
            }
            SEMI if next == SEMI => (Tk::DoubleSemi, b";;", 2),
            SEMI if next == AMP => (Tk::SemiAmp, b";&", 2),
            SEMI => (Tk::Semi, b";", 1),
            PIPE if next == PIPE => (Tk::Or, b"||", 2),
            PIPE if next == AMP => (Tk::Pipe, b"|&", 2),
            PIPE => (Tk::Pipe, b"|", 1),
            AMP if next == AMP => (Tk::And, b"&&", 2),
            AMP if next == GT => {
                // `&>` / `&>>`：重定向而非后台
                self.pos += 2;
                let append = self.pos < self.src_end && self.c(self.pos) == GT;
                if append {
                    self.pos += 1;
                }
                self.skip_spaces_and_tabs();
                self.redirect_target_pos = self.pos;
                if self.pos < self.src_end && self.c(self.pos) != NL {
                    self.read_word_text();
                }
                self.redirect_token(slot, if append { b"&>>" } else { b"&>" }, start);
                return true;
            }
            AMP => (Tk::Amp, b"&", 1),
            LPAREN if ctx == Ctx::CommandStart && next == LPAREN => {
                let body = self.scan_arithmetic_body();
                self.set_token(slot, Tk::ArithCmd, &body, start, self.pos);
                return true;
            }
            LPAREN => (Tk::LParen, b"(", 1),
            RPAREN => (Tk::RParen, b")", 1),
            LT | GT => {
                self.read_redirection(slot, start);
                return true;
            }
            _ => return false,
        };
        self.pos += width;
        self.set_token(slot, kind, value, start, self.pos);
        true
    }

    fn skip_spaces_and_tabs(&mut self) {
        while self.pos < self.src_end {
            let ch = self.c(self.pos);
            if ch == SPACE || ch == TAB {
                self.pos += 1;
            } else if ch == BACKSLASH && self.pos + 1 < self.src_end && self.c(self.pos + 1) == NL {
                self.pos += 2;
            } else {
                break;
            }
        }
    }
}
