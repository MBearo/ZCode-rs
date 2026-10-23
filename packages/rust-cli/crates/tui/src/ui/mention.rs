//! 输入框里的 `@` 词（docs/specs/rust-cli/tui-input.md §1.2）：取出补全的词与替换的写法。
//! 纯函数，下标是字节。

/// 光标所在的 `@` 词。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// `@` 的位置。
    pub start: usize,
    /// 词的结尾（含光标后面的路径字符或引号部分）。
    pub end: usize,
    /// 去掉 `@` 与引号后的查询。
    pub query: String,
    /// `@"…`：替换时保留引号。
    pub quoted: bool,
}

/// 路径字符：字母、数字与 `_-./\()[]~:`。
fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || "_-./\\()[]~:".contains(c)
}

pub fn at_token(text: &str, cursor: usize) -> Option<Token> {
    let cursor = cursor.min(text.len());
    let before = &text[..cursor];
    let after = &text[cursor..];
    // 带引号：`@"([^"]*)"?$`，后面接着取到下一个引号（含）。
    if let Some(start) = before.rfind("@\"") {
        let inside = &before[start + 2..];
        let closed = inside.strip_suffix('"').unwrap_or(inside);
        if !closed.contains('"') {
            let tail = match after.find('"') {
                Some(quote) if !inside.ends_with('"') => quote + 1,
                Some(_) => 0,
                None => after.len(),
            };
            let end = cursor + tail;
            let query = text[start + 2..end].trim_end_matches('"').to_owned();
            return Some(Token {
                start,
                end,
                query,
                quoted: true,
            });
        }
    }
    let start = before.rfind('@')?;
    let preceded = before[..start]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace);
    if !preceded || !before[start + 1..].chars().all(is_path_char) {
        return None;
    }
    let tail: usize = after
        .chars()
        .take_while(|c| is_path_char(*c))
        .map(char::len_utf8)
        .sum();
    let end = cursor + tail;
    Some(Token {
        start,
        end,
        query: text[start + 1..end].to_owned(),
        quoted: false,
    })
}

/// 替换词的文字：完整的候选加空格；含空格或原来带引号时用 `@"…"`。
pub fn replacement(path: &str, quoted: bool, complete: bool) -> String {
    let space = if complete { " " } else { "" };
    if quoted || (complete && path.contains(' ')) {
        format!("@\"{path}\"{space}")
    } else {
        format!("@{path}{space}")
    }
}

/// 候选的最长公共前缀。
pub fn common_prefix(items: &[String]) -> String {
    let Some(first) = items.first() else {
        return String::new();
    };
    let mut prefix = first.as_str();
    for item in &items[1..] {
        let shared: usize = prefix
            .chars()
            .zip(item.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum();
        prefix = &prefix[..shared];
    }
    prefix.to_owned()
}

#[cfg(test)]
#[path = "mention_tests.rs"]
mod tests;
