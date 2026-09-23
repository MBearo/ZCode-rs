use super::{
    model_protocol::{delta, index, required},
    model_stream::{Assembly, TextBuffer},
};
use crate::contract::{ModelFailure, ModelOutput};
use serde_json::{Value, json};
use std::collections::BTreeMap;
#[derive(Default)]
pub(super) struct Responses {
    pub inner: Assembly,
    items: BTreeMap<u64, Item>,
}
struct Item {
    value: Value,
    text: String,
    arguments: String,
    done: bool,
}
impl Responses {
    pub async fn consume(
        &mut self,
        data: &str,
        output: &mut TextBuffer<'_>,
    ) -> Result<(), ModelFailure> {
        let value: Value = serde_json::from_str(data).map_err(|_| ModelFailure::invalid())?;
        let kind = required(&value["type"])?;
        match kind {
            "error" | "response.failed" => {
                let error = if kind == "error" {
                    value.get("error").unwrap_or(&value)
                } else {
                    &value["response"]["error"]
                };
                return Err(super::model_failure::response(
                    None,
                    &json!({"error":error}),
                    &Default::default(),
                ));
            }
            "response.output_item.added" => {
                let n = index(&value["output_index"])?;
                let item = &value["item"];
                if self.items.contains_key(&n)
                    || required(&item["id"])?.is_empty()
                    || required(&item["id"])?.len() > 512
                    || self
                        .items
                        .values()
                        .any(|known| known.value["id"] == item["id"])
                {
                    return Err(ModelFailure::invalid());
                }
                let arguments = match required(&item["type"])? {
                    "function_call" => {
                        let args = required(&item["arguments"])?;
                        delta(&mut self.inner,output,json!({"tool_calls":[{"index":n,"id":item["call_id"],"type":"function","function":{"name":item["name"],"arguments":args}}]})).await?;
                        args.to_owned()
                    }
                    "message" | "reasoning" => String::new(),
                    _ => return Err(ModelFailure::invalid()),
                };
                self.items.insert(
                    n,
                    Item {
                        value: json!({"id":item["id"],"type":item["type"],"call_id":item["call_id"],"name":item["name"]}),
                        text: String::new(),
                        arguments,
                        done: false,
                    },
                );
            }
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_summary_text.delta"
            | "response.function_call_arguments.delta" => {
                let n = index(&value["output_index"])?;
                let item = self.items.get_mut(&n).ok_or_else(ModelFailure::invalid)?;
                if item.done || value["item_id"] != item.value["id"] {
                    return Err(ModelFailure::invalid());
                }
                let text = required(&value["delta"])?;
                match kind {
                    "response.function_call_arguments.delta" => {
                        if item.value["type"] != "function_call" {
                            return Err(ModelFailure::invalid());
                        }
                        item.arguments.push_str(text);
                        delta(
                            &mut self.inner,
                            output,
                            json!({"tool_calls":[{"index":n,"function":{"arguments":text}}]}),
                        )
                        .await?;
                    }
                    "response.reasoning_summary_text.delta" => {
                        if item.value["type"] != "reasoning" {
                            return Err(ModelFailure::invalid());
                        }
                        item.text.push_str(text);
                        delta(&mut self.inner, output, json!({"reasoning_content":text})).await?;
                    }
                    _ => {
                        if item.value["type"] != "message" {
                            return Err(ModelFailure::invalid());
                        }
                        item.text.push_str(text);
                        delta(&mut self.inner, output, json!({"content":text})).await?;
                    }
                }
            }
            "response.function_call_arguments.done" => {
                let item = self
                    .items
                    .get(&index(&value["output_index"])?)
                    .ok_or_else(ModelFailure::invalid)?;
                if item.done
                    || item.value["id"] != value["item_id"]
                    || item.arguments != required(&value["arguments"])?
                {
                    return Err(ModelFailure::invalid());
                }
            }
            "response.output_item.done" => {
                let n = index(&value["output_index"])?;
                let completed = &value["item"];
                let item = self.items.get_mut(&n).ok_or_else(ModelFailure::invalid)?;
                if item.done
                    || item.value["id"] != completed["id"]
                    || item.value["type"] != completed["type"]
                    || completed
                        .get("status")
                        .is_some_and(|s| !s.is_null() && s != "completed" && s != "incomplete")
                    || completed.get("role").is_some_and(|r| r != "assistant")
                {
                    return Err(ModelFailure::invalid());
                }
                if completed["type"] == "function_call" {
                    if item.value["call_id"] != completed["call_id"]
                        || item.value["name"] != completed["name"]
                        || item.arguments != required(&completed["arguments"])?
                        || !serde_json::from_str::<Value>(&item.arguments)
                            .is_ok_and(|v| v.is_object())
                    {
                        return Err(ModelFailure::invalid());
                    }
                } else if completed["type"] == "message" {
                    let text = completed["content"]
                        .as_array()
                        .ok_or_else(ModelFailure::invalid)?
                        .iter()
                        .map(|v| match v["type"].as_str() {
                            Some("refusal") => required(&v["refusal"]),
                            Some("output_text") => required(&v["text"]),
                            _ => Err(ModelFailure::invalid()),
                        })
                        .collect::<Result<String, _>>()?;
                    if item.text.is_empty() && !text.is_empty() {
                        delta(&mut self.inner, output, json!({"content":text})).await?;
                    } else if item.text != text {
                        return Err(ModelFailure::invalid());
                    }
                } else {
                    let summary = completed["summary"]
                        .as_array()
                        .ok_or_else(ModelFailure::invalid)?
                        .iter()
                        .map(|v| required(&v["text"]))
                        .collect::<Result<String, _>>()?;
                    if item.text.is_empty() && !summary.is_empty() {
                        delta(
                            &mut self.inner,
                            output,
                            json!({"reasoning_content":summary}),
                        )
                        .await?;
                    } else if item.text != summary {
                        return Err(ModelFailure::invalid());
                    }
                    self.inner
                        .count(completed["encrypted_content"].as_str().map_or(0, str::len))?;
                }
                item.value = clean_item(completed);
                item.done = true;
            }
            "response.completed" | "response.incomplete" => {
                let output_limit = kind == "response.incomplete";
                if value["response"]["status"]
                    != if output_limit {
                        "incomplete"
                    } else {
                        "completed"
                    }
                    || self.items.values().any(|i| !i.done)
                    || (!output_limit
                        && self
                            .items
                            .values()
                            .any(|i| i.value["status"] == "incomplete"))
                    || (output_limit
                        && value["response"]["incomplete_details"]["reason"] != "max_output_tokens")
                {
                    return Err(ModelFailure::invalid());
                }
                let final_items = value["response"]["output"]
                    .as_array()
                    .ok_or_else(ModelFailure::invalid)?;
                if final_items.len() != self.items.len()
                    || final_items.iter().enumerate().any(|(n, item)| {
                        self.items
                            .get(&(n as u64))
                            .is_none_or(|known| known.value != clean_item(item))
                    })
                {
                    return Err(ModelFailure::invalid());
                }
                let usage = &value["response"]["usage"];
                let calls = self
                    .items
                    .values()
                    .any(|i| i.value["type"] == "function_call");
                self.inner.consume_value(&json!({"choices":[{"delta":{},"finish_reason":if output_limit{"length"}else if calls{"tool_calls"}else{"stop"}}],"usage":if usage.is_object(){json!({"prompt_tokens":usage["input_tokens"],"completion_tokens":usage["output_tokens"],"prompt_tokens_details":{"cached_tokens":usage["input_tokens_details"]["cached_tokens"]}})}else{Value::Null}}),output).await?;
                self.inner.done = true;
            }
            _ => {}
        }
        Ok(())
    }
    pub fn finish(self) -> Result<ModelOutput, ModelFailure> {
        let mut output = self.inner.finish()?;
        let reasoning = self
            .items
            .into_values()
            .filter(|i| i.value["type"] == "reasoning")
            .map(|i| i.value)
            .collect::<Vec<_>>();
        if !reasoning.is_empty() {
            output.message["_zcode_responses_reasoning"] = reasoning.into();
        }
        Ok(output)
    }
}

fn clean_item(item: &Value) -> Value {
    let keys = match item["type"].as_str() {
        Some("function_call") => &["type", "id", "call_id", "name", "arguments"][..],
        Some("reasoning") => &["type", "id", "summary", "encrypted_content"][..],
        _ => &["type", "id", "content"][..],
    };
    Value::Object(
        keys.iter()
            .chain(std::iter::once(&"status"))
            .filter_map(|key| item.get(key).map(|v| ((*key).into(), v.clone())))
            .collect(),
    )
}
