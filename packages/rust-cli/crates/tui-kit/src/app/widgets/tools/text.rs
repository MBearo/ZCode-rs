//! 工具调用的文字工具：行、第一行、短路径、补丁的文件、grep 的计数，与 Scala 正则的手写替身。
//! 移植自 Puffin 的 shell/tools/text.rs（Tools.scala 第 520–548、614–728 行的 helpers）。
//!
//! 与 Scala 的不同：正则换成手写的前缀与查找（没有 regex 依赖）；位置是字节偏移；`trim` 按 Java（去掉 ≤ U+0020 的码点）；
//! `isLetterOrDigit` 用 `char::is_alphanumeric`。
use serde_json::Value;

pub use crate::app::widgets::fit::{cut_text, java_space};

use super::Where;

/// Java 的 `String.trim`：去掉两端 ≤ U+0020 的码点。
pub fn jtrim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= ' ')
}

/// Scala 的 `linesIterator`：按 `\n`、`\r\n`、`\r` 分行，末尾的换行不产生空行。
pub fn lines_iter(s: &str) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
            i += 1;
        }
        let end = i;
        if i < b.len() {
            let cr = b[i] == b'\r';
            i += 1;
            if cr && i < b.len() && b[i] == b'\n' {
                i += 1;
            }
        }
        out.push(&s[start..end]);
    }
    out
}

/// 字符串值；不是字符串或没有时为 ""。
pub fn str_of<'a>(o: &'a Value, key: &str) -> &'a str {
    o.get(key).and_then(Value::as_str).unwrap_or("")
}

/// 数值；不是数时为 `None`。
pub fn num_of(o: &Value, key: &str) -> Option<f64> {
    o.get(key).and_then(Value::as_f64)
}

/// 网格用的纯文字：制表符与回车会弄坏格子。
pub fn clean(s: &str) -> String {
    s.replace('\r', "").replace('\t', "    ")
}

/// 去掉制表符与回车的行；空行除非 `keep_blank` 都去掉，末尾的空行总是去掉。
pub fn lines(text: &str, keep_blank: bool) -> Vec<String> {
    let mut all: Vec<String> = text.split('\n').map(clean).collect();
    if !keep_blank {
        all.retain(|l| !jtrim(l).is_empty());
    }
    while all.last().is_some_and(|l| jtrim(l).is_empty()) {
        all.pop();
    }
    all
}

/// 第一个非空行；后面还有时加 ` …`。
pub fn first_line(text: &str) -> String {
    let ls = lines(text, false);
    match ls.first() {
        None => String::new(),
        Some(l) if ls.len() > 1 => format!("{} …", jtrim(l)),
        Some(l) => jtrim(l).to_owned(),
    }
}

pub fn plural(n: i64, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

pub fn matches(n: i64) -> String {
    if n == 1 {
        "1 match".to_owned()
    } else {
        format!("{n} matches")
    }
}

/// Scala 的 `fmt`：整数写成整数，其余照 JS 的写法。
pub fn fmt(d: f64) -> String {
    if d.is_finite() && d.fract() == 0.0 {
        format!("{}", d as i64)
    } else {
        format!("{d}")
    }
}

/// 开头的十进制数字（至少一位）与它们之后的字节偏移。
pub fn digits(s: &str) -> Option<(&str, usize)> {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    (n > 0).then(|| (&s[..n], n))
}

/// `"{key}: (\d+)"` 第一次出现时的数字。
pub fn find_figure(text: &str, key: &str) -> Option<f64> {
    let pat = format!("{key}: ");
    text.match_indices(&pat)
        .find_map(|(i, _)| digits(&text[i + pat.len()..]))
        .and_then(|(d, _)| d.parse::<f64>().ok())
}

/// `^Exit code (-?\d+)`：输出开头的退出码。
pub fn exit_line(text: &str) -> Option<i64> {
    let rest = text.strip_prefix("Exit code ")?;
    let (neg, body) = match rest.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, rest),
    };
    let (d, _) = digits(body)?;
    let v: i64 = d.parse().ok()?;
    Some(if neg { -v } else { v })
}

/// 文件夹里的路径相对于它，家目录下写成 `~/…`，其余照原样。
pub fn short(path: &str, where_: &Where) -> String {
    let under = |root: &str| {
        !root.is_empty()
            && path.len() > root.len()
            && path.starts_with(root)
            && matches!(path.as_bytes()[root.len()], b'/' | b'\\')
    };
    if !where_.cwd.is_empty() && path == where_.cwd {
        ".".to_owned()
    } else if under(&where_.cwd) {
        path[where_.cwd.len() + 1..].to_owned()
    } else {
        match where_.home.as_deref().filter(|h| under(h)) {
            Some(h) => format!("~{}", &path[h.len()..]),
            None => path.to_owned(),
        }
    }
}

/// 去掉开头的 `cd <文件夹> &&` 的命令。
pub fn uncd(command: &str, cwd: &str) -> String {
    if cwd.is_empty() {
        return command.to_owned();
    }
    let forms = [
        format!("cd {cwd} && "),
        format!("cd \"{cwd}\" && "),
        format!("cd '{cwd}' && "),
    ];
    forms
        .iter()
        .find_map(|f| command.strip_prefix(f.as_str()))
        .unwrap_or(command)
        .to_owned()
}

fn path_char(c: char) -> bool {
    c.is_alphanumeric() || "._-~/\\@+".contains(c)
}

fn sep(c: char) -> bool {
    c == '/' || c == '\\'
}

/// `text` 里会话的文件夹写成相对的：`cwd/x` 写成 `x`，文件夹本身写成 `.`（`cwd/` 写成 `./`）。只在它作为一整个路径时：
/// 不紧跟在路径字符之后（`/x/a/b`），也不紧接路径字符（`/a/bc`）。根目录不动：每个路径都会匹配它。
pub fn relative(text: &str, cwd: &str) -> String {
    if cwd.len() < 2 || !text.contains(cwd) {
        return text.to_owned();
    }
    let at_char = |k: usize| text[k..].chars().next();
    let mut out = String::new();
    let mut i = 0;
    while i < text.len() {
        let Some(off) = text[i..].find(cwd) else {
            out.push_str(&text[i..]);
            break;
        };
        let at = i + off;
        let end = at + cwd.len();
        let free = at == 0 || !text[..at].chars().next_back().is_some_and(path_char);
        out.push_str(&text[i..at]);
        let named = |k: usize| at_char(k).is_some_and(|c| path_char(c) && !sep(c));
        let after = at_char(end);
        if free && after.is_some_and(sep) && named(end + 1) {
            i = end + 1;
        } else if free && after.is_none_or(|c| !path_char(c) || sep(c)) {
            out.push('.');
            i = end;
        } else {
            out.push_str(cwd);
            i = end;
        }
    }
    out
}

/// 结束了的命令的输出行：没有 `Exit code N` 行、没有 persisted-output 的包装、开头没有空行。
pub fn shell_lines(out: &str) -> Vec<String> {
    let mut ls: Vec<String> = lines(out, true)
        .into_iter()
        .filter(|l| !l.starts_with("<persisted-output>") && !l.starts_with("</persisted-output>"))
        .skip_while(|l| jtrim(l).is_empty())
        .collect();
    if ls.first().is_some_and(|l| exit_line(l).is_some()) {
        ls.remove(0);
    }
    ls.retain(|l| !jtrim(l).is_empty());
    ls
}

pub fn line_count(text: &str) -> i64 {
    lines(text, true).len() as i64
}

/// 补丁的文件，按次序，与各自加减的行数。
pub fn patch_files(patch: &str, where_: &Where) -> Vec<(String, i64, i64)> {
    let mut out = Vec::new();
    let mut cur: Option<(String, i64, i64)> = None;
    for l in lines_iter(patch) {
        let file = ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
            .iter()
            .find_map(|p| l.strip_prefix(p))
            .filter(|rest| !rest.is_empty());
        match file {
            Some(name) => {
                out.extend(cur.take());
                cur = Some((short(jtrim(name), where_), 0, 0));
            }
            None => {
                if let Some(c) = cur.as_mut() {
                    if l.starts_with('+') {
                        c.1 += 1;
                    } else if l.starts_with('-') {
                        c.2 += 1;
                    }
                }
            }
        }
    }
    out.extend(cur);
    out
}

/// grep 的结果说了多少。
pub fn grep_count(out: &str, mode: &str) -> Option<String> {
    let text = jtrim(out);
    if text == "No files found" || text == "No matches found" {
        return Some("no matches".to_owned());
    }
    if let Some((occ, files)) = found_total(text) {
        return Some(format!("{} in {}", matches(occ), plural(files, "file")));
    }
    if mode == "content" {
        let hits: Vec<&str> = lines_iter(text).into_iter().filter_map(hit_file).collect();
        if hits.is_empty() {
            return None;
        }
        let mut distinct: Vec<&str> = Vec::new();
        for h in &hits {
            if !distinct.contains(h) {
                distinct.push(h);
            }
        }
        return Some(format!(
            "{} in {}",
            matches(hits.len() as i64),
            plural(distinct.len() as i64, "file")
        ));
    }
    let rest = text.strip_prefix("Found ")?;
    let (d, n) = digits(rest)?;
    rest[n..]
        .strip_prefix(" file")
        .and_then(|_| d.parse::<i64>().ok())
        .map(|k| plural(k, "file"))
}

/// `Found (\d+) total occurrences? across (\d+) files?`，在文字的任何位置。
fn found_total(text: &str) -> Option<(i64, i64)> {
    text.match_indices("Found ").find_map(|(i, _)| {
        let rest = &text[i + 6..];
        let (a, n) = digits(rest)?;
        let rest = rest[n..].strip_prefix(" total occurrence")?;
        let rest = rest.strip_prefix('s').unwrap_or(rest);
        let rest = rest.strip_prefix(" across ")?;
        let (b, m) = digits(rest)?;
        rest[m..].strip_prefix(" file")?;
        Some((a.parse().ok()?, b.parse().ok()?))
    })
}

/// `^(.+?):\d+:`：一行 grep 结果的文件。
fn hit_file(line: &str) -> Option<&str> {
    line.match_indices(':').find_map(|(k, _)| {
        if k == 0 {
            return None;
        }
        let (_, n) = digits(&line[k + 1..])?;
        line[k + 1 + n..].starts_with(':').then(|| &line[..k])
    })
}

/// 输入的第一个字符串值：MCP 调用的第一个参数。
pub fn first_string(i: &Value) -> Option<String> {
    i.as_object()?
        .keys()
        .map(|k| str_of(i, k))
        .find(|s| !jtrim(s).is_empty())
        .map(str::to_owned)
}
