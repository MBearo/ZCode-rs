//! Edit 输入的整理与写回（docs/specs/rust-cli/query.md §3「工具输入的整理与写回」）。
//!
//! 主循环组装 assistant 消息时经 `Tool::rewrite_input` 调这里，结果写回 tool_use 块，执行与回放给模型的都是它：按
//! Edit 的 schema 解析，成功时重建成 `{replace_all, file_path, old_string, new_string}`。工作线 F131 从 query 的
//! `defaults.rs` 移来：以前 query 按工具名认 Edit。

use serde_json::{Value, json};

/// Edit schema 的全部键（严格对象，`additionalProperties: false`）。
const EDIT_KEYS: [&str; 4] = ["file_path", "old_string", "new_string", "replace_all"];

/// `replace_all` 的解析：缺省为 false；先预处理，
/// 把字符串 "true" / "false" 换成布尔。其他值解析失败。
fn replace_all(value: Option<&Value>) -> Option<bool> {
    match value {
        None => Some(false),
        Some(Value::Bool(flag)) => Some(*flag),
        Some(Value::String(text)) if text == "true" => Some(true),
        Some(Value::String(text)) if text == "false" => Some(false),
        Some(_) => None,
    }
}

/// 重建 Edit 的输入；schema 解析失败时返回 None（保留原输入）。
///
/// 缺陷修复（L25）：原来按 schema 的 `default` 把 `replace_all: false` 补在最后，回放的键序是
/// `file_path, old_string, new_string, replace_all`。键序来自重建的对象字面量，`replace_all`
/// 总在最前（模型给了它也一样），不是 zod 解析的结果（zod 按 schema 顺序写）。
pub(crate) fn rebuild(input: &Value) -> Option<Value> {
    let object = input.as_object()?;
    if object.keys().any(|key| !EDIT_KEYS.contains(&key.as_str())) {
        return None;
    }
    let text = |key: &str| object.get(key)?.as_str();
    Some(json!({
        "replace_all": replace_all(object.get("replace_all"))?,
        "file_path": text("file_path")?,
        "old_string": text("old_string")?,
        "new_string": text("new_string")?,
    }))
}

#[cfg(test)]
#[path = "edit_input_tests.rs"]
mod tests;
