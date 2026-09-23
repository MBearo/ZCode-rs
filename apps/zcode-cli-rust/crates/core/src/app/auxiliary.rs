use super::Engine;
use crate::{
    contract::{Event, EventSink, ModelFailure, RunEvent},
    domain::protocol::{Request, RequestId, rpc_error},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
pub(super) struct Auxiliary {
    pub request: Option<RequestId>,
    pub cancel: CancellationToken,
    pub operation: Option<String>,
}
impl Engine {
    pub(super) fn start_auxiliary(&mut self, request: &Request) -> Result<()> {
        self.query("runtime/capabilities", &request.params)?; // 复用 workspace identity 验证。
        ensure!(self.auxiliary.len() < 16, "Too many workspace requests");
        let p = &request.params;
        let selected = self.select(&json!({"modelSelection":p["selection"]}), None)?;
        let mut model = if let Some(registry) = &self.registry {
            registry.resolve(&selected)?
        } else {
            self.model.clone().context("Model required")?
        };
        if let Some(max) = p["maxOutputTokens"].as_u64()
            && let Some(bound) = model.with_max_output_tokens(max as usize)?
        {
            model = bound;
        }
        let connectivity = request.method == "provider/testModelConnectivity";
        let mut messages = p["messages"].as_array().cloned().unwrap_or_default();
        if let Some(prompt) = p["prompt"].as_str() {
            messages.push(json!({"role":"user","content":prompt}));
        }
        if connectivity {
            messages = vec![json!({"role":"user","content":"Reply OK."})];
        }
        ensure!(!messages.is_empty(), "Prompt or messages required");
        for m in &mut messages {
            ensure!(
                matches!(
                    m["role"].as_str(),
                    Some("system" | "user" | "assistant" | "tool")
                ) && m["content"].is_string(),
                "Invalid workspace message"
            );
            if let Some(calls) = m["toolCalls"].as_array() {
                m["tool_calls"]=calls.iter().map(|c|json!({"id":c["id"],"type":"function","function":{"name":c["name"],"arguments":c["input"].to_string()}})).collect();
            }
            if m["role"] == "tool" {
                m["tool_call_id"] = m["toolCallId"].clone();
            }
            for key in ["toolCalls", "toolCallId", "toolName", "isError"] {
                m.as_object_mut().unwrap().remove(key);
            }
        }
        let tools=p["tools"].as_array().map(|tools|tools.iter().map(|t|json!({"type":"function","function":{"name":t["name"],"description":t["description"].as_str().unwrap_or(""),"parameters":t["inputSchema"]}})).collect::<Vec<_>>()).unwrap_or_default();
        let operation = p["operationId"].as_str().map(str::to_owned);
        ensure!(
            operation.is_none() || !self.auxiliary.values().any(|j| j.operation == operation),
            "Duplicate operation id"
        );
        let id = format!("workspace-query:{}", self.clock.id());
        let cancel = CancellationToken::new();
        self.auxiliary.insert(
            id.clone(),
            Auxiliary {
                request: request.id.clone(),
                cancel: cancel.clone(),
                operation,
            },
        );
        let sink = EventSink {
            session_id: id.clone(),
            run_id: id,
            tx: self.events.clone(),
        };
        tokio::spawn(async move {
            let result=model.complete(messages,&tools,&sink,&cancel).await.map(|out|{
                if connectivity {return json!({"success":true});}
                json!({"text":out.message["content"].as_str().unwrap_or(""),"selection":{"providerId":selected.provider_id,"modelId":selected.model_id,"options":{"reasoningLevel":selected.reasoning_level}},"toolCalls":out.calls.iter().map(|c|json!({"id":c["id"],"name":c["function"]["name"],"input":serde_json::from_str::<Value>(c["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(Value::Null)})).collect::<Vec<_>>(),"finishReason":if out.output_limit{"length"}else if out.calls.is_empty(){"stop"}else{"tool-calls"},"usage":{"inputTokens":out.usage["prompt_tokens"].as_u64().unwrap_or(0),"outputTokens":out.usage["completion_tokens"].as_u64().unwrap_or(0)}})
            });
            let _ = sink.send(Event::AuxiliaryDone { result }).await;
        });
        Ok(())
    }
    pub(super) fn auxiliary_event(&mut self, event: RunEvent) -> Result<()> {
        let id = event.session_id;
        if event.run_id != id {
            return Ok(());
        }
        match event.event {
            Event::ToolCleanupFailed(message) => anyhow::bail!("{message}"),
            Event::RequestAuth {
                provider,
                selection,
                access,
                reply,
            } if !self.auxiliary[&id].cancel.is_cancelled() && !reply.is_closed() => {
                let request_id = format!("rust-auth-{}", self.clock.id());
                let workspace = json!({"workspaceKey":self.workspace,"workspacePath":self.workspace_path,"workspaceIdentity":self.workspace});
                let params = json!({"requestId":request_id,"sessionId":id,"workspace":workspace,"providerId":provider,"modelSelection":selection,"accountAccess":access,"reason":"model-request"});
                self.auth
                    .insert(request_id.clone(), (id.clone(), id, workspace, reply));
                self.outbox.push(json!({"id":request_id,"method":"interaction/requestProviderRuntimeHeaders","params":params}));
            }
            Event::AuxiliaryDone { result } => {
                self.cancel_auth(&id);
                let job = self.auxiliary.remove(&id).unwrap();
                let result = if job.cancel.is_cancelled() {
                    Err(ModelFailure::cancelled())
                } else {
                    result
                };
                if job.request.is_some() {
                    self.outbox.push(match result {
                        Ok(value) => json!({"id":job.request,"result":value}),
                        Err(error) => rpc_error(&job.request, -32000, &error.to_string()),
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }
}
