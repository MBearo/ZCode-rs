use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRef {
    pub r#ref: String,
    pub file_name: String,
    pub mime: String,
    pub bytes: u64,
    pub preview_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RequestId {
    Text(String),
    Number(i64),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: Option<RequestId>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    pub trace: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
        let mut ack = json!({"commandId": self.command_id, "status": status, "revisionAtDecision": revision});
        if let Some(reason) = reason {
            ack["reasonCode"] = reason.into();
        }
        ack
    }
}

pub fn rpc_error(id: &Option<RequestId>, code: i32, message: &str) -> Value {
    json!({"id": id.clone().unwrap_or(RequestId::Text("invalid-message".into())), "error":{"code":code,"message":message}})
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelSelection {
    pub provider_id: String,
    pub model_id: String,
    pub options: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandAck {
    pub command_id: String,
    pub status: AckStatus,
    pub revision_at_decision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AckStatus {
    Accepted,
    Duplicate,
    Stale,
    Rejected,
    Noop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope {
    pub trace_id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub sequence: u64,
    pub kind: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub session_id: String,
    pub revision: u64,
    pub log_epoch: String,
    pub state: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_and_event_round_trip() {
        let command = Command {
            command_id: "cmd-1".into(),
            client_id: "client-1".into(),
            session_id: Some("session-1".into()),
            base_revision: Some(3),
            base_log_epoch: Some("epoch".into()),
            kind: "sendText".into(),
            payload: serde_json::json!({"text":"hello"}),
            issued_at: 1.0,
        };
        let encoded = serde_json::to_string(&command).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&encoded).unwrap(), command);

        let event = EventEnvelope {
            trace_id: "trace".into(),
            session_id: "session-1".into(),
            run_id: Some("run".into()),
            turn_id: Some("turn".into()),
            sequence: 4,
            kind: "assistantText".into(),
            payload: serde_json::json!({"text":"hello"}),
        };
        assert_eq!(serde_json::from_str::<EventEnvelope>(&serde_json::to_string(&event).unwrap()).unwrap(), event);
    }
}
