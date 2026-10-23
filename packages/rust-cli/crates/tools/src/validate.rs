//! 工具输入校验：JSON Schema 子集，报错格式见 spec rust-cli/tools.md。
//!
//! 能归类的三种问题：缺少必填参数、多出的参数（`additionalProperties: false`）、
//! 类型不符。其余问题（enum、范围等）没有统一格式，用 `"<path>: <说明>"` 列出（spec rust-cli/tools.md）。

use serde_json::{Map, Value};

enum Issue {
    Missing(String),
    Unexpected(String),
    Mismatch {
        path: String,
        expected: String,
        received: &'static str,
    },
    Other(String),
}

/// zod 的 `received` 用 JavaScript 的类型名。
fn received(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn matches_type(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|number| number.fract() == 0.0),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn check(schema: &Value, value: &Value, path: &str, issues: &mut Vec<Issue>) {
    if let Some(expected) = schema.get("type").and_then(Value::as_str)
        && !matches_type(expected, value)
    {
        issues.push(Issue::Mismatch {
            path: path.to_owned(),
            expected: expected.to_owned(),
            received: received(value),
        });
        return;
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(value)
    {
        issues.push(Issue::Other(format!(
            "{path}: value is not one of the allowed options"
        )));
    }
    match value {
        Value::Object(object) => check_object(schema, object, path, issues),
        Value::Array(items) => {
            if let Some(item_schema) = schema.get("items") {
                for (index, item) in items.iter().enumerate() {
                    check(item_schema, item, &format!("{path}[{index}]"), issues);
                }
            }
        }
        _ => {}
    }
}

fn check_object(schema: &Value, object: &Map<String, Value>, path: &str, issues: &mut Vec<Issue>) {
    let properties = schema.get("properties").and_then(Value::as_object);
    for required in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        // 有 `default` 的字段在 zod 的输入校验里可以缺省（输出形的 JSON Schema 仍把它列进 required，
        // 例如 ToolSearch 的 `max_results`、AskUserQuestion 的 `multiSelect`）。
        let defaulted = properties
            .and_then(|properties| properties.get(required))
            .is_some_and(|property| property.get("default").is_some());
        if !object.contains_key(required) && !defaulted {
            issues.push(Issue::Missing(join(path, required)));
        }
    }
    let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
    for (key, value) in object {
        match properties.and_then(|properties| properties.get(key)) {
            Some(property) => check(property, value, &join(path, key), issues),
            None if closed => issues.push(Issue::Unexpected(key.clone())),
            None => {}
        }
    }
}

/// zod `z.object` 的 strip（修复线 F51，agents.md §2）：去掉 schema `properties` 以外的顶层键；不是对象、或 schema
/// 没有 `properties` 时原样返回。
pub(crate) fn strip_unknown(schema: &Value, mut input: Value) -> Value {
    if let (Value::Object(object), Some(properties)) = (
        &mut input,
        schema.get("properties").and_then(Value::as_object),
    ) {
        object.retain(|key, _| properties.contains_key(key));
    }
    input
}

/// 校验通过时为 None；否则是 `<tool_use_error>InputValidationError: …</tool_use_error>` 里的正文。
pub(crate) fn validate(tool: &str, schema: &Value, input: &Value) -> Option<String> {
    let mut issues = Vec::new();
    check(schema, input, "", &mut issues);
    if issues.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for issue in &issues {
        if let Issue::Missing(param) = issue {
            parts.push(format!("The required parameter `{param}` is missing"));
        }
    }
    for issue in &issues {
        if let Issue::Unexpected(param) = issue {
            parts.push(format!("An unexpected parameter `{param}` was provided"));
        }
    }
    for issue in &issues {
        if let Issue::Mismatch {
            path,
            expected,
            received,
        } = issue
        {
            parts.push(format!(
                "The parameter `{path}` type is expected as `{expected}` but provided as `{received}`"
            ));
        }
    }
    if parts.is_empty() {
        let other: Vec<&str> = issues
            .iter()
            .filter_map(|issue| match issue {
                Issue::Other(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        return Some(other.join("\n"));
    }
    let noun = if parts.len() > 1 { "issues" } else { "issue" };
    Some(format!(
        "{tool} failed due to the following {noun}:\n{}",
        parts.join("\n")
    ))
}

#[cfg(test)]
#[path = "validate_tests.rs"]
mod tests;
