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

use crate::controller_services::ControllerServices;
use crate::env::{EnvError, OperationContext, WorkspaceRef};
use crate::ids::{AgentId, RunId, SessionId, ToolUseId};
use crate::machine::MachineEnv;
use crate::mcp::McpToolInfo;
use crate::message::{Message, ToolResultContent};
use crate::orchestration::{Interactions, SessionOps};
use crate::permission::{PermissionDecision, PermissionMode};
use crate::store::ArtifactStore;

/// 用户拒绝工具调用时的结果文字：执行管线（tools）与主循环取消排队的调用（query）
/// 共用（F55：以前两边各写一份）。
pub const REJECT_MESSAGE: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.";

/// 工具结果落盘的通用上限（字符）：结果超过它就落盘为 Artifact，只把预览交给模型（[`Tool::max_result_size_chars`]）。
/// 多数工具用这个值；值不同的工具在自己那里命名（工作线 F122，用户决定 2026-10-30，docs/specs/rust-cli/constants.md §5.1）。
pub const DEFAULT_MAX_RESULT_CHARS: usize = 100_000;

/// 内核内部调用一个工具、不看它的进度时，进度通道的容量（满了的进度丢掉，不阻塞工具）：compact 的恢复与 skill 的
/// 内联调用共用（工作线 F122：以前各写一份 16）。
pub const DISCARDED_PROGRESS_CAPACITY: usize = 16;

/// 旧工具名 → 现在的名字：hook 的 matcher 与权限规则用同一张表（F58：以前 hooks 一份 8 项、policy 一份旧版本的
/// 4 项）。
pub const LEGACY_TOOL_NAMES: [(&str, &str); 8] = [
    ("Task", crate::tool_names::AGENT),
    ("KillShell", crate::tool_names::TASK_STOP),
    ("KillBash", crate::tool_names::TASK_STOP),
    ("ListPeers", crate::tool_names::LIST_AGENTS),
    ("Brief", crate::tool_names::SEND_USER_MESSAGE),
    ("ListMcpResources", crate::tool_names::LIST_MCP_RESOURCES),
    ("ReadMcpResource", crate::tool_names::READ_MCP_RESOURCE),
    (
        "ReadMcpResourceDir",
        crate::tool_names::READ_MCP_RESOURCE_DIR,
    ),
];

/// 旧名换成现在的名字，不是旧名时原样。
pub fn canonical_tool_name(name: &str) -> &str {
    LEGACY_TOOL_NAMES
        .iter()
        .find(|(old, _)| *old == name)
        .map_or(name, |(_, new)| new)
}

/// 这个工具认不认 `name`：是它的名字，或是按 [`LEGACY_TOOL_NAMES`] 换成它的名字的旧名。工具的查找与 hook 的
/// matcher、权限规则用同一张旧名表。
///
/// 缺陷修复（工作线 F132）：以前查找看每个工具自己列的旧名（`Tool::aliases`），表里有、工具上没列的旧名找不到工具：
/// ListAgents 没有列 ListPeers，模型按旧名调用时回 “No such tool available”。
pub fn answers_to(tool: &dyn Tool, name: &str) -> bool {
    tool.name() == canonical_tool_name(name)
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
    /// 这次调用在哪台机器上执行：缺省是会话的主 env，工具写了 `machine` 时换成那台额外机器（remote-exec.md §15.8.1）。
    /// Git 与 worktree 也在这台机器上。
    pub machine: MachineEnv,
    /// controller 所在机器的服务（WebFetch）。
    pub controller: ControllerServices,
    pub artifacts: Arc<dyn ArtifactStore>,
    /// 需要用户回答时（权限、提问、计划审批）经这里；会话 actor 登记后通知前端。
    pub interactions: Arc<dyn Interactions>,
    /// 会话的读与命令（当前的主模型、plan 开关、工作目录、Monitor 的 owner、fork 继承的对话，工作线 F134）。主循环里与
    /// `interactions` 是同一个输出端。
    pub session_ops: Arc<dyn SessionOps>,
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

// 写一个工具时的零件（工作线 F132 的 T7）：以前在 tools 的 `common.rs`，agents 与 ext 各自又写了几份。

/// 只有一段文字、不是错误、没有界面数据的结果（多数工具的成功结果）。
pub fn text(text: String) -> ToolOutput {
    ToolOutput {
        content: vec![ToolResultContent::Text { text }],
        is_error: false,
        display: Value::Null,
    }
}

/// 工具自己的失败：文字原样给模型（执行管线包成 `<tool_use_error>`）。
pub fn failed(message: String) -> ToolError {
    ToolError::Failed { message }
}

/// 输入不合法、没有错误码。
pub fn invalid(message: String) -> Validation {
    Validation::Invalid {
        message,
        error_code: None,
    }
}

/// 按工具的输入结构解析 `input`；不合法时是 [`ToolError::InvalidInput`]（serde 的错误文字）。
pub fn parse<T: serde::de::DeserializeOwned>(input: &Value) -> Result<T, ToolError> {
    serde_json::from_value(input.clone()).map_err(|error| ToolError::InvalidInput {
        message: error.to_string(),
    })
}

#[async_trait]
pub trait Tool: Send + Sync {
    /// 模型看到的名字。
    fn name(&self) -> &str;

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

    /// 组装 assistant 消息时整理这次调用的输入（docs/specs/rust-cli/query.md §3「工具输入的整理与写回」）：返回 Some 时
    /// 写回 tool_use 块，执行与回放给模型的都是它。缺省 None（不写回）；只有 Edit 覆盖（工作线 F131：以前主循环按名字
    /// 认 Edit）。
    fn rewrite_input(&self, _input: &Value) -> Option<Value> {
        None
    }

    fn is_enabled(&self) -> bool {
        true
    }

    /// 怎样用 `machine` 参数（docs/specs/rust-cli/remote-exec.md §20.5.1、§20.6）：缺省不带；在执行环境里干活的工具
    /// 与 Agent 自己说。发给模型的工具定义与执行管线的路由都按它（工作线 F137：以前按名字查表）。
    fn machine_param(&self) -> crate::machine_param::MachineParam {
        crate::machine_param::MachineParam::Absent
    }

    /// 只在模型支持服务端 web search 时提供（生效配置的 `supportsNativeWebSearch`，修复线 F23，
    /// docs/specs/rust-cli/tools-more.md §5）：主循环按当前选择的能力去掉为真的工具。
    fn needs_native_web_search(&self) -> bool {
        false
    }

    /// 只按名字宣布、经 ToolSearch 按需加载。是否真的延迟由
    /// [`crate::tool_search::is_deferred`] 统一判定（docs/specs/rust-cli/tools-more.md §3.1；MCP 工具看
    /// [`Tool::mcp_info`] 的 `always_load`，不看这个方法）。
    fn should_defer(&self) -> bool {
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

    /// 内置权限检查的判决主体（docs/specs/rust-cli/shell-permissions.md §4.1）：工具自己没有意见时，判决链按它检查这次
    /// 输入（Skill 放行时也按它先查 deny 规则）。None：只有整个工具的规则与 [`Tool::check_permissions`]。
    fn permission_subject(&self, _input: &Value) -> Option<crate::permission::PermissionSubject> {
        None
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

    /// MCP 工具的来源；内置工具为 None。“是不是 MCP 工具”“要不要延迟”都从它推出
    /// （[`crate::tool_search::is_mcp`]、[`crate::tool_search::is_deferred`]）。
    fn mcp_info(&self) -> Option<&McpToolInfo> {
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

/// 读过的文件在磁盘上被改动（docs/specs/rust-cli/tools-files.md §4.1）：每一步的当步 attachment 之前查一次。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    /// 执行环境里的路径（readFileState 的键）。
    pub path: String,
    /// 改动片段（带行号）；空表示只说改了，不给片段。
    pub snippet: String,
}

/// 查一个会话读过的文件有没有在磁盘上被改动：改了的重新读入 readFileState，返回改动片段。由工具实现
/// （readFileState 的 owner）、runtime 组装，context 在当步 attachment 里渲染。
#[async_trait]
pub trait ChangedFiles: Send + Sync {
    async fn changed_files(
        &self,
        session: &SessionId,
        workspace: &WorkspaceRef,
    ) -> Vec<ChangedFile>;
}
