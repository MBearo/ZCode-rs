//! 会话消息模型（docs/specs/rust-cli/README.md §5）。
//!
//! 消息分 user / assistant / attachment / system 四类，内容块是 Anthropic
//! Messages 的块。这是内核的 canonical 形状；发给模型前由 query 的 normalize 转成 API 消息，
//! 落库时由会话存储适配器映射成自己的记录（本地是 Node 记录）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{MessageId, ToolUseId};
use crate::store::ArtifactRef;

/// Anthropic Messages 的内容块。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: ToolUseId,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: ToolUseId,
        content: Vec<ToolResultContent>,
        #[serde(default)]
        is_error: bool,
    },
    /// Anthropic 服务端工具的调用（WebSearch 的旁路请求里出现，docs/specs/rust-cli/tools-more.md §5）。
    /// ID 是服务端的 `srvtoolu_…`，不是客户端工具的 [`ToolUseId`]。
    ServerToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// 服务端 web_search 的结果：`content` 是结果数组，或 `{type: web_search_tool_result_error,
    /// error_code}`，原样保存，回放时原样发回。
    WebSearchToolResult {
        tool_use_id: String,
        content: Value,
    },
}

/// 图片来源。大图不内联在消息里，用 Artifact 引用，在发给模型的一侧物化（§6 规则 10）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 {
        media_type: String,
        data: String,
    },
    Artifact {
        media_type: String,
        artifact: ArtifactRef,
    },
}

/// tool_result 的内容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContent {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    /// PDF 文档（Read 整读 PDF 的结果，docs/specs/rust-cli/tools-files.md §1.5，修复线 F24）。`source` 与图片
    /// 同一种写法（base64 或 Artifact 引用），`media_type` 是 `application/pdf`；Anthropic 线格式是
    /// `{type: "document", source: {type: "base64", media_type, data}}`。
    Document {
        source: ImageSource,
    },
    /// ToolSearch 的结果：引用一个延迟工具，API 在服务端展开它的完整定义
    /// （beta `advanced-tool-use`，docs/specs/rust-cli/tools-more.md §3.6）。
    ToolReference {
        tool_name: String,
    },
}

/// `tool_reference` 在不支持它的地方（OpenAI 协议、Node 记录的文本）降成的文本。
pub fn tool_reference_text(tool_name: &str) -> String {
    format!("Tool loaded: {tool_name}")
}

/// 一条 user 消息从哪里来。决定 UI 是否显示、resume 时怎样重放。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserOrigin {
    /// 用户输入。
    Human,
    /// 工具结果，`assistant` 是发出 tool_use 的那条 assistant 消息。
    ToolResult { assistant: MessageId },
    /// 内核生成、只给模型看的消息（续写提示、hook 阻塞原因等）；`source` 是来源标签。
    Meta { source: String },
    /// 压缩摘要。
    CompactSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    pub id: MessageId,
    pub content: Vec<ContentBlock>,
    pub origin: UserOrigin,
}

/// 模型停止的原因（Anthropic `stop_reason`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
    #[serde(other)]
    Unknown,
}

/// token 用量（Anthropic `usage`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

/// 一次模型响应：一条消息、多个块（不按内容块拆成多行）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub id: MessageId,
    pub content: Vec<ContentBlock>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// attachment（system-reminder 类内容）：渲染好的文本加上结构化数据。
///
/// 落库形状是 `text` part + `synthetic` + `metadata.source`，`text` 自带 `<system-reminder>` 包装
/// （§5）；`data` 供 resume 后计算增量（例如 delta 类 attachment）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentMessage {
    pub id: MessageId,
    pub source: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

/// attachment 发给模型时的角色。
///
/// System：支持对话中间 system 消息的模型上，折叠进紧跟 user 消息的 `role: system` 消息；
/// User：留在 user 消息里，排在人类输入之前。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRole {
    User,
    System,
}

/// 以 user 角色渲染的 attachment 来源。来源名就是
/// attachment type；表外的来源都按 system 渲染。
const USER_ROLE_SOURCES: &[&str] = &[
    "dir_sync_notice",
    "unknown_command_fallback",
    "session_context",
    "instructions",
    "coordinator_context",
    "context_sections",
    "remote_session_change",
    "fork_briefing",
    "poll_events",
    "cowork_memory_context",
    "account_memory_recall",
    "artifact_opening_prefetch",
];

/// attachment 在 `text` 之后的内容块所在的 `data` 成员（docs/specs/rust-cli/tui-input.md §1.3）。
///
/// 某些 attachment 渲染出非文字内容：`@` 提及的图片、带图片输出的 notebook。这些块按顺序
/// 排在 `text` 之后发给模型；`data` 是它们在内核里的唯一位置（会话存储按自己的形状另存）。
pub const ATTACHMENT_BLOCKS: &str = "blocks";

impl AttachmentMessage {
    /// 角色由来源决定，不单独存储：Node 记录只保存 `metadata.source`，resume 后按同一张表还原。
    pub fn role(&self) -> AttachmentRole {
        if USER_ROLE_SOURCES.contains(&self.source.as_str()) {
            AttachmentRole::User
        } else {
            AttachmentRole::System
        }
    }

    /// `text` 之后的内容块（[`ATTACHMENT_BLOCKS`]）；没有或形状不对时为空。有内容块的 attachment 发给模型时
    /// 按 user 处理，不折进 system 消息（只折纯文字的 attachment）。
    pub fn blocks(&self) -> Vec<ContentBlock> {
        self.data
            .get(ATTACHMENT_BLOCKS)
            .and_then(|blocks| serde_json::from_value(blocks.clone()).ok())
            .unwrap_or_default()
    }
}

/// 压缩边界。`preserved` 是压缩后原样保留的消息段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactBoundary {
    pub id: MessageId,
    pub trigger: CompactTrigger,
    pub pre_tokens: u64,
    pub summary: MessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserved: Option<PreservedSegment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactTrigger {
    Manual,
    Auto,
    Reactive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreservedSegment {
    pub head: MessageId,
    pub anchor: MessageId,
    pub tail: MessageId,
}

/// system 消息：不发给模型（或由 normalize 特殊处理）的会话标记。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum SystemMessage {
    CompactBoundary(CompactBoundary),
    /// 用户中断了这一轮。
    Interrupted {
        id: MessageId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    Attachment(AttachmentMessage),
    System(SystemMessage),
}

impl Message {
    pub fn id(&self) -> &MessageId {
        match self {
            Self::User(message) => &message.id,
            Self::Assistant(message) => &message.id,
            Self::Attachment(message) => &message.id,
            Self::System(SystemMessage::CompactBoundary(boundary)) => &boundary.id,
            Self::System(SystemMessage::Interrupted { id }) => id,
        }
    }
}
