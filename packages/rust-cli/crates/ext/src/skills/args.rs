//! skill 正文的参数替换（docs/specs/rust-cli/ext.md §2.5）。

/// 替换期间代替 `` $ `` 的占位，最后换回 `` $ ``。
const DOLLAR: char = '\u{FFFF}';
/// 包住替换进去的值，替换进去的文本不再参与后面的替换，最后去掉。
const GUARD: char = '\u{FFFE}';

/// 参数按 shell 词法切分（引号、反斜杠转义）；切不出时按空白切。
pub(crate) fn split_args(args: &str) -> Vec<String> {
    if args.trim().is_empty() {
        return Vec::new();
    }
    let words = shell_words(args);
    if words.is_empty() {
        args.split_whitespace().map(str::to_owned).collect()
    } else {
        words
    }
}

fn shell_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                started = true;
                for inner in chars.by_ref() {
                    if inner == '\'' {
                        break;
                    }
                    current.push(inner);
                }
            }
            '"' => {
                started = true;
                while let Some(inner) = chars.next() {
                    match inner {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some(escaped @ ('"' | '\\' | '$' | '`')) => current.push(escaped),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => current.push('\\'),
                        },
                        other => current.push(other),
                    }
                }
            }
            '\\' => {
                started = true;
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            other => {
                started = true;
                current.push(other);
            }
        }
    }
    if started {
        words.push(current);
    }
    words
}

/// 替换进去的值里的 `!` 先转义，参数不能注入 `!` 命令。
fn escape_bang(value: &str) -> String {
    let value = value.replace("`!", "` !").replace("!`", "! `");
    let mut out = String::with_capacity(value.len());
    let mut previous: Option<char> = None;
    for c in value.chars() {
        if c == '!' && previous.is_none_or(char::is_whitespace) {
            out.push('\\');
        }
        out.push(c);
        previous = Some(c);
    }
    out
}

fn guarded(value: &str) -> String {
    let value = value.replace([DOLLAR, GUARD], "\u{FFFD}");
    format!(
        "{GUARD}{}{GUARD}",
        escape_bang(&value).replace('$', &DOLLAR.to_string())
    )
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// 在 `text` 里找 `$<name>`（后面不接 `[` 或单词字符），换成 `replacement`；返回是否换过。
fn replace_named(text: &mut String, name: &str, replacement: &str) -> bool {
    let needle = format!("${name}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    let mut replaced = false;
    while let Some(at) = rest.find(&needle) {
        let after = &rest[at + needle.len()..];
        let next = after.chars().next();
        out.push_str(&rest[..at]);
        if next.is_some_and(|c| c == '[' || is_word(c)) {
            out.push_str(&needle);
        } else {
            out.push_str(replacement);
            replaced = true;
        }
        rest = after;
    }
    out.push_str(rest);
    *text = out;
    replaced
}

/// `\$` 后面接数字、`ARGUMENTS` 或命名参数时是转义：换成占位，不参与替换。
fn protect_escapes(text: &str, names: &[&str]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        let escaped = chars[index] == '\\'
            && chars.get(index + 1) == Some(&'$')
            && (index == 0 || chars[index - 1] != '\\');
        if escaped {
            let rest: String = chars[index + 2..].iter().collect();
            let protects = rest.chars().next().is_some_and(|c| c.is_ascii_digit())
                || rest.starts_with("ARGUMENTS")
                || names.iter().any(|name| {
                    rest.starts_with(name)
                        && !rest[name.len()..]
                            .chars()
                            .next()
                            .is_some_and(|c| c == '[' || is_word(c))
                });
            if protects {
                out.push(DOLLAR);
                index += 2;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// `$ARGUMENTS[n]` 与 `` $n ``：下标在参数里时替换，否则原样（`$ARGUMENTS[n]` 的 `` $ `` 记成占位）。
fn replace_indexed(text: &str, words: &[String], replaced: &mut bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(inner) = after.strip_prefix("ARGUMENTS[") {
            let digits: String = inner.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() && inner[digits.len()..].starts_with(']') {
                let consumed = "ARGUMENTS[".len() + digits.len() + 1;
                match digits.parse::<usize>().ok().and_then(|n| words.get(n)) {
                    Some(word) => {
                        out.push_str(&guarded(word));
                        *replaced = true;
                    }
                    None => {
                        out.push(DOLLAR);
                        out.push_str(&after[..consumed]);
                    }
                }
                rest = &after[consumed..];
                continue;
            }
        }
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let followed = after[digits.len()..].chars().next().is_some_and(is_word);
        if !digits.is_empty()
            && !followed
            && let Some(word) = digits.parse::<usize>().ok().and_then(|n| words.get(n))
        {
            out.push_str(&guarded(word));
            *replaced = true;
            rest = &after[digits.len()..];
            continue;
        }
        out.push('$');
        rest = after;
    }
    out.push_str(rest);
    out
}

/// 参数替换（`$ARGUMENTS`、下标、命名参数，值里的 `!` 转义）。
pub(crate) fn substitute(content: &str, args: &str, arg_names: &[String]) -> String {
    let mut text = content.replace([DOLLAR, GUARD], "\u{FFFD}");
    let words = split_args(args);
    let mut named: Vec<(usize, &str)> = arg_names
        .iter()
        .enumerate()
        .filter(|(_, name)| !name.is_empty())
        .map(|(index, name)| (index, name.as_str()))
        .collect();
    named.sort_by_key(|(_, name)| std::cmp::Reverse(name.len()));
    let names: Vec<&str> = named.iter().map(|(_, name)| *name).collect();
    text = protect_escapes(&text, &names);
    let mut replaced = false;
    for (index, name) in &named {
        let value = guarded(words.get(*index).map_or("", String::as_str));
        replaced |= replace_named(&mut text, name, &value);
    }
    text = replace_indexed(&text, &words, &mut replaced);
    if text.contains("$ARGUMENTS") {
        text = text.replace("$ARGUMENTS", &guarded(args));
        replaced = true;
    }
    if !replaced && !args.is_empty() {
        text.push_str("\n\nARGUMENTS: ");
        text.push_str(&guarded(args));
    }
    text.replace(DOLLAR, "$").replace(GUARD, "")
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;
