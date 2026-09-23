use super::{
    model_protocol::{delta, index, required},
    model_stream::{Assembly, TextBuffer},
};
use crate::contract::{ModelFailure, ModelOutput};
use serde_json::{Value, json};
use std::collections::BTreeMap;
#[derive(Default)]
pub(super) struct Anthropic {
    pub inner: Assembly,
    blocks: BTreeMap<u64, Block>,
    started: bool,
    stopped: bool,
    usage: Value,
}
struct Block {
    value: Value,
    arguments: String,
    closed: bool,
    signature_delta: bool,
}
impl Anthropic {
    pub async fn consume(
        &mut self,
        data: &str,
        output: &mut TextBuffer<'_>,
    ) -> Result<(), ModelFailure> {
        let value: Value = serde_json::from_str(data).map_err(|_| ModelFailure::invalid())?;
        match required(&value["type"])? {
            "error" => {
                return Err(super::model_failure::response(
                    None,
                    &value,
                    &Default::default(),
                ));
            }
            "message_start" => {
                if self.started {
                    return Err(ModelFailure::invalid());
                }
                self.started = true;
                self.usage = value["message"]["usage"].clone();
            }
            "content_block_start" => {
                let n = index(&value["index"])?;
                if !self.started || self.stopped || self.blocks.contains_key(&n) {
                    return Err(ModelFailure::invalid());
                }
                let block = &value["content_block"];
                let mut args = String::new();
                match required(&block["type"])? {
                    "tool_use" => {
                        if !block["input"].is_object() {
                            return Err(ModelFailure::invalid());
                        }
                        if block["input"].as_object().is_some_and(|o| !o.is_empty()) {
                            args = block["input"].to_string();
                        }
                        delta(&mut self.inner,output,json!({"tool_calls":[{"index":n,"id":block["id"],"type":"function","function":{"name":block["name"],"arguments":args}}]})).await?;
                    }
                    "text" => {
                        delta(
                            &mut self.inner,
                            output,
                            json!({"content":required(&block["text"])?}),
                        )
                        .await?
                    }
                    "thinking" => {
                        self.inner
                            .count(block["signature"].as_str().map_or(0, str::len))?;
                        delta(
                            &mut self.inner,
                            output,
                            json!({"reasoning_content":required(&block["thinking"])?}),
                        )
                        .await?
                    }
                    "redacted_thinking" => self.inner.count(required(&block["data"])?.len())?,
                    _ => return Err(ModelFailure::invalid()),
                }
                self.blocks.insert(
                    n,
                    Block {
                        value: match block["type"].as_str() {
                            Some("thinking")=>json!({"type":"thinking","thinking":block["thinking"],"signature":block["signature"].as_str().unwrap_or("")}),
                            Some("redacted_thinking")=>json!({"type":"redacted_thinking","data":block["data"]}),
                            _=>json!({"type":block["type"]}),
                        },
                        arguments: args,
                        closed: false,
                        signature_delta: false,
                    },
                );
            }
            "content_block_delta" => {
                let n = index(&value["index"])?;
                let block = self.blocks.get_mut(&n).ok_or_else(ModelFailure::invalid)?;
                if block.closed || self.stopped {
                    return Err(ModelFailure::invalid());
                }
                let d = &value["delta"];
                match required(&d["type"])? {
                    "input_json_delta" if block.value["type"] == "tool_use" => {
                        let args = required(&d["partial_json"])?;
                        block.arguments.push_str(args);
                        delta(
                            &mut self.inner,
                            output,
                            json!({"tool_calls":[{"index":n,"function":{"arguments":args}}]}),
                        )
                        .await?;
                    }
                    "text_delta" if block.value["type"] == "text" => {
                        delta(
                            &mut self.inner,
                            output,
                            json!({"content":required(&d["text"])?}),
                        )
                        .await?
                    }
                    "thinking_delta" if block.value["type"] == "thinking" => {
                        let text = required(&d["thinking"])?;
                        let Value::String(thinking) = &mut block.value["thinking"] else {
                            return Err(ModelFailure::invalid());
                        };
                        thinking.push_str(text);
                        delta(&mut self.inner, output, json!({"reasoning_content":text})).await?;
                    }
                    "signature_delta" if block.value["type"] == "thinking" => {
                        let signature = required(&d["signature"])?;
                        self.inner.count(signature.len())?;
                        let Value::String(current) = &mut block.value["signature"] else {
                            return Err(ModelFailure::invalid());
                        };
                        // 首段替换供应商在 start 中的占位签名；后续原地追加，避免分片越多复制越多。
                        if !block.signature_delta {
                            current.clear();
                        }
                        block.signature_delta = true;
                        current.push_str(signature);
                    }
                    _ => return Err(ModelFailure::invalid()),
                }
            }
            "content_block_stop" => {
                let n = index(&value["index"])?;
                let block = self.blocks.get_mut(&n).ok_or_else(ModelFailure::invalid)?;
                if block.closed || self.stopped {
                    return Err(ModelFailure::invalid());
                }
                if block.value["type"] == "tool_use" {
                    if block.arguments.is_empty() {
                        block.arguments = "{}".into();
                        delta(
                            &mut self.inner,
                            output,
                            json!({"tool_calls":[{"index":n,"function":{"arguments":"{}"}}]}),
                        )
                        .await?;
                    }
                    if !serde_json::from_str::<Value>(&block.arguments).is_ok_and(|v| v.is_object())
                    {
                        return Err(ModelFailure::invalid());
                    }
                }
                block.closed = true;
            }
            "message_delta" => {
                if !self.started || self.stopped || self.blocks.values().any(|b| !b.closed) {
                    return Err(ModelFailure::invalid());
                }
                if let Some(usage) = value["usage"].as_object() {
                    self.usage
                        .as_object_mut()
                        .ok_or_else(ModelFailure::invalid)?
                        .extend(usage.clone());
                }
                if let Some(reason) = value["delta"]["stop_reason"].as_str() {
                    let finish = match reason {
                        "end_turn" | "stop_sequence" => "stop",
                        "tool_use" => "tool_calls",
                        "max_tokens" | "model_context_window_exceeded" => "length",
                        _ => return Err(ModelFailure::invalid()),
                    };
                    let has_calls = self.blocks.values().any(|b| b.value["type"] == "tool_use");
                    if (reason == "tool_use") != has_calls {
                        return Err(ModelFailure::invalid());
                    }
                    let input = self.usage["input_tokens"]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_add(self.usage["cache_read_input_tokens"].as_u64().unwrap_or(0))
                        .saturating_add(
                            self.usage["cache_creation_input_tokens"]
                                .as_u64()
                                .unwrap_or(0),
                        );
                    self.inner.consume_value(&json!({"choices":[{"delta":{},"finish_reason":finish}],"usage":if self.usage.as_object().is_some_and(|u| !u.is_empty()) {json!({"prompt_tokens":input,"completion_tokens":self.usage["output_tokens"],"prompt_tokens_details":{"cached_tokens":self.usage["cache_read_input_tokens"],"cache_write_tokens":self.usage["cache_creation_input_tokens"]}})} else {Value::Null}}),output).await?;
                    self.stopped = true;
                }
            }
            "message_stop" => {
                if !self.stopped || self.blocks.values().any(|b| !b.closed) {
                    return Err(ModelFailure::invalid());
                }
                self.inner.done = true;
            }
            _ => {}
        }
        Ok(())
    }
    pub fn finish(self) -> Result<ModelOutput, ModelFailure> {
        let mut output = self.inner.finish()?;
        let blocks = self
            .blocks
            .into_values()
            .filter(|b| {
                matches!(
                    b.value["type"].as_str(),
                    Some("thinking" | "redacted_thinking")
                )
            })
            .map(|b| b.value)
            .collect::<Vec<_>>();
        if !blocks.is_empty() {
            output.message["_zcode_anthropic_thinking"] = blocks.into();
        }
        Ok(output)
    }
}
