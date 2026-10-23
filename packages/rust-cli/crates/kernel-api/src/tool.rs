//! 工具接口（docs/specs/rust-cli/README.md §3、§6 规则 2）。
//!
//! 工具的形状：名称与别名、输入 schema、描述、只读 / 并发安全判定、输入校验、
//! 自身的权限检查、执行与结果预算。默认值：启用、不只读、不能并发、
//! 权限交给通用判决链。
//!
//! 工具只负责语义；读写文件、跑命令都经 [`ToolUseContext::env`]，不直接做 IO。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::env::{EnvError, Environment, OperationContext, WorkspaceRef};
use crate::ids::{AgentId, RunId, SessionId, ToolUseId};
use crate::mcp::McpToolInfo;
use crate::message::{Message, ToolResultContent};
use crate::orchestration::Interactions;
use crate::permission::{PermissionDecision, PermissionMode};
use crate::store::ArtifactStore;

/// 用户拒绝工具调用时的结果文字：执行管线（tools）与主循环取消排队的调用（query）
/// 共用（F55：以前两边各写一份）。
pub const REJECT_MESSAGE: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.";

/// 旧工具名 → 现在的名字：hook 的 matcher 与权限规则用同一张表（F58：以前 hooks 一份 8 项、policy 一份旧版本的
/// 4 项）。
pub const LEGACY_TOOL_NAMES: [(&str, &str); 8] = [
    ("Task", "Agent"),
    ("KillShell", "TaskStop"),
    ("KillBash", "TaskStop"),
    ("ListPeers", "ListAgents"),
    ("Brief", "SendUserMessage"),
    ("ListMcpResources", "ListMcpResourcesTool"),
    ("ReadMcpResource", "ReadMcpResourceTool"),
    ("ReadMcpResourceDir", "ReadMcpResourceDirTool"),
];

/// 旧名换成现在的名字，不是旧名时原样。
pub fn canonical_tool_name(name: &str) -> &str {
    LEGACY_TOOL_NAMES
        .iter()
        .find(|(old, _)| *old == name)
        .map_or(name, |(_, new)| new)
}

/// 渲染工具描述时可用的信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptContext {
    /// 本次请求里可见的工具名（有的描述会引用其他工具）。
    pub tool_names: Vec<String>,
    pub mode: PermissionMode,
}

/// 一次工具调用的上下文。由执行管线构造；`operation` 已经作为意图落库（§6 规则 3）。
#[derive(Clone)]
pub struct ToolUseContext {
    pub session: SessionId,
    pub run: RunId,
    pub tool_use: ToolUseId,
    /// 子代理里的调用带子代理 ID。
    pub agent: Option<AgentId>,
    pub workspace: WorkspaceRef,
    pub mode: PermissionMode,
    pub cancel: CancellationToken,
    pub env: Arc<dyn Environment>,
    pub artifacts: Arc<dyn ArtifactStore>,
    /// 需要用户回答时（权限、提问、计划审批）经这里；会话 actor 先持久化再通知前端。
    pub interactions: Arc<dyn Interactions>,
    pub operation: OperationContext,
}

/// 运行中提交新输入时，这个工具是被取消（Cancel）还是继续跑完（Block）。缺省 Block。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptBehavior {
    Cancel,
    Block,
}

/// 输入校验结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Validation {
    Ok,
    Invalid {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_code: Option<i32>,
    },
}

/// 执行中的进度（UI 用，不进模型上下文）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolProgress {
    pub data: Value,
}

/// 工具结果：给模型的内容，加上只给 UI 的展示数据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: Vec<ToolResultContent>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub display: Value,
}

/// [`Tool::call_with_messages`] 的结果：工具结果，加上排在 tool_result 之后提交的消息（例如
/// Skill 工具载入的 skill 正文，docs/specs/rust-cli/ext.md §2.4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallOutput {
    pub output: ToolOutput,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_messages: Vec<Message>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum ToolError {
    #[error("invalid input: {message}")]
    InvalidInput { message: String },
    #[error("tool call cancelled")]
    Cancelled,
    #[error(transparent)]
    Environment(#[from] EnvError),
    #[error("{message}")]
    Failed { message: String },
    #[error("not implemented: {what}")]
    Unsupported { what: String },
}

#[async_trait]
pub trait Tool: Send + Sync {
    /// 模型看到的名字。
    fn name(&self) -> &str;

    /// 旧名字；模型用旧名字调用时也能找到这个工具。
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// JSON Schema（Anthropic `input_schema`）。
    fn input_schema(&self) -> Value;

    /// 输入 schema 不严格（zod 的 `z.object`，不是 `z.strictObject`）：schema `properties` 以外的顶层键不报错，
    /// 执行管线在校验之前去掉（zod 的 strip）。两种写法生成的 JSON Schema 都是
    /// `additionalProperties: false`，分不出来，所以由工具声明。缺省为假：多数内置工具是严格的，
    /// 多出的键照常报错（修复线 F51，docs/specs/rust-cli/agents.md §2）。
    fn strips_unknown_keys(&self) -> bool {
        false
    }

    /// 工具描述；文本来自 `prompts/` 数据文件。
    fn description(&self, context: &PromptContext) -> String;

    fn is_enabled(&self) -> bool {
        true
    }

    /// 只在模型支持服务端 web search 时提供（生效配置的 `supportsNativeWebSearch`，修复线 F23，
    /// docs/specs/rust-cli/tools-more.md §5）：主循环按当前选择的能力去掉为真的工具。
    fn needs_native_web_search(&self) -> bool {
        false
    }

    /// 只按名字宣布、经 ToolSearch 按需加载。是否真的延迟由
    /// [`crate::tool_search::is_deferred`] 统一判定（docs/specs/rust-cli/tools-more.md §3.1）。
    fn should_defer(&self) -> bool {
        false
    }

    /// MCP 工具：除非 [`Tool::always_load`]，一律延迟。
    fn is_mcp(&self) -> bool {
        false
    }

    /// 总是内联给出完整 schema，不延迟（MCP 服务器的 `alwaysLoad` 或工具的 `_meta['zcode/alwaysLoad']`）。
    fn always_load(&self) -> bool {
        false
    }

    /// ToolSearch 关键词打分用的提示词。
    fn search_hint(&self) -> Option<&str> {
        None
    }

    /// 只读工具在 plan 模式与只读判定中放行。
    fn is_read_only(&self, _input: &Value) -> bool {
        false
    }

    /// 并发安全的调用可以和其他并发安全的调用同时执行（按输入判定）。
    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        false
    }

    fn interrupt_behavior(&self) -> InterruptBehavior {
        InterruptBehavior::Block
    }

    /// 结果超过这个字符数时落盘为 Artifact，只把预览交给模型。None 表示不落盘。
    fn max_result_size_chars(&self) -> Option<usize> {
        None
    }

    async fn validate_input(&self, _input: &Value, _context: &ToolUseContext) -> Validation {
        Validation::Ok
    }

    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Passthrough
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError>;

    /// 与 [`call`](Self::call) 相同，另外带上排在 tool_result 之后提交的消息。执行管线调用它；
    /// 缺省没有新消息。
    async fn call_with_messages(
        &self,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolCallOutput, ToolError> {
        Ok(ToolCallOutput {
            output: self.call(input, context, progress).await?,
            new_messages: Vec::new(),
        })
    }

    /// MCP 工具的来源；内置工具为 None。
    fn mcp_info(&self) -> Option<McpToolInfo> {
        None
    }
}

/// 会话的 readFileState（docs/specs/rust-cli/tools-files.md §4）的清空接口，由工具实现、runtime 组装。
///
/// 压缩时调用（先复制 readFileState、再清空，
/// 用复制品选压缩后要重读的文件）：清空这个会话记着的文件，返回清空前的路径，最近用过的在前。
pub trait ReadFileStates: Send + Sync {
    fn clear(&self, session: &SessionId) -> Vec<String>;
}
