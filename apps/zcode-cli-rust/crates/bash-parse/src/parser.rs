//! unbash 4.0.1 `Parser`（dist/parser.js）中 list / and_or / pipeline / 简单命令的移植。
//!
//! peek 缓存与上下文无关：先以 CommandStart 预读的 token 之后用 Normal 取出时仍保持原来的类型，
//! 这决定了保留字、赋值和 `time` 的识别位置，因此 peek/next 的调用顺序与上下文与 JS 逐一对应。
//!
//! 复合命令（子 shell、花括号组、if/for/while/case/select、函数、coproc、`[[`、`((`）一经识别
//! 即中止解析并记录节点类型：Node 在这些节点上必然判定为不支持的语法，其余字段不再被使用。

use crate::Operator;
use crate::js::slice;
use crate::lexer::{Ctx, Lexer, Tk};
use crate::nodes::{
    AssignmentNode, CommandNode, OpBefore, ParsedCommand, RedirectNode, Script, WordRef,
};

struct Bail(&'static str);

struct Parser<'a> {
    source: &'a [u8],
    lexer: Lexer<'a>,
    script: Script,
}

pub(crate) fn parse(source: &[u8]) -> Script {
    let mut parser = Parser {
        source,
        lexer: Lexer::new(source),
        script: Script::default(),
    };
    if let Err(Bail(kind)) = parser.list() {
        parser.script.bail = Some(kind);
    }
    let Parser {
        lexer, mut script, ..
    } = parser;
    for body in &lexer.heredoc_bodies {
        if let Some(redirect) = script.redirects.get_mut(body.target) {
            redirect.body = Some(*body);
        }
    }
    script.lexer_errors = lexer.errors;
    script.hung |= lexer.hung;
    script.overflow |= lexer.overflow;
    script
}

fn redirect_operator(value: &[u8]) -> &'static str {
    match value {
        b">>" => ">>",
        b"<" => "<",
        b"<<" => "<<",
        b"<<-" => "<<-",
        b"<<<" => "<<<",
        b"<>" => "<>",
        b"<&" => "<&",
        b">&" => ">&",
        b">|" => ">|",
        b"&>" => "&>",
        b"&>>" => "&>>",
        _ => ">",
    }
}

impl Parser<'_> {
    fn peek_kind(&mut self, ctx: Ctx) -> Tk {
        self.lexer.peek(ctx).kind
    }

    fn skip_newlines(&mut self, ctx: Ctx) {
        while self.peek_kind(ctx) == Tk::Newline {
            self.lexer.next(ctx);
        }
    }

    fn starts_statement(&mut self) -> bool {
        let kind = self.peek_kind(Ctx::CommandStart);
        !kind.is_list_terminator() && kind.is_command_start()
    }

    /// 顶层 `list()`：`and_or ((';' | '&' | NEWLINE) and_or)*`，遇到其他 token 直接结束
    /// （之后的内容被静默丢弃，例如 `echo x;; rm` 只有 `echo x`）。
    fn list(&mut self) -> Result<(), Bail> {
        self.skip_newlines(Ctx::CommandStart);
        if !self.starts_statement() {
            return Ok(());
        }
        self.script.backgrounds.push(false);
        self.and_or()?;
        loop {
            let kind = self.peek_kind(Ctx::Normal);
            if !matches!(kind, Tk::Semi | Tk::Newline | Tk::Amp) {
                return Ok(());
            }
            self.lexer.next(Ctx::Normal);
            if kind == Tk::Amp
                && let Some(last) = self.script.backgrounds.last_mut()
            {
                *last = true;
            }
            self.skip_newlines(Ctx::CommandStart);
            if !self.starts_statement() {
                return Ok(());
            }
            self.script.backgrounds.push(false);
            self.and_or()?;
        }
    }

    fn and_or(&mut self) -> Result<bool, Bail> {
        if !self.pipeline(OpBefore::Inherit)? {
            return Ok(false);
        }
        while matches!(self.peek_kind(Ctx::Normal), Tk::And | Tk::Or) {
            let op = if self.lexer.next(Ctx::Normal).kind == Tk::And {
                Operator::And
            } else {
                Operator::Or
            };
            self.skip_newlines(Ctx::CommandStart);
            if !self.pipeline(OpBefore::Op(op))? {
                break;
            }
        }
        Ok(true)
    }

    /// `pipeline()`：`['time' ['-p']] ['!'] command ('|' newlines command)*`。
    /// 返回是否产生了节点（仅有 time/! 而无命令时是空 Pipeline，同样算产生）。
    fn pipeline(&mut self, first_op: OpBefore) -> Result<bool, Bail> {
        let mut timed = false;
        if self.is_word(b"time") {
            timed = true;
            self.lexer.next(Ctx::CommandStart);
            if self.is_word(b"-p") {
                self.lexer.next(Ctx::CommandStart);
            }
        }
        let negated = self.peek_kind(Ctx::CommandStart) == Tk::Bang;
        if negated {
            self.lexer.next(Ctx::CommandStart);
        }
        let Some(first) = self.command()? else {
            return Ok(timed || negated);
        };
        self.emit(first_op, first);
        // JS 在命令缺失时也会压入 operator，之后的命令按下标取 `operators[i - 1]`，这里照搬错位。
        let mut operators = Vec::new();
        let mut count = 1;
        while self.peek_kind(Ctx::Normal) == Tk::Pipe {
            let all = self.lexer.next(Ctx::Normal).value == b"|&";
            operators.push(if all {
                Operator::PipeAll
            } else {
                Operator::Pipe
            });
            self.skip_newlines(Ctx::CommandStart);
            if let Some(command) = self.command()? {
                self.emit(OpBefore::Op(operators[count - 1]), command);
                count += 1;
            }
        }
        Ok(true)
    }

    fn is_word(&mut self, value: &[u8]) -> bool {
        let token = self.lexer.peek(Ctx::CommandStart);
        token.kind == Tk::Word && token.value == value
    }

    fn emit(&mut self, op: OpBefore, node: CommandNode) {
        let statement = self.script.backgrounds.len() - 1;
        self.script.commands.push(ParsedCommand {
            statement,
            op,
            node,
        });
    }

    fn command(&mut self) -> Result<Option<CommandNode>, Bail> {
        let kind = match self.peek_kind(Ctx::CommandStart) {
            Tk::Word | Tk::Assignment | Tk::Redirect => return self.simple_command(),
            Tk::LParen => "Subshell",
            Tk::LBrace => "BraceGroup",
            Tk::If => "If",
            Tk::For => {
                self.lexer.next(Ctx::CommandStart);
                if self.peek_kind(Ctx::Normal) == Tk::LParen {
                    "ArithmeticFor"
                } else {
                    "For"
                }
            }
            Tk::While | Tk::Until => "While",
            Tk::Case => "Case",
            Tk::Function => "Function",
            Tk::Select => "Select",
            Tk::DblLBracket => "TestCommand",
            Tk::ArithCmd => "ArithmeticCommand",
            Tk::Coproc => "Coproc",
            _ => return Ok(None),
        };
        Err(Bail(kind))
    }

    /// `simpleCommandOrFunction()`。
    fn simple_command(&mut self) -> Result<Option<CommandNode>, Bail> {
        let pos = self.lexer.peek(Ctx::CommandStart).pos;
        let mut end = pos;
        let mut prefix = Vec::new();
        let mut redirects = Vec::new();
        while self.peek_kind(Ctx::CommandStart) == Tk::Assignment {
            let token = self.lexer.next(Ctx::CommandStart);
            let (start, token_end) = (token.pos, token.end);
            end = token_end;
            prefix.push(self.parse_assignment(start, token_end));
        }
        while self.peek_kind(Ctx::CommandStart) == Tk::Redirect {
            end = self.collect_redirect(&mut redirects, Ctx::CommandStart);
        }
        if self.peek_kind(Ctx::Normal) != Tk::Word {
            // 没有命令名：只有前缀赋值时保留重定向，否则连重定向一起丢弃（`> $(x)` 得到空命令）。
            if prefix.is_empty() {
                redirects.clear();
            }
            let node = CommandNode {
                pos,
                end,
                name: None,
                prefix,
                suffix: Vec::new(),
                redirects,
            };
            return Ok(Some(node));
        }
        let name = self.read_word(Ctx::Normal);
        end = name.end;
        if self.peek_kind(Ctx::Normal) == Tk::LParen {
            self.lexer.next(Ctx::Normal);
            if self.peek_kind(Ctx::Normal) == Tk::RParen {
                return Err(Bail("Function"));
            }
        }
        let mut suffix = Vec::new();
        loop {
            match self.peek_kind(Ctx::Normal) {
                Tk::Word | Tk::Assignment => {
                    let word = self.read_word(Ctx::Normal);
                    end = word.end;
                    suffix.push(word);
                }
                Tk::Redirect => end = self.collect_redirect(&mut redirects, Ctx::Normal),
                _ => break,
            }
        }
        let node = CommandNode {
            pos,
            end,
            name: Some(name),
            prefix,
            suffix,
            redirects,
        };
        Ok(Some(node))
    }

    fn read_word(&mut self, ctx: Ctx) -> WordRef {
        let token = self.lexer.next(ctx);
        let (pos, end) = (token.pos, token.end);
        WordRef {
            text: slice(self.source, pos, end).to_vec(),
            pos,
            end,
        }
    }

    /// `collectRedirect()`，返回该重定向的结束位置。
    fn collect_redirect(&mut self, redirects: &mut Vec<usize>, ctx: Ctx) -> usize {
        let token = self.lexer.next(ctx);
        let operator = redirect_operator(&token.value);
        let target = WordRef {
            text: token.content.clone().unwrap_or_default(),
            pos: token.target_pos,
            end: token.target_end,
        };
        let (fd, end) = (token.fd, token.end);
        let index = self.script.redirects.len();
        self.script.redirects.push(RedirectNode {
            operator,
            fd,
            target,
            body: None,
        });
        if matches!(operator, "<<" | "<<-") {
            self.lexer.register_heredoc_target(index);
        }
        redirects.push(index);
        end
    }

    /// `parseAssignment()`：作用在源码切片上（不是去引号文本），所以 `"FOO"=1` 的 name 带引号。
    fn parse_assignment(&mut self, pos: usize, end: usize) -> AssignmentNode {
        let text = slice(self.source, pos, end);
        let Some(eq) = text.iter().position(|&b| b == b'=').filter(|&eq| eq > 0) else {
            return AssignmentNode {
                name: None,
                value: None,
            };
        };
        let mut name_end = eq;
        if text[eq - 1] == b'+' {
            name_end = eq - 1;
        }
        if let Some(bracket) = text.iter().position(|&b| b == b'[')
            && bracket > 0
            && bracket < name_end
            && let Some(offset) = text[bracket..].iter().position(|&b| b == b']')
            && offset > 0
            && bracket + offset + 1 == name_end
        {
            name_end = bracket;
        }
        let name = Some(text[..name_end].to_vec());
        let value_text = &text[eq + 1..];
        if value_text.len() >= 2
            && value_text[0] == b'('
            && value_text[value_text.len() - 1] == b')'
        {
            self.scan_array_elements(&value_text[1..value_text.len() - 1]);
            return AssignmentNode { name, value: None };
        }
        let value = WordRef {
            text: value_text.to_vec(),
            pos: pos + eq + 1,
            end,
        };
        AssignmentNode {
            name,
            value: Some(value),
        }
    }

    /// `parseArrayElements()`：用独立 lexer 扫描数组内部。元素不参与分析，但扫描本身可能触发
    /// unbash 的死循环（`a=($'\)`），必须照做以便识别。
    fn scan_array_elements(&mut self, inner: &[u8]) {
        let mut lexer = Lexer::new(inner);
        while lexer.peek(Ctx::Normal).kind != Tk::Eof {
            lexer.next(Ctx::Normal);
        }
        self.script.hung |= lexer.hung;
        self.script.overflow |= lexer.overflow;
    }
}
