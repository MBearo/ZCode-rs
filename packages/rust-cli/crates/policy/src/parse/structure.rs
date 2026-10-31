//! 结构化解析（工作线 F76，docs/specs/rust-cli/shell-permissions.md §4.5.4）：复合命令不停下。子 shell、if / elif /
//! else、while / until、for / select 的条件、分支与循环体照 unbash 的文法读成命令列表，里面的简单命令照常收集；
//! `[[ … ]]` 读成一条名字是 `[[` 的命令；花括号组、`(( … ))`、函数体照常读，但记为不能拆。`case`、`for (( … ))`、
//! `coproc` 记为不能拆后停下。不能拆的写法与有没有子 shell 记在 `Script::shape`，询问的文字在 `bash_compound`。
use super::{Bail, Cmd, Parser};
use crate::consts::SIGIL_PAIR_LEN;
use crate::parse::SubstitutionKind;
use crate::parse::js::slice;
use crate::parse::lexer::{Ctx, Tk};
use crate::parse::nodes::{CommandNode, Complex, Loop, WordRef};
use crate::parse::parts::word_structure;

/// 复合命令的最多嵌套层数，超过按解析出错。
const MAX_DEPTH: usize = 64;

/// 一段命令列表在哪种结构里：决定哪些分隔符不能静态检查。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// 子 shell：`;` 与 `&` 都不行，换行可以。
    Subshell,
    /// 条件、分支、循环体、花括号组：`&` 不行。
    Body,
}

impl Parser<'_> {
    fn note(&mut self, pos: usize, complex: Complex) {
        self.script.shape.complex.push((pos, complex));
    }

    /// 复合命令；当前 token 不是复合命令的开头时 None（交给简单命令）。
    pub(super) fn compound(&mut self) -> Result<Option<Cmd>, Bail> {
        let token = self.lexer.peek(Ctx::CommandStart);
        let (kind, pos) = (token.kind, token.pos);
        let redirected = match kind {
            Tk::LParen => Complex::Subshell,
            Tk::LBrace | Tk::ArithCmd => Complex::Braces,
            Tk::If => Complex::If,
            Tk::While | Tk::Until => Complex::While,
            Tk::For | Tk::Select => Complex::For,
            Tk::Function => Complex::Function,
            Tk::DblLBracket => return self.test_command().map(Some),
            Tk::Case => {
                self.note(pos, Complex::Case);
                return Err(Bail("Case"));
            }
            Tk::Coproc => {
                self.note(pos, Complex::Generic);
                return Err(Bail("Coproc"));
            }
            _ => return Ok(None),
        };
        if self.depth >= MAX_DEPTH {
            return self.syntax();
        }
        self.depth += 1;
        self.lexer.next(Ctx::CommandStart);
        match kind {
            Tk::LParen => {
                self.script.shape.subshell = true;
                self.body(Frame::Subshell)?;
                self.expect(Tk::RParen)?;
            }
            Tk::LBrace => {
                self.note(pos, Complex::Braces);
                self.body(Frame::Body)?;
                self.expect(Tk::RBrace)?;
            }
            Tk::ArithCmd => self.note(pos, Complex::Braces),
            Tk::If => self.if_clause()?,
            Tk::While | Tk::Until => {
                self.body(Frame::Body)?;
                self.do_group()?;
            }
            Tk::For | Tk::Select => self.for_clause(kind == Tk::Select, pos)?,
            _ => self.function_keyword(pos)?,
        }
        self.depth -= 1;
        self.trailing_redirects(pos, redirected);
        Ok(Some(Cmd::Compound))
    }

    /// 复合命令后面的重定向：整条不能拆，理由是那个结构的说法。
    fn trailing_redirects(&mut self, pos: usize, complex: Complex) {
        if self.peek_kind(Ctx::Normal) != Tk::Redirect {
            return;
        }
        self.note(pos, complex);
        let mut redirects = Vec::new();
        while self.peek_kind(Ctx::Normal) == Tk::Redirect {
            self.collect_redirect(&mut redirects, Ctx::Normal);
        }
    }

    /// 结构里的命令列表：读到 `then`、`fi`、`do`、`done`、`)`、`}` 等终止符为止（终止符留给调用方）。
    fn body(&mut self, frame: Frame) -> Result<(), Bail> {
        self.skip_newlines(Ctx::CommandStart);
        while self.starts_statement() {
            self.script.backgrounds.push(false);
            self.and_or()?;
            let token = self.lexer.peek(Ctx::Normal);
            let (kind, pos) = (token.kind, token.pos);
            if !matches!(kind, Tk::Semi | Tk::Amp | Tk::Newline) {
                break;
            }
            self.lexer.next(Ctx::Normal);
            if kind == Tk::Amp || (kind == Tk::Semi && frame == Frame::Subshell) {
                self.note(pos, Complex::Generic);
            }
            self.skip_newlines(Ctx::CommandStart);
        }
        Ok(())
    }

    /// 取走一个保留字或终止符；不是它时按解析出错。
    fn expect(&mut self, kind: Tk) -> Result<(), Bail> {
        if self.peek_kind(Ctx::CommandStart) != kind {
            return self.syntax();
        }
        self.lexer.next(Ctx::CommandStart);
        Ok(())
    }

    /// `if` 已取走：条件、`then` 与分支、任意个 `elif`、可选的 `else`、`fi`。
    fn if_clause(&mut self) -> Result<(), Bail> {
        self.body(Frame::Body)?;
        self.expect(Tk::Then)?;
        self.body(Frame::Body)?;
        loop {
            match self.peek_kind(Ctx::CommandStart) {
                Tk::Elif => {
                    self.lexer.next(Ctx::CommandStart);
                    self.body(Frame::Body)?;
                    self.expect(Tk::Then)?;
                    self.body(Frame::Body)?;
                }
                Tk::Else => {
                    self.lexer.next(Ctx::CommandStart);
                    self.body(Frame::Body)?;
                }
                _ => break,
            }
        }
        self.expect(Tk::Fi)
    }

    /// `do` 列表 `done`；返回 `do` 之后的位置。
    fn do_group(&mut self) -> Result<usize, Bail> {
        if self.peek_kind(Ctx::CommandStart) != Tk::Do {
            return self.syntax();
        }
        let start = self.lexer.next(Ctx::CommandStart).end;
        self.body(Frame::Body)?;
        self.expect(Tk::Done)?;
        Ok(start)
    }

    /// `for` / `select` 已取走：名字、可选的 `in` 词表、分隔符、`do` 列表 `done`。`for (( … ))` 记为不能拆后停下。
    fn for_clause(&mut self, select: bool, pos: usize) -> Result<(), Bail> {
        if !select && self.peek_kind(Ctx::Normal) == Tk::LParen {
            self.note(pos, Complex::CountingFor);
            return Err(Bail("ArithmeticFor"));
        }
        if select {
            self.note(pos, Complex::Select);
        }
        let token = self.lexer.next(Ctx::Normal);
        if token.kind != Tk::Word {
            return self.syntax();
        }
        let name = slice(self.source, token.pos, token.end).to_vec();
        if self.peek_kind(Ctx::CommandStart) == Tk::In {
            self.lexer.next(Ctx::CommandStart);
            while self.peek_kind(Ctx::Normal) == Tk::Word {
                let word = self.lexer.next(Ctx::Normal);
                let (start, end) = (word.pos, word.end);
                self.loop_word(start, end);
            }
        }
        // 用 CommandStart 预读：分隔符之后的 `do` 要按保留字认出来。
        while matches!(self.peek_kind(Ctx::CommandStart), Tk::Semi | Tk::Newline) {
            self.lexer.next(Ctx::CommandStart);
        }
        let body = self.do_group()?;
        self.script.shape.loops.push(Loop { name, pos, body });
        Ok(())
    }

    /// for 的词表里的一个词：有替换或展开时不能拆（只认出种类，shell-permissions.md D22）。
    fn loop_word(&mut self, pos: usize, end: usize) {
        let structure = word_structure(self.source, pos, end);
        self.script.hung |= structure.hung;
        let text = slice(self.source, pos, end);
        let complex = match structure.subs.first() {
            Some(sub) if sub.kind == SubstitutionKind::Process => Complex::AsFile,
            Some(_) => Complex::Nested,
            None if !structure.dynamic => return,
            None if text.windows(SIGIL_PAIR_LEN).any(|pair| pair == b"${") => {
                Complex::VariableBraces
            }
            None if text.contains(&b'$') => Complex::Variable,
            None => Complex::BracePattern,
        };
        self.note(pos, complex);
    }

    /// `function` 已取走：名字、可选的 `()`、函数体。
    fn function_keyword(&mut self, pos: usize) -> Result<(), Bail> {
        if self.lexer.next(Ctx::Normal).kind != Tk::Word {
            return self.syntax();
        }
        // 用 CommandStart 预读：没有 `()` 时下一个就是函数体，`{` 要按保留字认出来。
        if self.peek_kind(Ctx::CommandStart) == Tk::LParen {
            self.lexer.next(Ctx::Normal);
            if self.peek_kind(Ctx::Normal) != Tk::RParen {
                return self.syntax();
            }
            self.lexer.next(Ctx::Normal);
        }
        self.function_body(pos).map(|_| ())
    }

    /// 函数定义：记为不能拆，函数体（一个复合命令）照常读，里面的命令进每一段的检查。
    pub(super) fn function_body(&mut self, pos: usize) -> Result<Cmd, Bail> {
        self.note(pos, Complex::Function);
        self.skip_newlines(Ctx::CommandStart);
        match self.compound()? {
            Some(_) => Ok(Cmd::Compound),
            None => self.syntax(),
        }
    }

    /// `[[ … ]]`：读成一条名字是 `[[` 的命令（文字到 `]]` 为止，参数是中间的词；`&&`、`||`、括号不算参数）。
    fn test_command(&mut self) -> Result<Cmd, Bail> {
        let token = self.lexer.next(Ctx::CommandStart);
        let (pos, open) = (token.pos, token.end);
        let name = WordRef {
            text: slice(self.source, pos, open).to_vec(),
            pos,
            end: open,
        };
        let mut suffix = Vec::new();
        let mut redirects = Vec::new();
        let end = loop {
            match self.peek_kind(Ctx::Normal) {
                Tk::DblRBracket => break self.lexer.next(Ctx::Normal).end,
                Tk::Word | Tk::Assignment => suffix.push(self.read_word(Ctx::Normal)),
                // unbash 的词法把 `<` / `>` 当作重定向：按写入检查（比只看字面严）。
                Tk::Redirect => {
                    self.collect_redirect(&mut redirects, Ctx::Normal);
                }
                Tk::Eof => return self.syntax(),
                _ => {
                    self.lexer.next(Ctx::Normal);
                }
            }
        };
        self.trailing_redirects(pos, Complex::Test);
        Ok(Cmd::Simple(CommandNode {
            pos,
            end,
            name: Some(name),
            prefix: Vec::new(),
            suffix,
            redirects,
        }))
    }
}
