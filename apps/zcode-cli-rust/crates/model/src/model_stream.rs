use crate::{
    contract::{Event, EventSink, ModelFailure, ModelOutput},
    domain::MAX_TEXT_BYTES,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tokio::time::Instant;

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
}
#[derive(Default)]
pub struct Assembly {
    text: String,
    reasoning: String,
    calls: BTreeMap<u64, Call>,
    usage: Value,
    finish: Option<String>,
    bytes: usize,
    pub done: bool,
}
impl Assembly {
    pub async fn consume(
        &mut self,
        data: &str,
        output: &mut TextBuffer<'_>,
    ) -> Result<(), ModelFailure> {
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let value: Value = serde_json::from_str(data).map_err(|_| ModelFailure::invalid())?;
        self.consume_value(&value, output).await
    }
    pub(super) async fn consume_value(
        &mut self,
        value: &Value,
        output: &mut TextBuffer<'_>,
    ) -> Result<(), ModelFailure> {
        if value.get("error").is_some_and(|e| !e.is_null()) {
            return Err(super::model_failure::response(
                None,
                value,
                &reqwest::header::HeaderMap::new(),
            ));
        }
        if value["usage"].is_object() {
            self.usage = value["usage"].clone();
        }
        let choice = &value["choices"][0];
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish = Some(reason.into());
        }
        let delta = &choice["delta"];
        for (key, reasoning) in [("reasoning_content", true), ("content", false)] {
            if let Some(part) = string(&delta[key])?
                && !part.is_empty()
            {
                self.count(part.len())?;
                if reasoning {
                    self.reasoning.push_str(part);
                } else {
                    self.text.push_str(part);
                }
                output.push(part, reasoning).await?;
            }
        }
        if let Some(parts) = delta.get("tool_calls").filter(|v| !v.is_null()) {
            output.flush().await?;
            for part in parts.as_array().ok_or_else(ModelFailure::invalid)? {
                let index = part["index"]
                    .as_u64()
                    .filter(|n| *n < 128)
                    .ok_or_else(ModelFailure::invalid)?;
                if part
                    .get("type")
                    .is_some_and(|v| !v.is_null() && v != "function")
                {
                    return Err(ModelFailure::invalid());
                }
                let id = string(&part["id"])?.unwrap_or("");
                let name = string(&part["function"]["name"])?.unwrap_or("");
                let args = string(&part["function"]["arguments"])?.unwrap_or("");
                self.count(id.len() + name.len() + args.len())?;
                let call = self.calls.entry(index).or_default();
                call.id.push_str(id);
                call.name.push_str(name);
                call.arguments.push_str(args);
            }
        }
        Ok(())
    }
    pub(super) fn count(&mut self, bytes: usize) -> Result<(), ModelFailure> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > MAX_TEXT_BYTES {
            Err(ModelFailure::invalid())
        } else {
            Ok(())
        }
    }
    pub fn finish(self) -> Result<ModelOutput, ModelFailure> {
        if !self.done {
            return Err(ModelFailure::new("network_error", true));
        }
        let output_limit = matches!(
            self.finish.as_deref(),
            Some("length" | "max_tokens" | "max_output_tokens" | "model_context_window_exceeded")
        );
        if !output_limit && !matches!(self.finish.as_deref(), Some("stop" | "tool_calls")) {
            return Err(ModelFailure::invalid());
        }
        if output_limit && !self.calls.is_empty() {
            return Err(ModelFailure::invalid());
        }
        if self.finish.as_deref() == Some("tool_calls") && self.calls.is_empty() {
            return Err(ModelFailure::invalid());
        }
        let mut ids = BTreeSet::new();
        let mut calls = vec![];
        for call in self.calls.into_values() {
            if call.id.trim().is_empty()
                || call.name.trim().is_empty()
                || !ids.insert(call.id.clone())
            {
                return Err(ModelFailure::invalid());
            }
            calls.push(json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments}}));
        }
        if !output_limit
            && self.text.is_empty()
            && calls.is_empty()
            && self.usage.as_object().is_none_or(|m| m.is_empty())
        {
            return Err(ModelFailure::empty());
        }
        let mut message = json!({"role":"assistant","content":self.text});
        if !self.reasoning.is_empty() {
            message["reasoning_content"] = self.reasoning.into();
        }
        if !calls.is_empty() {
            message["tool_calls"] = calls.clone().into();
        }
        Ok(ModelOutput {
            message,
            calls,
            usage: self.usage,
            output_limit,
        })
    }
}
fn string(value: &Value) -> Result<Option<&str>, ModelFailure> {
    if value.is_null() {
        Ok(None)
    } else {
        value.as_str().map(Some).ok_or_else(ModelFailure::invalid)
    }
}

pub struct TextBuffer<'a> {
    sink: &'a EventSink,
    response: String,
    pending: String,
    reasoning: bool,
    pub deadline: Option<Instant>,
    pub committed: bool,
}
impl<'a> TextBuffer<'a> {
    pub fn new(sink: &'a EventSink) -> Self {
        Self {
            sink,
            response: super::id(),
            pending: String::new(),
            reasoning: false,
            deadline: None,
            committed: false,
        }
    }
    async fn push(&mut self, mut text: &str, reasoning: bool) -> Result<(), ModelFailure> {
        if self.reasoning != reasoning {
            self.flush().await?;
        }
        self.reasoning = reasoning;
        while !text.is_empty() {
            let mut take = text.len().min(8192 - self.pending.len());
            while !text.is_char_boundary(take) {
                take -= 1;
            }
            if take == 0 {
                self.flush().await?;
                continue;
            }
            self.pending.push_str(&text[..take]);
            text = &text[take..];
            self.deadline
                .get_or_insert_with(|| Instant::now() + Duration::from_millis(16));
            if !self.committed || self.pending.len() >= 8192 || !text.is_empty() {
                self.flush().await?;
            }
        }
        Ok(())
    }
    pub async fn flush(&mut self) -> Result<(), ModelFailure> {
        self.deadline = None;
        if self.pending.is_empty() {
            return Ok(());
        }
        self.sink
            .send(Event::Text {
                response_id: self.response.clone(),
                text: std::mem::take(&mut self.pending),
                reasoning: self.reasoning,
            })
            .await
            .map_err(|_| ModelFailure::cancelled())?;
        self.committed = true;
        Ok(())
    }
}
