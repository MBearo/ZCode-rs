use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRef {
    pub r#ref: String,
    pub file_name: String,
    pub mime: String,
    pub bytes: u64,
    pub preview_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Text(String),
    Number(i64),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: Option<RequestId>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    pub trace: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub command_id: String,
    pub client_id: String,
    pub session_id: Option<String>,
    pub base_revision: Option<u64>,
    pub base_log_epoch: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub payload: Value,
    pub issued_at: f64,
}

impl Command {
    pub fn key(&self) -> String {
        serde_json::to_string(&(self.session_id.as_deref(), &self.command_id)).unwrap()
    }
    pub fn ack(&self, status: &str, revision: u64, reason: Option<&str>) -> Value {
        let mut ack =
            json!({"commandId":self.command_id,"status":status,"revisionAtDecision":revision});
        if let Some(reason) = reason {
            ack["reasonCode"] = reason.into();
        }
        ack
    }
}

pub fn rpc_error(id: &Option<RequestId>, code: i32, message: &str) -> Value {
    json!({"id": id.clone().unwrap_or(RequestId::Text("invalid-message".into())),
        "error":{"code":code,"message":message}})
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelSelection {
    pub provider_id: String,
    pub model_id: String,
    pub options: Option<Value>,
}
