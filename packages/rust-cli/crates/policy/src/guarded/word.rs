// 移植自 apps/zcode-cli/packages/core/src/tool/handlers/guarded/word.ts（修复线 F20）。
//! 一个词的字面事实（Node `CommandWord` 去掉位置）：不做展开，未知的词不当作已知选项。

use crate::consts::{ESCAPE_LEN, SIGIL_PAIR_LEN};
use crate::parse::WordText;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Word {
    pub value: String,
    pub dynamic: bool,
    /// 有不加引号的 `*`、`?`、`[`（Node `hasUnquotedGlob`）。
    pub glob: bool,
}

impl Word {
    /// Node `collectSimpleCommand` 的词：`literalPosixWord(word.text)`；有结构化部件时 glob 只看顶层的字面片段。
    pub(crate) fn from_text(word: &WordText) -> Self {
        let mut out = literal(&word.text);
        if word.structured {
            out.glob = top_level_glob(&word.text);
        }
        out
    }
}

/// Node `literalPosixWord`：去引号与反斜杠；`$` / 反引号（单引号外）、不加引号的 `*?[{}~`、没闭合的引号都记为动态。
pub(crate) fn literal(text: &str) -> Word {
    let chars: Vec<char> = text.chars().collect();
    let mut value = String::new();
    let mut quote: Option<char> = None;
    let mut dynamic = false;
    let mut glob = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if Some(c) == quote {
            quote = None;
            i += 1;
            continue;
        }
        if quote != Some('\'') && c == '\\' {
            let Some(&next) = chars.get(i + 1) else {
                dynamic = true;
                break;
            };
            if quote.is_none() || matches!(next, '$' | '`' | '"' | '\\' | '\n') {
                if next != '\n' {
                    value.push(next);
                }
                i += ESCAPE_LEN;
                continue;
            }
        }
        if quote.is_none() && (c == '\'' || c == '"') {
            quote = Some(c);
            i += 1;
            continue;
        }
        if quote != Some('\'') && (c == '$' || c == '`') {
            dynamic = true;
        }
        if quote.is_none() && "*?[{}~".contains(c) {
            dynamic = true;
        }
        if quote.is_none() && "*?[".contains(c) {
            glob = true;
        }
        value.push(c);
        i += 1;
    }
    Word {
        value,
        dynamic: dynamic || quote.is_some(),
        glob,
    }
}

/// 跳到配对的右括号（`open` 已在 `i` 之前）之后；没有配对时到结尾。
fn skip_balanced(chars: &[char], mut i: usize, open: char, close: char) -> usize {
    let mut depth = 1;
    while i < chars.len() && depth > 0 {
        match chars[i] {
            '\\' => i += 1,
            c if c == open => depth += 1,
            c if c == close => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    i
}

/// 跳过引号里的内容（`i` 在左引号之后）；双引号里认反斜杠。
fn skip_quoted(chars: &[char], mut i: usize, quote: char) -> usize {
    while i < chars.len() && chars[i] != quote {
        if quote != '\'' && chars[i] == '\\' {
            i += 1;
        }
        i += 1;
    }
    i + 1
}

/// `{…}` 是不是花括号展开（顶层有 `,` 或 `..`）；是的话返回右括号之后的位置。
fn brace_expansion(chars: &[char], start: usize) -> Option<usize> {
    let end = skip_balanced(chars, start + 1, '{', '}');
    if end > chars.len() || chars.get(end - 1) != Some(&'}') {
        return None;
    }
    let inner: String = chars[start + 1..end - 1].iter().collect();
    (inner.contains(',') || inner.contains("..")).then_some(end)
}

/// 有结构化部件的词：Node 只看顶层 `Literal` 部件里不加引号的 `*?[`（`$(…)`、`${…}`、花括号展开、extglob
/// 与引号里的都不算）。这里按 bash 的词法跳过这些部件；是 unbash 部件划分的近似（shell-permissions.md §4.6.1）。
fn top_level_glob(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\\' => i += ESCAPE_LEN,
            '\'' | '"' => i = skip_quoted(&chars, i + 1, c),
            '`' => i = skip_quoted(&chars, i + 1, '`'),
            '$' => match next {
                Some('(') => i = skip_balanced(&chars, i + SIGIL_PAIR_LEN, '(', ')'),
                Some('{') => i = skip_balanced(&chars, i + SIGIL_PAIR_LEN, '{', '}'),
                Some('\'') => i = skip_quoted(&chars, i + SIGIL_PAIR_LEN, '\''),
                Some('"') => i = skip_quoted(&chars, i + SIGIL_PAIR_LEN, '"'),
                // `$*`、`$?` 等特殊参数与 `$name`：展开，不是 glob。
                Some(n) if n.is_ascii_alphabetic() || n == '_' => {
                    i += SIGIL_PAIR_LEN;
                    while chars
                        .get(i)
                        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
                    {
                        i += 1;
                    }
                }
                Some(_) => i += SIGIL_PAIR_LEN,
                None => i += 1,
            },
            // extglob：`?(…)`、`*(…)`、`+(…)`、`@(…)`、`!(…)` 是 ExtendedGlob 部件。
            '?' | '*' | '+' | '@' | '!' if next == Some('(') => {
                i = skip_balanced(&chars, i + SIGIL_PAIR_LEN, '(', ')');
            }
            '*' | '?' | '[' => return true,
            '{' => match brace_expansion(&chars, i) {
                Some(end) => i = end,
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    false
}

/// Node `commandBasename`：动态的词没有名字；Windows（Git Bash）上不分大小写、两种分隔符、去掉 `.exe`。
/// 名字为空时也是 None（Node 按假值处理）。
pub(crate) fn basename(word: &Word, windows: bool) -> Option<String> {
    if word.dynamic {
        return None;
    }
    let name = if windows {
        word.value.rsplit(['\\', '/']).next()
    } else {
        word.value.rsplit('/').next()
    }
    .unwrap_or_default();
    let name = if windows {
        let lower = name.to_lowercase();
        lower
            .strip_suffix(".exe")
            .map_or(lower.clone(), str::to_owned)
    } else {
        name.to_owned()
    };
    (!name.is_empty()).then_some(name)
}
