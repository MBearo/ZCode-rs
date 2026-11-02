//! 权限规则的语法（docs/specs/rust-cli/shell-permissions.md §4.2）：规则串的解析与写回、规则的工具名、Bash 规则内容的
//! 形状与比较。纯字符串运算，policy 的判决链与 hooks 的 `if` 条件共用（F125 的 T4：以前 hooks 另有一份简化的副本，
//! `Bash(git *)` 不匹配 `git`、不并空白、不认转义的括号，已经与判决链分叉）。
//!
//! 规则串是 `Tool` 或 `Tool(content)`：content 里的 `(`、`)`、`\` 用反斜杠转义；`Tool()` 与 `Tool(*)` 等同于整个
//! 工具。旧工具名按别名表换成新名。MCP 规则 `mcp__server` 与 `mcp__server__*` 匹配这个服务器的所有工具。
//!
//! Bash 规则的 content 有三种（[`ShellRule`]）：`git status:*` 是前缀（前缀后必须是结尾或空格），含未转义 `*` 的是
//! 通配，其余精确。拿哪些候选去比（去包装、去前缀赋值）是判决链的事，在 policy。

use regex::Regex;

use crate::permission::PermissionRule;
use crate::tool::canonical_tool_name;

/// 规则的工具名：旧名换成新名（与 hook 的 matcher 共用 [`crate::tool::LEGACY_TOOL_NAMES`]，修复线 F58）。
/// `AgentOutputTool` / `BashOutputTool` 已退役（不再是别名）；保留它们，免得用户已有的 deny 规则失效（保守，README §11
/// F58 的需要用户确认）。
pub fn canonical_rule_tool(name: &str) -> &str {
    match name {
        "AgentOutputTool" | "BashOutputTool" => crate::tool_names::TASK_OUTPUT,
        other => canonical_tool_name(other),
    }
}

/// `index` 处的字符前面的反斜杠是偶数个（没有被转义）。
fn unescaped(text: &str, index: usize) -> bool {
    text[..index]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count()
        % 2
        == 0
}

fn first_unescaped(text: &str, needle: u8) -> Option<usize> {
    (0..text.len()).find(|&index| text.as_bytes()[index] == needle && unescaped(text, index))
}

fn last_unescaped(text: &str, needle: u8) -> Option<usize> {
    (0..text.len())
        .rev()
        .find(|&index| text.as_bytes()[index] == needle && unescaped(text, index))
}

fn unescape(content: &str) -> String {
    content
        .replace(r"\(", "(")
        .replace(r"\)", ")")
        .replace(r"\\", r"\")
}

fn escape(content: &str) -> String {
    content
        .replace('\\', r"\\")
        .replace('(', r"\(")
        .replace(')', r"\)")
}

/// 规则串解析成规则值。
pub fn parse_rule(text: &str) -> PermissionRule {
    let whole = || PermissionRule {
        tool: canonical_rule_tool(text).to_owned(),
        content: None,
    };
    let Some(open) = first_unescaped(text, b'(') else {
        return whole();
    };
    let Some(close) = last_unescaped(text, b')') else {
        return whole();
    };
    if close <= open || close != text.len() - 1 || open == 0 {
        return whole();
    }
    let tool = canonical_rule_tool(&text[..open]).to_owned();
    let raw = &text[open + 1..close];
    let content = (!raw.is_empty() && raw != "*").then(|| unescape(raw));
    PermissionRule { tool, content }
}

/// 规则值写回规则串。
pub fn format_rule(rule: &PermissionRule) -> String {
    match rule.content.as_deref() {
        None | Some("") => rule.tool.clone(),
        Some(content) => format!("{}({})", rule.tool, escape(content)),
    }
}

/// 规则的工具名是否指向这个工具（同名，或 MCP 服务器规则 `mcp__<server>` / `mcp__<server>__*`）。名字的拆法只在
/// [`crate::mcp_name`]（F132 的 T10）。
pub fn tool_matches(rule_tool: &str, tool: &str) -> bool {
    if rule_tool == tool {
        return true;
    }
    match crate::mcp_name::parse(rule_tool) {
        Some(crate::mcp_name::McpName {
            server,
            tool: None | Some("*"),
        }) => tool.starts_with(&crate::mcp_name::server_prefix(server)),
        Some(_) | None => false,
    }
}

/// 一条 Bash 规则的 content。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellRule<'a> {
    Exact(&'a str),
    Prefix(&'a str),
    Wildcard(&'a str),
}

impl<'a> ShellRule<'a> {
    /// 解析一条 Bash 规则的 content：`x:*`（`x` 非空）是前缀，含未转义 `*` 的是通配，其余精确。
    pub fn parse(content: &'a str) -> Self {
        if let Some(prefix) = content
            .strip_suffix(":*")
            .filter(|prefix| !prefix.is_empty())
        {
            return Self::Prefix(prefix);
        }
        if has_wildcard(content) {
            Self::Wildcard(content)
        } else {
            Self::Exact(content)
        }
    }
}

fn has_wildcard(pattern: &str) -> bool {
    if pattern.ends_with(":*") {
        return false;
    }
    pattern
        .char_indices()
        .any(|(index, c)| c == '*' && unescaped(pattern, index))
}

/// Bash 规则的 content 显示时去掉末尾的 ` *` / `:*`（询问框列出几条建议规则的那一行，shell-permissions.md §4.10）。
pub fn shell_rule_label(content: &str) -> &str {
    content
        .strip_suffix(":*")
        .or_else(|| content.strip_suffix(" *"))
        .unwrap_or(content)
}

/// 去掉末尾空白后以未转义的 `*` 结尾（allow 的通配规则这时也认 `xargs`）。
pub fn ends_with_wildcard(pattern: &str) -> bool {
    let pattern = pattern.trim_end();
    pattern.ends_with('*') && unescaped(pattern, pattern.len() - 1)
}

/// 连续的空格、制表符并成一个空格。
pub fn squeeze(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t') {
            if !blank {
                out.push(' ');
            }
            blank = true;
        } else {
            out.push(c);
            blank = false;
        }
    }
    out
}

/// 前缀规则：两边并空白后，命令等于前缀或以“前缀 + 空格”开头；`xargs <前缀>` 也算。
pub fn prefix_matches(prefix: &str, command: &str) -> bool {
    let (prefix, command) = (squeeze(prefix), squeeze(command));
    let xargs = format!("xargs {prefix}");
    command == prefix
        || command.starts_with(&format!("{prefix} "))
        || command == xargs
        || command.starts_with(&format!("{xargs} "))
}

/// `/**/` 里 `/` 之后的一段（可以连着几段）。
const GLOBSTAR_SEGMENT: &str = "**/";
/// 唯一的 `*` 在结尾 ` *` 时翻译出的正则尾巴（这时它可以整个省略）。
const TRAILING_ANY: &str = " .*";

/// 通配匹配：`*` 匹配任意文本，`\*` 是字面星号，`/**/` 也匹配 `/`；只有一个 `*` 且以 ` *` 结尾时，
/// 结尾部分可以整个省略（`git *` 也匹配 `git`）。比较前模式与命令里连续的空格、制表符并成一个空格。
///
/// 修复（工作线 F85）：以前不并空白，`Bash(cp *)` 拦不住 `cp⇥x y`。
pub fn wildcard_matches(pattern: &str, command: &str) -> bool {
    let pattern = squeeze(pattern.trim());
    let command = squeeze(command);
    let mut regex = String::from("^");
    let mut stars = 0;
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'*') => {
                chars.next();
                regex.push_str(r"\*");
            }
            '\\' if chars.peek() == Some(&'\\') => {
                chars.next();
                regex.push_str(r"\\");
            }
            '/' if chars.peek() == Some(&'*') => {
                // `/**/`（可以连着几段）匹配 `/` 或 `/…/`。
                let rest: String = chars.clone().collect();
                let mut globstars = 0;
                while rest[globstars * GLOBSTAR_SEGMENT.len()..].starts_with(GLOBSTAR_SEGMENT) {
                    globstars += 1;
                }
                if globstars > 0 {
                    for _ in 0..globstars * GLOBSTAR_SEGMENT.len() {
                        chars.next();
                    }
                    stars += globstars * 2;
                    regex.push_str("/(?:.*/)?");
                } else {
                    regex.push('/');
                }
            }
            '*' => {
                stars += 1;
                regex.push_str(".*");
            }
            other => {
                if ".+?^${}()|[]\\'\"".contains(other) {
                    regex.push('\\');
                }
                regex.push(other);
            }
        }
    }
    if stars == 1 && regex.ends_with(TRAILING_ANY) {
        regex.truncate(regex.len() - TRAILING_ANY.len());
        regex.push_str("( .*)?");
    }
    regex.push('$');
    Regex::new(&format!("(?s){regex}")).is_ok_and(|re| re.is_match(&command))
}

#[cfg(test)]
#[path = "syntax_tests.rs"]
mod tests;
