//! 规则语法与查找（docs/specs/rust-cli/shell-permissions.md §4.2）。
//!
//! 规则串是 `Tool` 或 `Tool(content)`：content 里的 `(`、`)`、`\` 用反斜杠转义；`Tool()` 与
//! `Tool(*)` 等同于整个工具。旧工具名按别名表换成新名。MCP 规则 `mcp__server`
//! 与 `mcp__server__*` 匹配这个服务器的所有工具。

use rust_cli_kernel_api::permission::{
    PermissionRule, PermissionRules, RuleBehavior, RuleSource, SourcedRule,
};

/// 旧工具名（与 hook 的 matcher 共用 kernel-api 的一张表，修复线 F58）。以前这里是一张只有
/// 4 项的别名表。`AgentOutputTool` / `BashOutputTool` 已退役（不再是
/// 别名）；保留它们，免得用户已有的 deny 规则失效（保守，README §11 F58 的需要用户确认）。
fn canonical(name: &str) -> &str {
    match name {
        "AgentOutputTool" | "BashOutputTool" => "TaskOutput",
        other => rust_cli_kernel_api::tool::canonical_tool_name(other),
    }
}

fn unescaped(text: &str, index: usize) -> bool {
    let slashes = text[..index]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count();
    slashes % 2 == 0
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
        tool: canonical(text).to_owned(),
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
    let tool = canonical(&text[..open]).to_owned();
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

/// 规则的工具名是否指向这个工具。
pub fn tool_matches(rule_tool: &str, tool: &str) -> bool {
    if rule_tool == tool {
        return true;
    }
    let Some(server) = rule_tool.strip_prefix("mcp__") else {
        return false;
    };
    let server = server.strip_suffix("__*").unwrap_or(server);
    tool.strip_prefix("mcp__")
        .and_then(|rest| rest.strip_prefix(server))
        .is_some_and(|rest| rest.starts_with("__"))
        && !server.contains("__")
}

/// 一次判决看到的规则：配置快照（含“总是允许”落库后的 LocalSettings 层），加上只对这一次判决生效的允许规则
/// （skill 的 `allowed-tools`）。
pub struct Rules<'a> {
    pub base: &'a PermissionRules,
    pub session: &'a [PermissionRule],
}

/// 找到的规则与它的来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<'a> {
    pub rule: &'a PermissionRule,
    pub source: RuleSource,
}

impl Found<'_> {
    pub fn text(&self) -> String {
        format_rule(self.rule)
    }
}

impl<'a> Rules<'a> {
    fn all(&self) -> impl Iterator<Item = (RuleBehavior, Found<'a>)> + '_ {
        let base = self.base.rules.iter().map(
            |SourcedRule {
                 source,
                 behavior,
                 rule,
             }| {
                (
                    *behavior,
                    Found {
                        rule,
                        source: *source,
                    },
                )
            },
        );
        let session = self.session.iter().map(|rule| {
            (
                RuleBehavior::Allow,
                Found {
                    rule,
                    source: RuleSource::Session,
                },
            )
        });
        base.chain(session)
    }

    /// 针对整个工具的规则（没有 content）。
    pub fn whole_tool(&self, tool: &str, behavior: RuleBehavior) -> Option<Found<'a>> {
        self.all()
            .filter(|(kind, found)| *kind == behavior && found.rule.content.is_none())
            .map(|(_, found)| found)
            .find(|found| tool_matches(&found.rule.tool, tool))
    }

    /// 某个工具名下带 content 的规则。
    pub fn contents(&self, tool: &str, behavior: RuleBehavior) -> Vec<(&'a str, Found<'a>)> {
        self.all()
            .filter(|(kind, found)| *kind == behavior && found.rule.tool == tool)
            .filter_map(|(_, found)| {
                found
                    .rule
                    .content
                    .as_deref()
                    .map(|content| (content, found))
            })
            .collect()
    }

    pub fn additional_directories(&self) -> &'a [rust_cli_kernel_api::env::EnvPath] {
        &self.base.additional_directories
    }
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
