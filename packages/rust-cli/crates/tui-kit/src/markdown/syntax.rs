//! 回答里代码块的语法着色（docs/puffin-tui.md §4.2「回答的 Markdown」）：一个小词法器，在几种语言里分出八类记号——
//! 关键字、字符串、数字、函数、类型、常量、JSON 的键、注释——其余是普通文字。
//! 移植自 packages/tui/tui/src/zcode/tui/markdown/render/Syntax.scala（feat/tui a2bf878b34）。
//!
//! 它是高亮器，不是解析器：只需在回答里出现的代码上对，且一个字符都不丢（一行的记号拼回去就是这一行）。
//! 块注释与可以跨行的字符串带到下一行。
//!
//! 按码点（`char`）走；Scala 按 UTF-16 单元走，两者只在字符字面量 `'X'` 的 X 是 BMP 之外的码点时不同
//! （Scala 看到的是代理项），那里按 Scala 的结果判。记号是原行的切片。
use super::inline_parser::java_char::{is_digit, is_letter_or_digit, is_upper, is_whitespace};

/// 记号的类别；渲染把它映射到主题的颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Plain,
    Keyword,
    Str,
    Num,
    Comment,
    /// 紧挨着 `(` 的名字（不是关键字）。
    Func,
    /// 大写开头的名字，只在 [`Lang::types`] 为真的语言里。
    Type,
    /// 字面量的词：`true`、`null`、`None`、`nil`……
    Const,
    /// JSON 的键。
    Key,
}

/// 一个记号：原行的一段切片与它的类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tok<'a> {
    pub text: &'a str,
    pub kind: Kind,
}

/// 一种语言怎么切记号。
struct Lang {
    keywords: &'static [&'static str],
    line_comments: &'static [&'static str],
    /// 行注释只在词首开始（shell 的 `#`）。
    comment_at_word: bool,
    block: Option<(&'static str, &'static str)>,
    /// 可以跨行的字符串定界符，长的在前。
    multi: &'static [&'static str],
    /// 跨行字符串里反斜杠是否转义。
    multi_escapes: bool,
    /// 在本行结束的字符串的引号。
    quotes: &'static [char],
    /// `'` 是字符字面量：一个字符或一个转义。
    chars: bool,
    /// 后面跟着 `:` 的字符串是键。
    keys: bool,
    /// 语言的字面量词。
    constants: &'static [&'static str],
    /// 大写开头的名字是类型。
    types: bool,
}

const BASE: Lang = Lang {
    keywords: &[],
    line_comments: &["//"],
    comment_at_word: false,
    block: Some(("/*", "*/")),
    multi: &[],
    multi_escapes: true,
    quotes: &['"', '\''],
    chars: false,
    keys: false,
    constants: &[],
    types: false,
};

const TS: Lang = Lang {
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "declare",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "keyof",
        "let",
        "new",
        "of",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "satisfies",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "try",
        "type",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    multi: &["`"],
    constants: &["true", "false", "null", "undefined"],
    types: true,
    ..BASE
};

const SCALA: Lang = Lang {
    keywords: &[
        "abstract",
        "case",
        "catch",
        "class",
        "def",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "final",
        "finally",
        "for",
        "given",
        "if",
        "implicit",
        "import",
        "lazy",
        "match",
        "new",
        "object",
        "override",
        "package",
        "private",
        "protected",
        "return",
        "sealed",
        "super",
        "then",
        "this",
        "throw",
        "trait",
        "try",
        "type",
        "using",
        "val",
        "var",
        "while",
        "with",
        "yield",
    ],
    multi: &["\"\"\""],
    quotes: &['"'],
    chars: true,
    constants: &["true", "false", "null"],
    types: true,
    ..BASE
};

const PYTHON: Lang = Lang {
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def",
        "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in",
        "is", "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return", "try",
        "while", "with", "yield",
    ],
    line_comments: &["#"],
    block: None,
    multi: &["\"\"\"", "'''"],
    constants: &["True", "False", "None"],
    ..BASE
};

const RUST: Lang = Lang {
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "type",
        "unsafe", "use", "where", "while",
    ],
    quotes: &['"'],
    chars: true,
    constants: &["true", "false"],
    types: true,
    ..BASE
};

const GO: Lang = Lang {
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "type",
        "var",
    ],
    multi: &["`"],
    multi_escapes: false,
    quotes: &['"'],
    chars: true,
    constants: &["true", "false", "nil"],
    ..BASE
};

const SHELL: Lang = Lang {
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if",
        "in", "local", "return", "select", "then", "time", "until", "while",
    ],
    line_comments: &["#"],
    comment_at_word: true,
    block: None,
    ..BASE
};

const JSON: Lang = Lang {
    quotes: &['"'],
    keys: true,
    constants: &["true", "false", "null"],
    ..BASE
};

fn lang_of(name: &str) -> Option<&'static Lang> {
    Some(match name.to_lowercase().as_str() {
        "ts" | "typescript" | "tsx" | "js" | "javascript" | "jsx" | "mjs" | "cjs" => &TS,
        "scala" | "sc" => &SCALA,
        "python" | "py" => &PYTHON,
        "rust" | "rs" => &RUST,
        "go" => &GO,
        "sh" | "bash" | "zsh" | "shell" => &SHELL,
        "json" | "jsonc" => &JSON,
        _ => return None,
    })
}

/// `code` 的各行切成记号；不认得的语言为 `None`（不着色地显示）。`lang` 是信息串的第一个词，不分大小写。
pub fn lex<'a>(lang: &str, code: &'a str) -> Option<Vec<Vec<Tok<'a>>>> {
    let l = lang_of(lang)?;
    // 跨行的块注释或字符串：它的结束符与它的类别，带到下一行。
    let mut open: Option<(&'static str, Kind)> = None;
    Some(
        code.split('\n')
            .map(|line| {
                let (toks, after) = lex_line(l, line, open);
                open = after;
                toks
            })
            .collect(),
    )
}

fn is_word(c: char) -> bool {
    is_letter_or_digit(c) || c == '_'
}

/// 一行的词法状态：码点、码点的字节位置（多一个行尾）、产出的记号与待合并的普通文字的起点。
struct LineLexer<'a> {
    line: &'a str,
    cs: Vec<char>,
    pos: Vec<usize>,
    n: usize,
    out: Vec<Tok<'a>>,
    /// 还没产出的普通文字从这个码点下标开始（普通文字总是连续的）。
    plain_from: Option<usize>,
    open: Option<(&'static str, Kind)>,
}

impl<'a> LineLexer<'a> {
    fn text(&self, from: usize, until: usize) -> &'a str {
        &self.line[self.pos[from]..self.pos[until]]
    }

    fn flush_plain(&mut self, until: usize) {
        if let Some(from) = self.plain_from.take()
            && from < until
        {
            self.out.push(Tok {
                text: self.text(from, until),
                kind: Kind::Plain,
            });
        }
    }

    /// 产出码点 [from, until) 为 `kind`；普通文字先攒着，与相邻的普通文字合并。
    fn emit(&mut self, from: usize, until: usize, kind: Kind) {
        if kind == Kind::Plain {
            if self.plain_from.is_none() {
                self.plain_from = Some(from);
            }
        } else {
            self.flush_plain(from);
            self.out.push(Tok {
                text: self.text(from, until),
                kind,
            });
        }
    }

    fn starts_with_at(&self, at: usize, pat: &str) -> bool {
        pat.chars()
            .enumerate()
            .all(|(d, p)| self.cs.get(at + d) == Some(&p))
    }

    /// 从 `from` 找 `close`，返回它之后的下标；本行没有时为 `None`。
    fn end_of(&self, close: &str, from: usize, escapes: bool) -> Option<usize> {
        let mut j = from;
        while j < self.n {
            if escapes && self.cs[j] == '\\' {
                j += 2;
            } else if self.starts_with_at(j, close) {
                return Some(j + close.chars().count());
            } else {
                j += 1;
            }
        }
        None
    }

    /// 从 `start` 产出到 `close` 的末尾；本行先结束时产出行的其余部分，这个构造开着带到下一行。
    fn spanning(
        &mut self,
        start: usize,
        body_at: usize,
        close: &'static str,
        kind: Kind,
        escapes: bool,
    ) -> usize {
        match self.end_of(close, body_at, escapes) {
            Some(end) => {
                self.emit(start, end, kind);
                end
            }
            None => {
                self.emit(start, self.n, kind);
                self.open = Some((close, kind));
                self.n
            }
        }
    }
}

fn lex_line<'a>(
    l: &Lang,
    line: &'a str,
    carried: Option<(&'static str, Kind)>,
) -> (Vec<Tok<'a>>, Option<(&'static str, Kind)>) {
    let mut pos: Vec<usize> = line.char_indices().map(|(k, _)| k).collect();
    pos.push(line.len());
    let cs: Vec<char> = line.chars().collect();
    let n = cs.len();
    let mut x = LineLexer {
        line,
        cs,
        pos,
        n,
        out: Vec::new(),
        plain_from: None,
        open: None,
    };
    let mut i = match carried {
        Some((close, kind)) => x.spanning(0, 0, close, kind, kind == Kind::Str && l.multi_escapes),
        None => 0,
    };
    while i < n {
        let c = x.cs[i];
        let line_comment = l.line_comments.iter().any(|lc| {
            x.starts_with_at(i, lc) && (!l.comment_at_word || i == 0 || is_whitespace(x.cs[i - 1]))
        });
        let multi = l.multi.iter().copied().find(|d| x.starts_with_at(i, d));
        if line_comment {
            x.emit(i, n, Kind::Comment);
            i = n;
        } else if let Some((o, close)) = l.block.filter(|(o, _)| x.starts_with_at(i, o)) {
            i = x.spanning(i, i + o.len(), close, Kind::Comment, false);
        } else if let Some(d) = multi {
            i = x.spanning(i, i + d.len(), d, Kind::Str, l.multi_escapes);
        } else if c == '\'' && l.chars {
            // 字符字面量：一个字符或一个转义；Rust 的生命周期 'a 不是字符串。
            // Scala 按 UTF-16 单元看 `charAt(i + 2)`：中间那个字符在 BMP 之外时那里是代理项，不是 '。
            let end = if i + 2 < n && x.cs[i + 1] == '\\' {
                x.end_of("'", i + 3, false)
            } else if i + 2 < n && x.cs[i + 2] == '\'' && x.cs[i + 1].len_utf16() == 1 {
                Some(i + 3)
            } else {
                None
            };
            match end {
                Some(end) => {
                    x.emit(i, end, Kind::Str);
                    i = end;
                }
                None => {
                    x.emit(i, i + 1, Kind::Plain);
                    i += 1;
                }
            }
        } else if l.quotes.contains(&c) {
            let mut buf = [0u8; 4];
            match x.end_of(c.encode_utf8(&mut buf), i + 1, true) {
                None => {
                    x.emit(i, i + 1, Kind::Plain);
                    i += 1;
                }
                Some(end) => {
                    let mut k = end;
                    while k < n && x.cs[k] == ' ' {
                        k += 1;
                    }
                    let kind = if l.keys && k < n && x.cs[k] == ':' {
                        Kind::Key
                    } else {
                        Kind::Str
                    };
                    x.emit(i, end, kind);
                    i = end;
                }
            }
        } else if is_digit(c) {
            let mut j = i + 1;
            while j < n && is_word(x.cs[j]) {
                j += 1;
            }
            if j + 1 < n && x.cs[j] == '.' && is_digit(x.cs[j + 1]) {
                j += 1;
                while j < n && is_word(x.cs[j]) {
                    j += 1;
                }
            }
            x.emit(i, j, Kind::Num);
            i = j;
        } else if is_word(c) {
            let mut j = i + 1;
            while j < n && is_word(x.cs[j]) {
                j += 1;
            }
            let w = x.text(i, j);
            let kind = if l.constants.contains(&w) {
                Kind::Const
            } else if l.keywords.contains(&w) {
                Kind::Keyword
            } else if j < n && x.cs[j] == '(' {
                Kind::Func
            } else if l.types && is_upper(c) {
                Kind::Type
            } else {
                Kind::Plain
            };
            x.emit(i, j, kind);
            i = j;
        } else {
            x.emit(i, i + 1, Kind::Plain);
            i += 1;
        }
    }
    x.flush_plain(n);
    (x.out, x.open)
}
