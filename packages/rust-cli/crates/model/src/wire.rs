//! 三种线协议共用的小件（docs/specs/rust-cli/model.md §1）：等响应头的时限与温度的写法。

use std::time::Duration;

use rust_cli_kernel_api::model::ModelError;
use serde_json::{Number, Value};

/// 等待响应头的时限：SDK 默认 timeout；值在 net 的单次超时表（F58）。
pub(crate) const HEADER_TIMEOUT: Duration = rust_cli_net::timeout::MODEL;

/// f32 按最短十进制表示转成 JSON 数字（0.7 是 `0.7` 而不是 `0.699999988079071`；1 是 `1`）。
pub(crate) fn number(value: f32) -> Result<Value, ModelError> {
    format!("{value}")
        .parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .map(Value::Number)
        .ok_or_else(|| ModelError::InvalidRequest {
            message: format!("temperature must be finite, got {value}"),
        })
}

#[cfg(test)]
#[path = "wire_tests.rs"]
pub(crate) mod tests;
