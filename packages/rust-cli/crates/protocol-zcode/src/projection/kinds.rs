//! 行的种类与工具调用的状态（`conversationRowSchema` 的 `kind` 与 toolCall 的 `status`）：线上是字符串，投影里用枚举，
//! 拼错或漏处理一种都是编译错误（F129，审查 E9）。行本身仍是 JSON（输出字节与 Node 对拍）。

use serde_json::{Map, Value};

/// 行的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowKind {
    TurnHeader,
    UserInput,
    AssistantText,
    Reasoning,
    ToolCall,
}

impl RowKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::TurnHeader => "turnHeader",
            Self::UserInput => "userInput",
            Self::AssistantText => "assistantText",
            Self::Reasoning => "reasoning",
            Self::ToolCall => "toolCall",
        }
    }

    /// 行上记的种类；行都由投影自己写，认不出只会是没有 `kind` 的行。
    pub(crate) fn of(row: &Map<String, Value>) -> Option<Self> {
        [
            Self::TurnHeader,
            Self::UserInput,
            Self::AssistantText,
            Self::Reasoning,
            Self::ToolCall,
        ]
        .into_iter()
        .find(|kind| row.get("kind").and_then(Value::as_str) == Some(kind.as_str()))
    }
}

/// toolCall 行的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolStatus {
    InputStreaming,
    PendingApproval,
    Running,
    Success,
    Error,
    Cancelled,
}

impl ToolStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::InputStreaming => "inputStreaming",
            Self::PendingApproval => "pendingApproval",
            Self::Running => "running",
            Self::Success => "success",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }

    /// 行上记的状态。
    pub(crate) fn of(row: &Map<String, Value>) -> Option<Self> {
        [
            Self::InputStreaming,
            Self::PendingApproval,
            Self::Running,
            Self::Success,
            Self::Error,
            Self::Cancelled,
        ]
        .into_iter()
        .find(|status| row.get("status").and_then(Value::as_str) == Some(status.as_str()))
    }

    /// 还没有结果：回合结束时投成 cancelled。
    pub(crate) fn is_open(self) -> bool {
        match self {
            Self::InputStreaming | Self::PendingApproval | Self::Running => true,
            Self::Success | Self::Error | Self::Cancelled => false,
        }
    }
}
