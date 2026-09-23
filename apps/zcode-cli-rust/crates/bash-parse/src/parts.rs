//! 单词的结构化部件（unbash `WordPart`）以及 `word.value` / 动态判定。
//!
//! unbash 的 `Word.parts` 是惰性的：首次访问时在 `[pos, end)` 上新建一个有界 lexer 重新扫描。
//! 这里只保留影响权限分析的信息：可计算 value 的字面部件，和一律视为动态的展开部件。

use crate::lexer::Lexer;

#[derive(Clone, Debug)]
pub(crate) enum Part {
    Literal(Vec<u8>),
    SingleQuoted(Vec<u8>),
    AnsiC(Vec<u8>),
    /// DoubleQuoted / LocaleString：`text` 为去引号后的内容，`dynamic` 表示含展开子节点。
    Quoted {
        text: Vec<u8>,
        dynamic: bool,
    },
    /// 其余部件（简单/参数/算术/命令/进程替换、花括号展开、extglob）一律视为动态，
    /// 携带原始文本仅用于拼出 value。
    Dynamic(Vec<u8>),
}

impl Part {
    fn is_dynamic(&self) -> bool {
        match self {
            Part::Literal(_) | Part::SingleQuoted(_) | Part::AnsiC(_) => false,
            Part::Quoted { dynamic, .. } => *dynamic,
            Part::Dynamic(_) => true,
        }
    }

    fn value(&self) -> &[u8] {
        match self {
            Part::Literal(value)
            | Part::SingleQuoted(value)
            | Part::AnsiC(value)
            | Part::Dynamic(value) => value,
            Part::Quoted { text, .. } => text,
        }
    }
}

/// 对 `[pos, end)` 重新扫描得到的结构信息；只取决于范围，与单词的 `text` 无关。
#[derive(Clone)]
pub(crate) struct Structure {
    /// 有结构化部件时由部件拼出的 value；`None` 表示 value 取单词自身的 `text`。
    pub value: Option<Vec<u8>>,
    pub dynamic: bool,
    /// 重新扫描时 unbash 会死循环或栈溢出：发生在 `analyzeBashCommand` 的 try 之外，Node 拿不到结果。
    pub hung: bool,
}

fn summarize(parts: Option<Vec<Part>>, hung: bool) -> Structure {
    let dynamic = parts
        .as_ref()
        .is_some_and(|parts| parts.iter().any(Part::is_dynamic));
    let value = parts.map(|parts| {
        parts
            .iter()
            .flat_map(|part| part.value().iter().copied())
            .collect()
    });
    Structure {
        value,
        dynamic,
        hung,
    }
}

/// `WordImpl(text, pos, end, source)` 的 `parts`（用于 value 与 `wordHasDynamicParts`）。
pub(crate) fn word_structure(source: &[u8], pos: usize, end: usize) -> Structure {
    let mut lexer = Lexer::bounded(source, pos, end, true);
    let parts = lexer.build_word_parts(pos);
    summarize(parts, lexer.hung || lexer.overflow)
}

/// heredoc 正文单词（`computeHereDocBodyParts`）的结构。
pub(crate) fn heredoc_structure(source: &[u8], pos: usize, end: usize) -> Structure {
    let mut lexer = Lexer::bounded(source, pos, end, true);
    let parts = lexer.build_heredoc_parts(pos, end);
    summarize(parts, lexer.hung || lexer.overflow)
}
