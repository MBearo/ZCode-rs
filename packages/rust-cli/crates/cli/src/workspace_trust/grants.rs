//! 对话框里的项目授权（docs/specs/rust-cli/workspace-trust.md §2.4）：项目级配置文件里会生效的
//! `permission.allowedTools`，排序、写成显示的形式、去掉控制字符、截断、去重。

use rust_cli_policy::rules::{format_rule, parse_rule};
use rust_cli_tui::trust::Grants;

/// 排在前两组的工具：Bash、PowerShell、写文件的工具、WebFetch、WebSearch，以及 MCP 工具。
const SPECIAL: [&str; 8] = [
    "Bash",
    "PowerShell",
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
];
/// 一条规则最多显示的字符数，超出的截断加 `…`。
const RULE_CHARS: usize = 60;

/// 排序的分组（小的在前）。
const SPECIAL_WHOLE: u8 = 0;
const SPECIAL_WITH_CONTENT: u8 = 1;
const OTHER_WHOLE: u8 = 2;
const OTHER_WITH_CONTENT: u8 = 3;

/// 分组：特殊工具整条 0、特殊工具带内容 1、其他整条 2、其他带内容 3。
fn group(tool: &str, has_content: bool) -> u8 {
    let special = SPECIAL.contains(&tool) || tool.starts_with("mcp__");
    match (special, has_content) {
        (true, false) => SPECIAL_WHOLE,
        (true, true) => SPECIAL_WITH_CONTENT,
        (false, false) => OTHER_WHOLE,
        (false, true) => OTHER_WITH_CONTENT,
    }
}

/// 显示的形式：去掉控制与格式字符、去首尾空白，超过 60 个字符截断加 `…`。
fn clean(text: &str) -> String {
    let kept: String = text
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{feff}'))
        .collect();
    let trimmed = kept.trim();
    if trimmed.chars().count() > RULE_CHARS {
        let head: String = trimmed.chars().take(RULE_CHARS).collect();
        format!("{head}…")
    } else {
        trimmed.to_owned()
    }
}

/// `source` 是写了这些规则的文件（相对工作区根）；列表为空时没有授权（只在有 allow 规则时显示）。
pub(crate) fn grants(source: &str, texts: &[String]) -> Option<Grants> {
    if texts.is_empty() {
        return None;
    }
    let mut parsed: Vec<_> = texts.iter().map(|text| parse_rule(text)).collect();
    parsed.sort_by_key(|rule| group(&rule.tool, rule.content.is_some()));
    let mut rules: Vec<String> = Vec::new();
    for rule in &parsed {
        let shown = clean(&format_rule(rule));
        if !shown.is_empty() && !rules.contains(&shown) {
            rules.push(shown);
        }
    }
    Some(Grants {
        count: texts.len(),
        rules,
        sources: vec![source.to_owned()],
    })
}

#[cfg(test)]
#[path = "grants_tests.rs"]
mod tests;
