//! 会话标题与不带参数的 `/rename` 的提示词（`packages/rust-cli/prompts/title/`，docs/specs/rust-cli/titles.md
//! §1、§8.4）。
//!
//! 文字逐字固定，没有产品名需要替换。user 提示是 `<session>…</session>` 加语言行。

use rust_cli_base::include_lf;
use serde::Deserialize;

use crate::trim;

/// system 提示。
pub fn system() -> &'static str {
    trim(include_lf!("../../../prompts/title/system.txt"))
}

/// user 提示的模板：`{{content}}` 与 `{{language}}`。
pub fn user() -> &'static str {
    trim(include_lf!("../../../prompts/title/user.txt"))
}

/// 没有语言设置时的语言行。
pub fn language_default() -> &'static str {
    trim(include_lf!("../../../prompts/title/language_default.txt"))
}

/// 指定语言时的语言行：`{{language}}`。
pub fn language_explicit() -> &'static str {
    trim(include_lf!("../../../prompts/title/language_explicit.txt"))
}

/// 不带参数的 `/rename` 的 system 起名提示（
/// docs/specs/rust-cli/titles.md §8.4）。
pub fn rename_system() -> &'static str {
    trim(include_lf!("../../../prompts/title/rename_system.txt"))
}

/// 起名请求的 user 模板：`{{conversation}}`。
pub fn rename_user() -> &'static str {
    trim(include_lf!("../../../prompts/title/rename_user.txt"))
}

/// 起名请求的 structured output 格式（`output_config.format`）；数据文件读不出时为 Null。
pub fn rename_format() -> serde_json::Value {
    serde_json::from_str(include_lf!("../../../prompts/title/rename_schema.json"))
        .unwrap_or(serde_json::Value::Null)
}

/// 起名的常量（`limits.json`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Limits {
    /// 去首尾空白后少于这么多字符不生成。
    pub min_input_chars: usize,
    /// 实验开关打开时的输入截断（默认关，本工程不用）。
    pub input_cap_chars: usize,
    pub input_cap_head_chars: usize,
    /// 从整段对话重起标题时取的末尾字符数（本工程不用）。
    pub conversation_tail_chars: usize,
}

pub fn limits() -> Limits {
    const FALLBACK: Limits = Limits {
        min_input_chars: 10,
        input_cap_chars: 2000,
        input_cap_head_chars: 1500,
        conversation_tail_chars: 1000,
    };
    serde_json::from_str(include_lf!("../../../prompts/title/limits.json")).unwrap_or(FALLBACK)
}

#[cfg(test)]
#[path = "title_tests.rs"]
mod tests;
