//! Rust 版 `analyzeBashCommand`（apps/zcode-cli/packages/core/src/tool/handlers/bash-command-parser.ts）。
//!
//! Node 侧依赖 unbash 4.0.1 解析 bash；这里逐函数移植了 unbash 的 lexer 与 parser 中权限分析会走到
//! 的路径，包括它的各种怪癖（见各模块注释），使 `is_permission_safe(analyze(c))` 与 Node 完全一致，
//! 且 Node 判定安全时整个 `Analysis` 的 JSON 与 Node 相同。复合结构（子 shell、if/for/case、函数、
//! `[[ ]]`、`(( ))` 等）一经识别即停止解析并标记为不支持：此时结果必然不安全，其余字段不再被使用。

mod analysis;
mod js;
mod lexer;
mod nodes;
mod parser;
mod parts;

use serde::{Deserialize, Serialize};

/// Node `MAX_BASH_PARSE_LENGTH`，按 UTF-16 码元计数。
const MAX_BASH_PARSE_LENGTH: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operator {
    #[serde(rename = "&&")]
    And,
    #[serde(rename = "||")]
    Or,
    #[serde(rename = "|")]
    Pipe,
    #[serde(rename = "|&")]
    PipeAll,
    #[serde(rename = "sequence")]
    Sequence,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvAssignment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Redirect {
    /// Node 为 `Number.parseInt` 的结果；超过 `u32::MAX` 的 fd 在这里饱和为 `u32::MAX`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_descriptor: Option<u32>,
    pub operator: String,
    pub target: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invocation {
    pub argv: Vec<String>,
    pub command_text: String,
    pub env_assignments: Vec<EnvAssignment>,
    pub has_assignment_prefix: bool,
    pub has_dynamic_words: bool,
    pub has_redirects: bool,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_before: Option<Operator>,
    pub redirects: Vec<Redirect>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub commands: Vec<Invocation>,
    pub has_dynamic_words: bool,
    pub has_parse_errors: bool,
    pub has_redirects: bool,
    pub has_unsupported_syntax: bool,
    pub unsupported_node_types: Vec<String>,
}

/// 与 Node `analyzeBashCommand` 等价。
pub fn analyze(command: &str) -> Analysis {
    if command.chars().all(js::is_js_whitespace) {
        return Analysis::default();
    }
    if js::utf16_len(command) > MAX_BASH_PARSE_LENGTH {
        return Analysis {
            has_parse_errors: true,
            ..Analysis::default()
        };
    }
    let source = command.as_bytes();
    analysis::collect(source, parser::parse(source))
}

/// 与 Node `isBashCommandPermissionSafe` 等价。
pub fn is_permission_safe(analysis: &Analysis) -> bool {
    !analysis.has_parse_errors && !analysis.has_unsupported_syntax && !analysis.has_dynamic_words
}
