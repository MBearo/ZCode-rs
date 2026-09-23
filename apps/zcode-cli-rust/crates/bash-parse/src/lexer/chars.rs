//! unbash `chars.js` 常量与 lexer 的字符分类表。

/// JS `charCodeAt` 越界返回 NaN：与任何字符比较都为假，也不属于任何字符表。
pub(crate) const NAN: u32 = 256;

pub(crate) const TAB: u32 = 0x09;
pub(crate) const NL: u32 = 0x0a;
pub(crate) const SPACE: u32 = 0x20;
pub(crate) const BANG: u32 = 0x21;
pub(crate) const DQUOTE: u32 = 0x22;
pub(crate) const HASH: u32 = 0x23;
pub(crate) const DOLLAR: u32 = 0x24;
pub(crate) const AMP: u32 = 0x26;
pub(crate) const SQUOTE: u32 = 0x27;
pub(crate) const LPAREN: u32 = 0x28;
pub(crate) const RPAREN: u32 = 0x29;
pub(crate) const STAR: u32 = 0x2a;
pub(crate) const PLUS: u32 = 0x2b;
pub(crate) const COMMA: u32 = 0x2c;
pub(crate) const DASH: u32 = 0x2d;
pub(crate) const DOT: u32 = 0x2e;
pub(crate) const SEMI: u32 = 0x3b;
pub(crate) const LT: u32 = 0x3c;
pub(crate) const EQ: u32 = 0x3d;
pub(crate) const GT: u32 = 0x3e;
pub(crate) const QUESTION: u32 = 0x3f;
pub(crate) const AT: u32 = 0x40;
pub(crate) const LBRACKET: u32 = 0x5b;
pub(crate) const BACKSLASH: u32 = 0x5c;
pub(crate) const UNDERSCORE: u32 = 0x5f;
pub(crate) const BACKTICK: u32 = 0x60;
pub(crate) const LBRACE: u32 = 0x7b;
pub(crate) const PIPE: u32 = 0x7c;
pub(crate) const RBRACE: u32 = 0x7d;

/// `charType` 表：bit0 为元字符，bit1 为单词内特殊字符；非 ASCII（含 NaN）为 0。
pub(crate) fn char_type(c: u32) -> u8 {
    match c {
        0x7c | 0x26 | 0x3b | 0x28 | 0x29 | 0x3c | 0x3e | 0x20 | 0x09 | 0x0a => 1,
        0x5c | 0x27 | 0x22 | 0x24 | 0x60 | 0x7b => 2,
        _ => 0,
    }
}

pub(crate) fn is_meta(c: u32) -> bool {
    char_type(c) & 1 != 0
}

/// JS `c < 128 && charType[c]`。
pub(crate) fn is_special(c: u32) -> bool {
    char_type(c) != 0
}

pub(crate) fn is_id_start(c: u32) -> bool {
    (c < 128 && (c as u8).is_ascii_alphabetic()) || c == UNDERSCORE
}

pub(crate) fn is_id_continue(c: u32) -> bool {
    is_id_start(c) || is_digit(c)
}

pub(crate) fn is_digit(c: u32) -> bool {
    c < 128 && (c as u8).is_ascii_digit()
}

pub(crate) fn is_extglob_prefix(c: u32) -> bool {
    matches!(c, QUESTION | AT | STAR | PLUS | BANG | EQ)
}
