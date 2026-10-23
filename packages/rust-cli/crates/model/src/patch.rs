//! Provider 的 option map 补丁（docs/specs/rust-cli/model.md §9）：JSON merge patch（RFC 7396）。
//!
//! 与 `rust/crates/shared/src/option_map.rs` 的 `merge_patch` 语义相同（Node 的
//! `applyOptionPatches`）：对象逐键递归合并，`null` 删除该键，其余值整体替换。已有键原位替换，
//! 新键追加在末尾，所以补丁不打乱请求体已有字段的顺序。

use serde_json::{Map, Value};

pub(crate) fn merge(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let Value::Object(object) = target else {
        return;
    };
    for (key, value) in patch {
        if value.is_null() {
            object.shift_remove(key);
        } else {
            merge(object.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

/// 依次合并全部补丁。
pub(crate) fn apply(body: &mut Value, patches: &[Value]) {
    for patch in patches {
        merge(body, patch);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn patches_merge_recursively_and_null_deletes() {
        let mut body = json!({"model": "m", "thinking": {"type": "adaptive", "display": "x"}, "max_tokens": 1});
        apply(
            &mut body,
            &[
                json!({"thinking": {"type": "enabled", "display": null}}),
                json!({"max_tokens": 128000, "reasoning_effort": "high"}),
            ],
        );
        assert_eq!(
            rust_cli_base::js::json::stringify(&body),
            r#"{"model":"m","thinking":{"type":"enabled"},"max_tokens":128000,"reasoning_effort":"high"}"#
        );
    }

    #[test]
    fn a_non_object_patch_replaces_the_value() {
        let mut body = json!({"a": {"b": 1}});
        merge(&mut body, &json!({"a": [1, 2]}));
        assert_eq!(body, json!({"a": [1, 2]}));
    }
}
