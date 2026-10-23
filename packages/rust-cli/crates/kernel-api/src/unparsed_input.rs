//! tool_use 的输入不是合法 JSON 时的标记对象（docs/specs/rust-cli/query.md §3「tool_use 输入不是 JSON」）。
//!
//! 组装 assistant 消息时输入换成只有 [`KEY`] 一个键的对象，原文保留前 [`RAW_UNITS`] 个 UTF-16
//! 单元与总长度；执行管线在 schema 校验之前认出它，给模型 JSON 解析失败的 InputValidationError。
//! 回放给模型的 tool_use 输入就是这个对象。

use rust_cli_base::js::string::utf16_prefix;
use serde_json::{Value, json};

/// 标记对象唯一的键。
pub const KEY: &str = "__unparsedToolInput";
/// 原文保留的 UTF-16 单元数（不拆开代理对）。
pub const RAW_UNITS: usize = 2048;

/// 标记里的原文（可能已截断）与原文的 UTF-16 长度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unparsed<'a> {
    pub raw: &'a str,
    pub len: u64,
}

/// 原文 `raw` 的标记对象：`{KEY: {raw: 前 2048 个单元, len: 总长度}}`。
pub fn marker(raw: &str) -> Value {
    json!({ KEY: { "raw": utf16_prefix(raw, RAW_UNITS), "len": raw.encode_utf16().count() } })
}

/// 输入是标记对象时取出原文与长度（只有这一个键，`raw` 是字符串、`len` 是数）。
pub fn parse(input: &Value) -> Option<Unparsed<'_>> {
    let object = input.as_object()?;
    if object.len() != 1 {
        return None;
    }
    let inner = object.get(KEY)?;
    Some(Unparsed {
        raw: inner.get("raw")?.as_str()?,
        len: inner.get("len")?.as_f64()? as u64,
    })
}

#[cfg(test)]
#[path = "unparsed_input_tests.rs"]
mod tests;
