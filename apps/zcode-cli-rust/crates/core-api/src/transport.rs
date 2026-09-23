//! Frontend ↔ runtime transport contract.
//!
//! The runtime actor receives [`ClientMsg`] and emits [`ServerMsg`] on one
//! ordered channel per connection, so a reply is always delivered before the
//! events produced while handling that request. Frontends (App Server, headless,
//! TUI) own wire encoding, subscriptions and delivery; they never share session
//! state with the runtime.
use serde_json::{Value, json};

macro_rules! methods {
    ($($variant:ident => $name:literal),* $(,)?) => {
        /// Runtime request kinds. Wire names parse through [`Method::parse`];
        /// delivery-internal requests have no wire name.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Method {
            $($variant,)*
            /// Open a topic for delivery: load the session if needed, pin it and return a snapshot.
            TopicOpen,
            /// Return the current snapshot of an already opened topic.
            TopicSnapshot,
        }
        impl Method {
            pub fn parse(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)*
                    _ => None,
                }
            }
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                    Self::TopicOpen => "internal/topicOpen",
                    Self::TopicSnapshot => "internal/topicSnapshot",
                }
            }
        }
    };
}

methods! {
    Command => "v4/command",
    CommandsQuery => "v4/commands/query",
    AttachmentBegin => "v4/attachment/begin",
    AttachmentChunk => "v4/attachment/chunk",
    AttachmentCommit => "v4/attachment/commit",
    AttachmentAbort => "v4/attachment/abort",
    AttachmentRead => "v4/attachment/read",
    AttachmentPreviewSource => "v4/attachment/previewSource",
    ConversationAttachmentRead => "v4/conversation/attachmentRead",
    ConversationAttachmentStat => "v4/conversation/attachmentStat",
    ConversationRowsRange => "v4/conversation/rowsRange",
    ConversationPlans => "v4/conversation/plans",
    ConversationFileChanges => "v4/conversation/fileChanges",
    ConversationFileRewindPreview => "v4/conversation/fileRewindPreview",
    McpList => "mcp/list",
    SkillsReferenceCatalog => "skills/referenceCatalog",
    SessionCreate => "session/create",
    SessionResume => "session/resume",
    SessionRead => "session/read",
    SessionList => "session/list",
    SessionSubagents => "session/subagents",
    SessionClose => "session/close",
    RuntimeCapabilities => "runtime/capabilities",
    ProcessChildProcesses => "process/childProcesses",
    WorkspaceReadPresentation => "workspace/readPresentation",
    WorkspaceGenerateText => "workspace/generateText",
    WorkspaceCancelGenerateText => "workspace/cancelGenerateText",
    WorkspaceUpdateInteractionPreferences => "workspace/updateInteractionPreferences",
    ProviderTestModelConnectivity => "provider/testModelConnectivity",
    ProviderUpdateAccountConfig => "provider/updateAccountConfig",
}

/// Frontend → runtime.
#[derive(Debug)]
pub enum ClientMsg {
    /// `token` is frontend-assigned and echoed in the matching [`ServerMsg::Reply`].
    Request {
        token: u64,
        method: Method,
        params: Value,
    },
    /// Reply to a [`ServerMsg::HostRequest`].
    HostReply { id: String, result: Value },
    /// A delivery subscription on `topic` ended; releases the pin taken by [`Method::TopicOpen`].
    TopicReleased { topic: String },
    /// A client connection closed; runtime releases connection-scoped staging (uploads).
    ConnectionClosed { connection: String },
    /// Input ended; the runtime drains and stops.
    Eof,
}

/// Runtime → frontend. All variants travel on one ordered channel.
#[derive(Debug)]
pub enum ServerMsg {
    Reply {
        token: u64,
        result: Result<Value, RuntimeError>,
    },
    Event(RuntimeEvent),
    /// A request the runtime needs the host to answer (e.g. request-scoped credentials).
    HostRequest {
        id: String,
        method: &'static str,
        params: Value,
    },
    HostNotification {
        method: &'static str,
        params: Value,
    },
}

/// Typed facts for delivery. Frontends decide framing, fan-out and flow control.
#[derive(Debug)]
pub enum RuntimeEvent {
    /// Conversation deltas covering `(from, to]`; emitted only while the topic is open.
    ConversationDeltas {
        session: String,
        from: u64,
        to: u64,
        deltas: Vec<Value>,
    },
    /// History was rewritten; subscribers must replace their state with `snapshot`.
    ConversationReset {
        session: String,
        seq: u64,
        snapshot: Value,
    },
    /// The topic no longer exists (e.g. session deleted); drop its subscriptions.
    TopicClosed { topic: String },
    /// One sessions-index change covering `(from, to]`.
    IndexChanged {
        workspace: String,
        from: u64,
        to: u64,
        delta: Value,
    },
    /// Workspace configuration changed; `snapshot` replaces the previous one.
    ConfigChanged {
        workspace: String,
        seq: u64,
        snapshot: Value,
    },
}

/// Errors a runtime reports to frontends. Business code returns these through
/// `anyhow` and never builds JSON-RPC error objects itself.
#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeError {
    MethodNotFound(String),
    /// Request or parameter shape is invalid (JSON-RPC -32602).
    InvalidParams(String),
    /// Any other failure, including V4 `fault.*`/`proto.*` reason codes (JSON-RPC -32603).
    Fault {
        message: String,
        code: Option<String>,
    },
    /// A protocol-defined code that must be preserved verbatim.
    Coded {
        code: i64,
        message: String,
    },
    /// Node `parseParams` failure: full message and the serialized ZodError (-32602).
    Params {
        message: String,
        data: Value,
    },
    /// A typed Node error (`data.name`, e.g. `ModelProtocolError`) with its `data.code` (-32603).
    Named {
        name: &'static str,
        message: String,
        code: Option<String>,
    },
}

impl RuntimeError {
    pub fn invalid_params(detail: impl Into<String>) -> anyhow::Error {
        Self::InvalidParams(detail.into()).into()
    }

    /// Classify an error raised by business code; unclassified errors are faults.
    pub fn classify(error: &anyhow::Error) -> Self {
        error
            .downcast_ref::<RuntimeError>()
            .cloned()
            .unwrap_or_else(|| Self::Fault {
                message: error.to_string(),
                code: None,
            })
    }

    pub fn code(&self) -> i64 {
        match self {
            Self::MethodNotFound(_) => -32601,
            Self::InvalidParams(_) | Self::Params { .. } => -32602,
            Self::Fault { .. } | Self::Named { .. } => -32603,
            Self::Coded { code, .. } => *code,
        }
    }

    /// JSON-RPC `error` object, matching the Node runtime's `toProtocolError`.
    pub fn to_json(&self) -> Value {
        match self {
            Self::MethodNotFound(method) => {
                json!({"code":self.code(),"message":format!("Method not found: {method}")})
            }
            Self::InvalidParams(detail) => {
                json!({"code":self.code(),"message":format!("Invalid params — {detail}")})
            }
            Self::Fault { message, code } => {
                let mut data = json!({"name":"Error"});
                if let Some(code) = code {
                    data["code"] = code.clone().into();
                }
                json!({"code":self.code(),"message":message,"data":data})
            }
            Self::Coded { code, message } => json!({"code":code,"message":message}),
            Self::Params { message, data } => {
                json!({"code":self.code(),"message":message,"data":data})
            }
            Self::Named {
                name,
                message,
                code,
            } => {
                let mut data = json!({"name":name});
                if let Some(code) = code {
                    data["code"] = code.clone().into();
                }
                json!({"code":self.code(),"message":message,"data":data})
            }
        }
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MethodNotFound(method) => write!(f, "Method not found: {method}"),
            Self::InvalidParams(detail) => write!(f, "Invalid params — {detail}"),
            Self::Fault { message, .. }
            | Self::Coded { message, .. }
            | Self::Params { message, .. }
            | Self::Named { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for RuntimeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_methods_round_trip_and_internal_methods_do_not_parse() {
        for method in [Method::Command, Method::SessionList, Method::McpList] {
            assert_eq!(Method::parse(method.as_str()), Some(method));
        }
        assert_eq!(Method::parse(Method::TopicOpen.as_str()), None);
        assert_eq!(Method::parse("v4/conversation/subscribe"), None);
    }

    #[test]
    fn errors_map_to_node_json_rpc_codes() {
        let unknown = RuntimeError::MethodNotFound("x/y".into()).to_json();
        assert_eq!(
            unknown,
            json!({"code":-32601,"message":"Method not found: x/y"})
        );
        let invalid = RuntimeError::classify(&RuntimeError::invalid_params("topic: required"));
        assert_eq!(invalid.code(), -32602);
        let fault = RuntimeError::classify(&anyhow::anyhow!("fault.subscription.notOwned"));
        assert_eq!(
            fault.to_json(),
            json!({"code":-32603,"message":"fault.subscription.notOwned","data":{"name":"Error"}})
        );
    }
}
