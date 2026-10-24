// 复制自 apps/zcode-cli-rust/crates/bash-parse/src/lib.rs（07ab266d4b）；`crate::` 换成 `crate::parse::`。
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

pub(crate) use nodes::{Complex, Loop, Shape};

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
    /// 命令名与参数的原文（Node `BashCommandInvocation.words` 的来源，修复线 F20 的 guarded 用；不进 JSON）。
    /// 管道带 `time` 关键字时第一条命令前面补一个 `time`（Node `collectPipelineCommands`）。
    #[serde(skip)]
    pub words: Vec<WordText>,
    /// 这条命令里的替换（词、前缀赋值的值、重定向目标、heredoc 正文），按出现的顺序（修复线 F68，
    /// shell-permissions.md §4.5.2；不进 JSON，Node 的分析结果不变）。
    #[serde(skip)]
    pub substitutions: Vec<Substitution>,
    /// 命令在原文里的起点（字节偏移；工作线 F76 的结构化分析按它排先后，不进 JSON）。
    #[serde(skip)]
    pub pos: usize,
    /// 原文截到命令名之后第一个重定向之前（规则比较用的“去掉重定向”，工作线 F85，shell-permissions.md §4.5.6；
    /// 不进 JSON）。没有这样的重定向时同 `command_text`。
    #[serde(skip)]
    pub plain_text: String,
    /// 有前缀赋值，而且赋值的值里没有参数展开（`$NAME`、`${…}`、`$((…))`；整个值是命令替换的照算）。deny / ask 规则的
    /// 候选按它判断这一段“有解析树而且有前缀赋值”（§4.5.6；不进 JSON）。
    #[serde(skip)]
    pub static_assignments: bool,
}

/// 替换的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubstitutionKind {
    /// `$(…)` 或反引号。
    Command,
    /// 双引号里的命令替换，但这个双引号字符串除了展开只有不到两个字符（`"$(cmd)"`）：输出几乎就是参数本身，不能拆。
    Quoted,
    /// `<(…)` 或 `>(…)`。
    Process,
    /// 藏在别的展开里的命令（`${…}`、`$((…))`、花括号展开、extglob、`${ cmd; }`、heredoc 正文）。
    Hidden,
}

/// 命令里的一处替换。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Substitution {
    pub kind: SubstitutionKind,
    /// 能拆成单独的子命令：双引号里的命令替换，或整个赋值的值就是一个命令替换。
    pub splittable: bool,
    /// 命令替换里的命令原文（反引号去掉转义）；藏在别的展开里与进程替换为空。
    pub inner: String,
}

/// 一个词的原文（`Word.text`）与它是否有结构化部件（unbash `Word.parts` 不为空：带引号或展开）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WordText {
    pub text: String,
    pub structured: bool,
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
    /// 管道符在原文里的范围，任意层（工作线 F80，shell-permissions.md §4.5.5；不进 JSON，Node 的分析结果不变）。
    #[serde(skip)]
    pub pipes: Vec<std::ops::Range<usize>>,
    /// `>` / `>>` 重定向在原文里的范围（同上）。
    #[serde(skip)]
    pub outputs: Vec<std::ops::Range<usize>>,
}

/// 与 Node `analyzeBashCommand` 等价。
pub fn analyze(command: &str) -> Analysis {
    if command.chars().all(js::is_space) {
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

/// 结构化的分析结果（工作线 F76，shell-permissions.md §4.5.4）：复合结构里面的简单命令（`[[ … ]]` 是名字为 `[[`
/// 的一条）与结构的事实。只给 Bash 的规则检查用。
pub(crate) struct Structured {
    pub analysis: Analysis,
    pub shape: Shape,
}

/// 复合结构不停下的分析；解析出错（缺 `fi` / `done` / `)`、多出的 token、嵌套过深等）时 None。
pub(crate) fn analyze_structured(command: &str) -> Option<Structured> {
    if command.chars().all(js::is_space) || js::utf16_len(command) > MAX_BASH_PARSE_LENGTH {
        return None;
    }
    let source = command.as_bytes();
    let mut script = parser::parse_structured(source);
    let shape = std::mem::take(&mut script.shape);
    let analysis = analysis::collect(source, script);
    (!analysis.has_parse_errors).then_some(Structured { analysis, shape })
}

/// 与 Node `isBashCommandPermissionSafe` 等价。
pub fn is_permission_safe(analysis: &Analysis) -> bool {
    !analysis.has_parse_errors && !analysis.has_unsupported_syntax && !analysis.has_dynamic_words
}
