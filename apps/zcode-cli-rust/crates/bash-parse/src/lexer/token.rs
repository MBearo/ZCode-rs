//! Token 类型（unbash `Token` / `TokenValue` / `LexContext`）。

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tk {
    Word,
    Assignment,
    Semi,
    Newline,
    Pipe,
    And,
    Or,
    Amp,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Bang,
    If,
    Then,
    Else,
    Elif,
    Fi,
    Do,
    Done,
    For,
    While,
    Until,
    In,
    Case,
    Esac,
    Function,
    DoubleSemi,
    SemiAmp,
    DoubleSemiAmp,
    Select,
    DblLBracket,
    DblRBracket,
    #[default]
    Eof,
    ArithCmd,
    Coproc,
    Redirect,
    /// unbash 用 `text in RESERVED_WORDS` 判断保留字，`toString`/`valueOf` 会命中 Object.prototype
    /// 上的同名方法，得到一个"函数 token"：既不是命令起始也不是列表终止符，解析器遇到即停止。
    ProtoFn,
}

impl Tk {
    pub(crate) fn is_list_terminator(self) -> bool {
        use Tk::*;
        matches!(
            self,
            Eof | RParen
                | RBrace
                | Then
                | Else
                | Elif
                | Fi
                | Do
                | Done
                | Esac
                | DoubleSemi
                | SemiAmp
                | DoubleSemiAmp
        )
    }

    pub(crate) fn is_command_start(self) -> bool {
        use Tk::*;
        matches!(
            self,
            Word | Assignment
                | Bang
                | LParen
                | LBrace
                | DblLBracket
                | If
                | For
                | While
                | Until
                | Case
                | Function
                | Select
                | ArithCmd
                | Coproc
                | Redirect
        )
    }
}

/// `RESERVED_WORDS`（仅在 CommandStart 上下文、未加引号且无展开时查询）。
pub(crate) fn reserved_word(text: &[u8]) -> Option<Tk> {
    Some(match text {
        b"if" => Tk::If,
        b"then" => Tk::Then,
        b"else" => Tk::Else,
        b"elif" => Tk::Elif,
        b"fi" => Tk::Fi,
        b"do" => Tk::Do,
        b"done" => Tk::Done,
        b"for" => Tk::For,
        b"while" => Tk::While,
        b"until" => Tk::Until,
        b"in" => Tk::In,
        b"case" => Tk::Case,
        b"esac" => Tk::Esac,
        b"function" => Tk::Function,
        b"select" => Tk::Select,
        b"coproc" => Tk::Coproc,
        b"!" => Tk::Bang,
        b"{" => Tk::LBrace,
        b"}" => Tk::RBrace,
        b"toString" | b"valueOf" => Tk::ProtoFn,
        _ => return None,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ctx {
    Normal,
    CommandStart,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Token {
    pub kind: Tk,
    pub value: Vec<u8>,
    pub pos: usize,
    pub end: usize,
    pub fd: Option<u32>,
    pub content: Option<Vec<u8>>,
    pub target_pos: usize,
    pub target_end: usize,
}
