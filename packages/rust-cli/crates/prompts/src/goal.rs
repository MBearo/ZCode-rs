//! `/goal` 与 prompt hook 评估的提示词（`packages/rust-cli/prompts/goal/`，docs/specs/rust-cli/goal.md §2、§5，
//! hooks.md §5.3）。
//!
//! `set_prompt`、`stop_system`、`stop_condition`、`stop_schema` 由 `scripts/rust-cli/prompts/goal.mjs` 生成
//! （`--check` 检查漂移）；`prompt_system`（非 Stop 事件的 system）手工维护。

use rust_cli_base::include_lf;

use crate::trim;

/// 设定目标时给模型的说明：`{{condition}}`。
pub fn set_prompt() -> &'static str {
    trim(include_lf!("../../../prompts/goal/set_prompt.txt"))
}

/// Stop / SubagentStop 评估的 system。
pub fn stop_system() -> &'static str {
    trim(include_lf!("../../../prompts/goal/stop_system.txt"))
}

/// 其他事件的 prompt hook 的 system。
pub fn prompt_system() -> &'static str {
    trim(include_lf!("../../../prompts/goal/prompt_system.txt"))
}

/// Stop 评估的 user 模板：`{{condition}}`（之后按 prompt hook 的规则接 ARGUMENTS）。
pub fn stop_condition() -> &'static str {
    trim(include_lf!("../../../prompts/goal/stop_condition.txt"))
}

/// 评估请求的 structured output 格式（`output_config.format`）；数据文件读不出时为 Null。
pub fn stop_format() -> serde_json::Value {
    serde_json::from_str(include_lf!("../../../prompts/goal/stop_schema.json"))
        .unwrap_or(serde_json::Value::Null)
}
