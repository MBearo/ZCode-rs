//! 模型端口（docs/specs/rust-cli/README.md §3、§6）。
//!
//! canonical 请求是 Anthropic Messages 的形状。适配器
//! `model` 把它编码成 anthropic / openai-chat / openai-responses；OpenAI 协议没有的能力（显式缓存
//! 断点、thinking 签名、refusal 等）在适配器里降级，并在 spec §10 登记。

use std::collections::BTreeMap;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::message::{ContentBlock, StopReason, Usage};

/// 显式缓存断点（Anthropic `cache_control`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheControl {
    /// 目前只有 `ephemeral`。
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemBlock {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiRole {
    User,
    Assistant,
    /// 对话中间的 system 消息（Anthropic beta `mid-conversation-system`）。用它
    /// 承载环境段与系统提醒；不支持的协议由模型适配器降级。
    System,
}

/// 发给模型的一个内容块，可带缓存断点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiContent {
    #[serde(flatten)]
    pub block: ContentBlock,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// 每轮的输出设置（Anthropic beta `per-turn-control`）：挂在对话中间的 system 消息上，
/// 说明这一轮人类输入之后用的 effort。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnOutputConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// normalize 之后、发给模型的一条消息（同角色已合并，attachment 已转成 user 内容）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiMessage {
    pub role: ApiRole,
    pub content: Vec<ApiContent>,
    /// 只出现在 `role: system` 的消息上；不支持的协议由模型适配器忽略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_config: Option<TurnOutputConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Any,
    Tool { name: String },
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThinkingConfig {
    Enabled {
        budget_tokens: u32,
    },
    /// 模型自行决定思考长度；`display` 控制流式展示（例如 `updates`）。
    Adaptive {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<String>,
    },
    Disabled,
}

/// 服务端上下文编辑（Anthropic `context_management.edits`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContextEdit {
    /// 清理旧的 thinking 块；`keep` 为 `all` 时保留全部。
    #[serde(rename = "clear_thinking_20251015")]
    ClearThinking { keep: String },
}

/// 一个工具在请求里的附加标记（Anthropic 工具定义的可选字段 `defer_loading`、`strict`、
/// `eager_input_streaming`）。
///
/// `ToolDefinition` 在 query 里用结构体字面量构造，给它加字段会让 query 编译失败；这些标记因此按工具名
/// 放在 [`RequestOptions::tool_flags`] 里，全部默认关闭。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolFlags {
    /// 工具只按名字列出、由 ToolSearch 按需加载（beta `advanced-tool-use`）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub defer_loading: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub eager_input_streaming: bool,
}

/// 与协议相关的可选项。全部有默认值：新增字段不影响已有的构造代码。
/// 非 Anthropic 协议不支持的项由模型适配器忽略或降级，并在 spec §10 登记。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestOptions {
    /// `output_config.effort`（low / medium / high）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_edits: Vec<ContextEdit>,
    /// `metadata.user_id`（含 device_id / account_uuid / session_id 的 JSON 文本）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// `diagnostics.previous_message_id`：上一次响应的 message id；会话第一次请求为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_message_id: Option<String>,
    /// 连续过载之后改用的模型。连续 3 次过载时，模型端口返回
    /// [`ModelError::FallbackTriggered`]，由主循环换成这个模型重发整个请求。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_model: Option<String>,
    /// 按工具名给出的附加标记；没有列出的工具不带任何标记。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_flags: BTreeMap<String, ToolFlags>,
    /// Anthropic 服务端工具，排在 `tools` 之后（WebSearch，docs/specs/rust-cli/tools-more.md §5）。
    /// 非 Anthropic 协议不支持，请求时报 [`ModelError::Unsupported`]。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub server_tools: Vec<ServerTool>,
    /// `output_config.format`（structured output，docs/specs/rust-cli/titles.md §8.4）。
    /// 非 Anthropic 协议忽略（model.md §10）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormat>,
    /// 发这次请求的会话（工作线 F52，docs/specs/rust-cli/model.md §3.3）：`user_id` 没有给时，模型适配器用它与设备身份
    /// 算出 `metadata.user_id`（只对 Anthropic 协议，工作线 F52b）。子代理是子会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<crate::ids::SessionId>,
    /// 发这次请求的回合（主循环的一次运行，工作线 F64，docs/specs/rust-cli/model.md §2.4）：模型适配器据此写
    /// `x-query-id`（Node 的 query id）。没有回合的请求（`/rename` 起名）为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<crate::ids::RunId>,
    /// 发这次请求的子代理（工作线 F64）：有值时 `x-zcode-session-type` 是 `subagent`（Node 按子 runtime 的任务类型）。
    /// 子代理的主循环、压缩，以及子代理里的工具与 hook 发的旁路请求都带。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<crate::ids::AgentId>,
    /// 交给 Provider 补丁的输出上限（工作线 F95，docs/specs/rust-cli/model.md §9）：目录模型的 `maxOutputTokens` map 以
    /// `min(配置的上限, 这个值)` 为输入（Node 的 option map 以请求自己的 `maxOutputTokens` 为输入）。None 时补丁的输入是
    /// 配置的上限。只有请求自己的上限就是 Node 的值的请求才给（现在是 WebSearch 的旁路请求与自动标题，tools-more.md §5、
    /// titles.md §3）；没有补丁的端点（显式覆盖）不看它。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_max_output_tokens: Option<u32>,
}

/// 模型输出的格式约束（Anthropic `output_config.format`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputFormat {
    /// `{type: "json_schema", schema}`。
    JsonSchema { schema: Value },
}

/// Anthropic 服务端工具。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerTool {
    /// `{type: "web_search_20250305", name: "web_search", allowed_domains?, blocked_domains?, max_uses}`。
    WebSearch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_domains: Option<Vec<String>>,
        max_uses: u32,
    },
}

/// 这次请求是谁发的：影响缓存、请求头与旁路请求的识别。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum QuerySource {
    MainThread,
    Subagent(String),
    Compact,
    /// 标题生成、WebFetch 二次处理、记忆召回等旁路请求。
    Side(String),
}

/// 会话标题的旁路请求名（docs/specs/rust-cli/titles.md §3）。
pub const SIDE_SESSION_TITLE: &str = "generate_session_title";
/// WebFetch 摘要的旁路请求名（docs/specs/rust-cli/tools-more.md 的 WebFetch）。
pub const SIDE_WEB_FETCH: &str = "web_fetch_apply";

impl QuerySource {
    /// 写给 Node 的用途名（工作线 F71）：用量表的 `query_source`（usage.md §2.1）与请求头 `x-zcode-query-source`
    /// （model.md §2.4）共用。Node 有对应用途的用 Node 的名字（`main_turn`、`subagent`、`compact`、`session_title`、
    /// `web_fetch_processing`），其余旁路请求用本工程的名字。
    pub fn node_name(&self) -> String {
        match self {
            Self::MainThread => "main_turn".to_owned(),
            Self::Subagent(_) => "subagent".to_owned(),
            Self::Compact => "compact".to_owned(),
            Self::Side(name) => match name.as_str() {
                SIDE_SESSION_TITLE => "session_title".to_owned(),
                SIDE_WEB_FETCH => "web_fetch_processing".to_owned(),
                _ => name.clone(),
            },
        }
    }
}

/// 选中的模型（Provider 由平台层解析成具体端点与凭据）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// 账号连接（工作线 F49，docs/specs/rust-cli/platform.md §9.4）：同一个 provider ID 下有多个套餐连接
    /// （Team Plan 的每个团队项目）时选的是哪一个。由目录项给出、原样带回，平台层按它取凭据；内核只传递。
    /// 装箱：选择随用量事实（`metering::Metered`）到处复制，不让它变大。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<Box<ModelConnection>>,
}

/// 账号 provider 下的一个连接：Team Plan 的团队项目（App `ProviderFamilyConnectionSelection` 的团队变体去掉
/// `kind`，三个 ID 都是去首尾空白后的非空值）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConnection {
    pub product_id: String,
    pub organization_id: String,
    pub project_id: String,
}

/// 小模型槽位（docs/specs/rust-cli/model.md §6.1）：给定会话
/// **当前**的主模型，返回旁路请求（会话标题、WebFetch 摘要、prompt hook 的缺省模型）用的模型。这是唯一
/// 入口；现在没有用户入口（没有设置或环境变量能改），等于传入的主模型。当前选择的 owner 是
/// 会话 actor，调用方按请求时的值传入，不另存副本。
pub fn small_fast_model(main: &ModelSelection) -> ModelSelection {
    main.clone()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub selection: ModelSelection,
    pub system: Vec<SystemBlock>,
    pub messages: Vec<ApiMessage>,
    pub tools: Vec<ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Anthropic beta 名称；非 Anthropic 协议忽略。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub betas: Vec<String>,
    pub source: QuerySource,
    #[serde(default)]
    pub options: RequestOptions,
}

/// 流式增量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum BlockDelta {
    Text(String),
    Thinking(String),
    Signature(String),
    InputJson(String),
}

/// 模型流事件（与 Anthropic SSE 事件一一对应；其他协议由适配器转成这个形状）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelEvent {
    MessageStart {
        id: String,
        model: String,
        usage: Usage,
    },
    BlockStart {
        index: usize,
        block: ContentBlock,
    },
    BlockDelta {
        index: usize,
        delta: BlockDelta,
    },
    BlockStop {
        index: usize,
    },
    MessageDelta {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<StopReason>,
        usage: Usage,
    },
    MessageStop,
}

/// 模型请求失败。主循环按类型决定重试、压缩或回退，不解析错误文本。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelError {
    #[error("model request cancelled")]
    Cancelled,
    #[error("rate limited: {message}")]
    RateLimited {
        message: String,
        retry_after_ms: Option<u64>,
    },
    #[error("model overloaded: {message}")]
    Overloaded { message: String },
    #[error("prompt too long: {message}")]
    PromptTooLong { message: String },
    #[error("authentication failed: {message}")]
    Authentication { message: String },
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },
    #[error("network error: {message}")]
    Network { message: String },
    #[error("server error {status}: {message}")]
    Server { status: u16, message: String },
    #[error("protocol error: {message}")]
    Protocol { message: String },
    /// 连续过载达到上限，且请求给了 [`RequestOptions::fallback_model`]。主循环换成 `fallback_model` 重发，
    /// 并提示用户已切换。
    #[error("model fallback triggered: {original_model} -> {fallback_model}")]
    FallbackTriggered {
        original_model: String,
        fallback_model: String,
    },
    #[error("not implemented: {what}")]
    Unsupported { what: String },
    /// 流开始之后，超过空闲上限没有收到数据，适配器中止了这次流（流空闲看门狗，
    /// docs/specs/rust-cli/stall-visibility.md §2）。`idle_ms` 是触发的那个上限。文字固定。
    #[error("Stream idle timeout - no chunks received")]
    StreamIdle { idle_ms: u64 },
    /// 余额、配额或套餐额度用尽，等也不会好：不重试（docs/specs/rust-cli/provider-errors.md §2.3、D4，修复线 F113）。
    /// 文字就是给用户的说明（§6）。
    #[error("API Error: Credit balance or quota exhausted · {message}")]
    QuotaExhausted { message: String },
    /// 端点的内容安全拦截（输入侧或输出侧的错误）：不重试，回合以错误结束并显示原因（provider-errors.md §2.3，已定）。
    #[error(
        "API Error: Model output was blocked by the provider's content safety filter. · {message}"
    )]
    ContentBlocked { message: String },
    /// 请求体或媒体太大（413、图片过大）：不重试，提示用户去掉图片或回退（provider-errors.md §2.3、D9）。
    #[error(
        "API Error: Request too large. Remove large images or files from the conversation, or use /rewind to go back to before they were added. · {message}"
    )]
    RequestTooLarge { message: String },
}

/// 内容拦截的说明（[`ModelError::ContentBlocked`] 的前一句；结束原因是 refusal 时主循环用它结束回合，
/// docs/specs/rust-cli/provider-errors.md §6，修复线 F113）。
pub const CONTENT_BLOCKED: &str =
    "API Error: Model output was blocked by the provider's content safety filter.";

/// 模型请求过程中的通知：不是响应内容，主循环可以展示或忽略。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelNotice {
    /// 第 `attempt` 次请求失败，等待 `delay_ms` 后重发（重试等待时产出 API 错误提示，
    /// stream-json 里是 `system/api_retry`）。
    Retrying {
        attempt: u32,
        max_retries: u32,
        delay_ms: u64,
        error: ModelError,
    },
    /// 流开始之后一段时间没有收到字节（`stalled` 重试状态）：再过 `retry_in_ms` 看门狗中止这次流
    /// （stall-visibility.md §2.4）。
    Stalled { retry_in_ms: u64 },
    /// 前面的重试或停顿通知不再成立：重发的请求拿到了响应头，或停顿之后又收到了事件（清掉重试状态）。
    Resumed,
}

/// [`ModelPort::stream_with_notices`] 的元素。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ModelStreamItem {
    Event(ModelEvent),
    Notice(ModelNotice),
}

/// 模型调用。错误也作为流的元素返回，调用方只处理一个流。
///
/// 删除（工作线 F93，用户决定 2026-10-24，“只加不改”的破例，README §11.1）：`count_tokens` 与 `CountTokensRequest`。
/// `/context` 只用本地估算（docs/specs/rust-cli/context-usage.md §3、D13）之后没有调用方。
pub trait ModelPort: Send + Sync {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>>;

    /// 与 [`stream`](Self::stream) 相同，另外带上重试等通知。默认实现没有通知；需要通知的调用方
    /// 改用这个方法，已有调用方不受影响。
    fn stream_with_notices(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelStreamItem, ModelError>> {
        self.stream(request, cancel)
            .map(|item| item.map(ModelStreamItem::Event))
            .boxed()
    }

    /// 这个选择的模型能力（Node 的生效模型配置，docs/specs/rust-cli/model.md §11，修复线 F23）。默认答不出
    /// （测试替身）：调用方按模型卡片回退。
    fn capabilities(
        &self,
        _selection: &ModelSelection,
    ) -> futures_util::future::BoxFuture<'static, Option<crate::provider::ModelCapabilities>> {
        Box::pin(async { None })
    }
}
