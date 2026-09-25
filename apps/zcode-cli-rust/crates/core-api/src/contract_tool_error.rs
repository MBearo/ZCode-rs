//! How a failed tool call reads to the model (Node `createErrorResult`).

/// A tool failure as the model reads it (Node `createErrorResult`). Other
/// errors a tool returns are thrown errors: their message, sanitized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// Node `ToolHandlerFailure`: `<tool_use_error>{message}</tool_use_error>`, message kept raw.
    Handler { code: u32, message: String },
    /// Model content used as given (e.g. an input validation envelope).
    Rendered(String),
    /// A client SDK failure (Node `SdkError`, e.g. MCP `CONNECTION_CLOSED`):
    /// the model reads the message; usage and telemetry record its code.
    Sdk { code: &'static str, message: String },
}
impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Handler { message, .. } | Self::Sdk { message, .. } => f.write_str(message),
            Self::Rendered(text) => f.write_str(text),
        }
    }
}
impl std::error::Error for ToolError {}
impl ToolError {
    pub fn handler(code: u32, message: impl Into<String>) -> anyhow::Error {
        Self::Handler {
            code,
            message: message.into(),
        }
        .into()
    }
}
/// Node's fallback when a thrown error has no readable message.
const FALLBACK_FAILURE: &str = "Turn execution failed";
/// The model-visible text of a failed tool call.
pub fn render_failure(error: &anyhow::Error) -> String {
    match error.downcast_ref::<ToolError>() {
        Some(ToolError::Handler { message, .. }) => {
            format!("<tool_use_error>{message}</tool_use_error>")
        }
        Some(ToolError::Rendered(text)) => text.clone(),
        Some(ToolError::Sdk { message, .. }) => message.clone(),
        None => zcode_cli_domain::js_string::sanitize_message(&error.to_string())
            .unwrap_or_else(|| FALLBACK_FAILURE.into()),
    }
}
