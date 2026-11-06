//! 提及发出去的写法与读回来的段（docs/puffin-tui.md §4.2「提及（@）」「提及技能」）：
//! 光秃的提及、技能与会话的桌面端链接、发出的文字，与发出的回合里链接读回的 `@标签`、`$名字`。Puffin 的壳
//! （`shell/mentions.rs`、`shell/at/wire.rs`）与旧界面（`ui/skill_mention.rs`、`ui/at_view.rs`、栈与用户行）调同一份，两边
//! 发出与显示的字相同。移植自 Puffin 的壳（At.scala 第 659–782 行的 `Link`、`Scheme`、`parts`）。纯的；位置是字节偏移，
//! `\s` 按 JS 的空白（[`js_space`]），正则换成手写的扫描。

/// JS 正则的 `\s` 在这里用到的部分（Puffin 一直用的这一组：不含 U+3000 与 U+FEFF）。
pub fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}'
    )
}

/// 一个光秃的提及：符号 `sign` 在开头或跟在空白之后，名字里没有空白与 `others`，在空白或末尾结束（桌面端自己对光秃提及的
/// 规则，Scala 的 `(^|\s)\$([^\s$¥￥]+)(?=\s|$)`）：符号的位置、末尾、名字。
pub fn bares<'a>(text: &'a str, sign: char, others: &[char]) -> Vec<(usize, usize, &'a str)> {
    let mut out = Vec::new();
    let mut prev: Option<char> = None;
    let mut skip_to = 0;
    for (i, c) in text.char_indices() {
        let at_start = prev.is_none_or(js_space);
        prev = Some(c);
        if i < skip_to || c != sign || !at_start {
            continue;
        }
        let from = i + c.len_utf8();
        let rest = &text[from..];
        let len = rest
            .find(|x: char| js_space(x) || x == sign || others.contains(&x))
            .unwrap_or(rest.len());
        let end = from + len;
        let followed = text[end..].chars().next().is_none_or(js_space);
        if len > 0 && followed {
            out.push((i, end, &text[from..end]));
            skip_to = end;
        }
    }
    out
}

/// 技能的桌面端链接 `[$名字](路径)`：标签里转义 `\` `[` `]`，路径里转义 `\` `>`。
pub fn link(name: &str, path: &str) -> String {
    let label = format!("${name}")
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]");
    let dest = path.replace('\\', "\\\\").replace('>', "\\>");
    format!("[{label}]({dest})")
}

/// 发出的文字：`path_of` 给出 SKILL.md 路径的技能，每个光秃的 `$名字`（`signs` 之外）成为桌面端的链接，模型拿到路径，桌面端
/// 画它的记号；其余的 `$词` 照打的发出。路径为空的技能（内核的命令都没有路径）也照打的发出：`[$名字]()` 读不回来
/// （链接的路径至少一个字），模型看到 `$名字` 自己调用 `Skill`。
pub fn to_wire<'p>(
    text: &str,
    signs: &[char],
    path_of: &dyn Fn(&str) -> Option<&'p str>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (s, e, name) in bares(text, '$', signs) {
        if let Some(path) = path_of(name).filter(|p| !p.is_empty()) {
            out.push_str(&text[at..s]);
            out.push_str(&link(name, path));
            at = e;
        }
    }
    out.push_str(&text[at..]);
    out
}

/// 链接标签里的转义：`\` `[` `]`。
pub fn escape_label(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

/// 链接目标里的转义：`\` `>`。
pub fn escape_dest(s: &str) -> String {
    s.replace('\\', "\\\\").replace('>', "\\>")
}

/// 会话的桌面端写法：`[#标题](#sess_…)`；没有标题的会话与桌面端一样只发 `#sess_…`。
pub fn session_link(session_id: &str, title: &str) -> String {
    let t = title.trim();
    if t.is_empty() {
        format!("#{session_id}")
    } else {
        format!(
            "[{}](#{})",
            escape_label(&format!("#{t}")),
            escape_dest(session_id)
        )
    }
}

/// JS 正则的 `.`：行结束符以外的字。
pub fn not_line_end(c: char) -> bool {
    !matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// 去掉 `\` 转义。
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('\\', Some(n)) if not_line_end(*n) => {
                out.push(*n);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// 开始技能提及的符号：`$`，与中文输入法的 `¥` `￥`。
pub const SIGNS: [char; 3] = ['$', '¥', '￥'];

/// 从 `i` 起一串（`\` 转义的一个字，或不是 `stop` 也不是 `\` 的字）的末尾，`stop` 之前停下。
fn run_to(text: &str, i: usize, stop: char) -> usize {
    let mut chars = text[i..].char_indices().peekable();
    let mut end = i;
    while let Some((k, c)) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some((k2, n)) if not_line_end(*n) => {
                    end = i + k2 + n.len_utf8();
                    chars.next();
                }
                _ => break,
            }
        } else if c == stop {
            break;
        } else {
            end = i + k + c.len_utf8();
        }
    }
    end
}

/// 桌面端的 Markdown 链接，带转义与 `<…>` 的写法：开头、末尾、标签（未去转义）、目标。
fn links(text: &str) -> Vec<(usize, usize, &str, &str)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(k) = text[i..].find('[') {
        let s = i + k;
        let label_end = run_to(text, s + 1, ']');
        let found = text[label_end..].starts_with("](").then(|| {
            let d0 = label_end + 2;
            let angled = text[d0..].starts_with('<').then(|| {
                let e = run_to(text, d0 + 1, '>');
                text[e..]
                    .starts_with(">)")
                    .then_some((e + 2, &text[d0 + 1..e]))
            });
            angled.flatten().or_else(|| {
                let e = run_to(text, d0, ')');
                text[e..].starts_with(')').then_some((e + 1, &text[d0..e]))
            })
        });
        match found.flatten() {
            Some((end, dest)) => {
                out.push((s, end, &text[s + 1..label_end], dest));
                i = end;
            }
            None => i = s + 1,
        }
    }
    out
}

/// `^[a-zA-Z][a-zA-Z\d+.-]*:.*`：带协议的地址。
fn has_scheme(dest: &str) -> bool {
    let mut cs = dest.chars();
    if !cs.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let rest = cs.as_str();
    match rest.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))) {
        Some(k) => rest[k..].starts_with(':') && rest[k + 1..].chars().all(not_line_end),
        None => false,
    }
}

/// `#sess_[A-Za-z0-9._-]+`：会话的引用。
fn session_dest(dest: &str) -> bool {
    dest.strip_prefix("#sess_").is_some_and(|id| {
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    })
}

/// 发出的回合分成段，提及的段带上种类（`skill`、`file`、`work`、`agent`；文字为 ""）：这里或桌面端的链接读成 `@标签`
/// （技能的是 `$名字`）；已知技能的光秃 `$名字` 与已知子代理的 `@名字` 也是提及。
pub fn parts(
    text: &str,
    skills: &dyn Fn(&str) -> bool,
    agents: &dyn Fn(&str) -> bool,
) -> Vec<(String, &'static str)> {
    let linked: Vec<(usize, usize, String, &'static str)> = links(text)
        .into_iter()
        .filter_map(|(s, e, label, dest)| {
            let label = unescape(label);
            let shown = if session_dest(dest) {
                Some((
                    format!("@{}", label.strip_prefix('#').unwrap_or(&label)),
                    "work",
                ))
            } else if dest.starts_with("#wfrun_") {
                Some((
                    format!("@{}", label.strip_prefix('%').unwrap_or(&label)),
                    "work",
                ))
            } else if label.starts_with('$') {
                Some((label, "skill"))
            } else if dest.starts_with('#') || has_scheme(dest) || dest.is_empty() {
                None
            } else {
                Some((
                    format!("@{}", label.strip_prefix('@').unwrap_or(&label)),
                    "file",
                ))
            };
            shown.map(|(t, k)| (s, e, t, k))
        })
        .collect();
    let free = |s: usize, e: usize| !linked.iter().any(|l| s < l.1 && e > l.0);
    let mut all: Vec<(usize, usize, String, &'static str)> = Vec::new();
    for (s, e, n) in bares(text, '$', &SIGNS) {
        if skills(n) && free(s, e) {
            all.push((s, e, format!("${n}"), "skill"));
        }
    }
    for (s, e, n) in bares(text, '@', &[]) {
        if agents(n) && free(s, e) {
            all.push((s, e, format!("@{n}"), "agent"));
        }
    }
    all.extend(linked);
    all.sort_by_key(|t| t.0);
    let mut out = Vec::new();
    let mut at = 0;
    for (s, e, label, kind) in all {
        if s >= at {
            if s > at {
                out.push((text[at..s].to_owned(), ""));
            }
            out.push((label, kind));
            at = e;
        }
    }
    if at < text.len() {
        out.push((text[at..].to_owned(), ""));
    }
    out
}

/// 读起来的文字：链接显示成它们的 `@标签`（与 `$名字`）。
pub fn unlinked(text: &str) -> String {
    parts(text, &|_| false, &|_| false)
        .into_iter()
        .map(|(t, _)| t)
        .collect()
}
