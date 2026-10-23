//! 请求体的 `output_config`（docs/specs/rust-cli/model.md §3）：`{effort?, format?}`，effort 在前（起名请求
//! 也是这个顺序）。`format` 是 structured output 的格式
//! （docs/specs/rust-cli/titles.md §8.4），beta 由 `anthropic_betas` 按请求体生成。

use rust_cli_kernel_api::model::OutputFormat;
use serde_json::{Map, Value};

/// 两项都没有时为 None（不写 `output_config`）。
pub(crate) fn config(effort: Option<String>, format: Option<OutputFormat>) -> Option<Value> {
    let mut config = Map::new();
    if let Some(effort) = effort {
        config.insert("effort".into(), Value::String(effort));
    }
    if let Some(OutputFormat::JsonSchema { schema }) = format {
        let mut format = Map::new();
        format.insert("type".into(), Value::String("json_schema".to_owned()));
        format.insert("schema".into(), schema);
        config.insert("format".into(), Value::Object(format));
    }
    (!config.is_empty()).then_some(Value::Object(config))
}
