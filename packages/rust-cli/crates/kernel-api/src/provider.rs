//! 模型目录与端点（docs/specs/rust-cli/platform.md §4）。
//!
//! 平台层（platform）解析 Provider 配置与账号，对内核与模型适配器只给出三样东西：
//!
//! - 可用模型目录（[`ModelCatalog`]，带版本的快照）；
//! - 默认模型选择（[`ModelCatalog::default`]）；
//! - 按 [`ModelSelection`] 解析出的端点与凭据（[`ModelEndpoint`]）。模型适配器每次请求取一次，
//!   官方账号模型因此每次请求都拿当前账号的凭据（每请求鉴权）。
//!
//! 端点带秘密值，只在进程内从平台层交给模型适配器，不落库、不跨执行环境，所以不实现 serde；
//! 目录是普通 DTO，可以序列化。

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{ModelConnection, ModelError, ModelSelection};

/// Provider 的线协议（Provider 配置的 `api.type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WireProtocol {
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "openai-chat-completions")]
    OpenAiChatCompletions,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
}

impl WireProtocol {
    /// Provider 配置里的写法；不认识的类型返回 `None`。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "anthropic-messages" => Some(Self::AnthropicMessages),
            "openai-chat-completions" => Some(Self::OpenAiChatCompletions),
            "openai-responses" => Some(Self::OpenAiResponses),
            _ => None,
        }
    }
}

/// 请求的鉴权。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// 不发鉴权 header（本地网关等）。
    None,
    /// API key 或账号派生的令牌：Anthropic 发 `x-api-key`，OpenAI 两种协议发 `Authorization: Bearer`。
    ApiKey(RedactedString),
}

/// 一次请求的端点。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEndpoint {
    pub protocol: WireProtocol,
    /// Provider 配置的 `api.baseUrl`，原样给出；各协议在适配器里拼路径。
    pub base_url: String,
    /// 请求体里的 `model`（Provider 侧的模型 ID）。
    pub model: String,
    pub credential: Credential,
    /// Provider 配置的 `api.headers`。值可能含秘密，一律按秘密处理；同名时覆盖适配器自己的 header。
    pub headers: Vec<(String, RedactedString)>,
    /// Provider 的 option map 算出的 JSON merge patch（RFC 7396），按顺序合并进请求体
    /// （例如 thinking、`output_config.effort`、`max_tokens`）。
    pub body_patches: Vec<Value>,
    /// 模型接受对话中间的 `role: system` 消息（Provider 配置的 `supportsMidConversationSystem`，只对
    /// Anthropic 协议有意义）。为假时适配器把这些消息降级成 user 消息里的 `<system-reminder>`。
    pub mid_conversation_system: bool,
    /// 模型接受图片输入（Provider 配置的 `inputFormat.supportsImage`）。为假时适配器把请求里的图片换成占位文字
    /// （docs/specs/rust-cli/model.md §10.5）。显式覆盖的 Anthropic 端点为真。
    pub supports_image: bool,
    /// 模型接受 PDF / 视频 / 音频输入（`inputFormat.supportsPdf` / `supportsVideo` / `supportsAudio`，修复线 F23）。
    /// 与 `supports_image` 一样由平台层从生效配置写入（docs/specs/rust-cli/model.md §11）。
    pub supports_pdf: bool,
    pub supports_video: bool,
    pub supports_audio: bool,
    /// 凭据是每次请求现取的（官方账号模型）：服务端回 401 时，适配器带上这个端点再取一次（见
    /// [`ModelEndpoints::endpoint`] 的 `rejected`），换到新凭据后重发。
    pub renewable: bool,
}

/// 一次取端点的上下文（缺口 G8，docs/specs/rust-cli/app-protocol.md §5.2）：发请求的会话。前端提供的请求头来源
/// （例如经宿主取账号凭据）按它找到会话与工作区；平台层自己算凭据时不用它。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointContext {
    /// 发这次请求的会话（[`crate::model::RequestOptions::session_id`]；子代理是子会话）。
    pub session: Option<crate::ids::SessionId>,
}

/// 按选择解析端点。实现方是平台层；测试与显式覆盖（`ZCODE_MODEL_BASE_URL` / `ZCODE_API_KEY`）
/// 用固定端点。
#[async_trait]
pub trait ModelEndpoints: Send + Sync {
    /// 这次请求的端点与凭据。`rejected` 是上一次取到、刚被服务端拒绝（401）的端点：实现方不能再给出
    /// 同一份缓存的凭据。选择不可用时返回 [`ModelError::Authentication`]（没有凭据）或
    /// [`ModelError::InvalidRequest`]（目录里没有这个模型）。
    async fn endpoint(
        &self,
        selection: &ModelSelection,
        rejected: Option<&ModelEndpoint>,
    ) -> Result<ModelEndpoint, ModelError>;

    /// 同 [`Self::endpoint`]，另带这次请求交给 Provider 补丁的输出上限（工作线 F95，docs/specs/rust-cli/model.md §9、
    /// platform.md §4）：有值时 `maxOutputTokens` map 的输入再与它取小。缺省实现忽略它、调 `endpoint`（固定端点与测试
    /// 替身没有补丁）。
    async fn endpoint_with_output_limit(
        &self,
        selection: &ModelSelection,
        rejected: Option<&ModelEndpoint>,
        output_limit: Option<u64>,
    ) -> Result<ModelEndpoint, ModelError> {
        let _ = output_limit;
        self.endpoint(selection, rejected).await
    }

    /// 同 [`Self::endpoint_with_output_limit`]，另带这次请求的上下文（缺口 G8）。模型适配器取端点只经它；缺省实现
    /// 忽略上下文，行为与以前相同。
    async fn endpoint_in(
        &self,
        selection: &ModelSelection,
        rejected: Option<&ModelEndpoint>,
        output_limit: Option<u64>,
        context: &EndpointContext,
    ) -> Result<ModelEndpoint, ModelError> {
        let _ = context;
        self.endpoint_with_output_limit(selection, rejected, output_limit)
            .await
    }

    /// 这个选择的模型能力（Node 的生效模型配置，docs/specs/rust-cli/model.md §11，修复线 F23）。不取凭据；
    /// 答不出（没有这个模型、测试替身）时为 None。
    async fn capabilities(&self, _selection: &ModelSelection) -> Option<ModelCapabilities> {
        None
    }
}

/// 模型能力：Node 生效模型配置的 `properties`（docs/specs/rust-cli/model.md §11、platform.md §3.3，修复线 F23）。
/// 平台层按 Node 的规则算出，目录项与端点从同一份写。缺省值是内置配置第一条规则（`.*`）的值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCapabilities {
    /// `contextWindow`。
    pub context_window: u64,
    /// `supportsToolCall`。
    pub supports_tool_call: bool,
    /// `supportsNativeWebSearch`：WebSearch 是否发给模型（tools-more.md §5）。
    pub supports_native_web_search: bool,
    /// `supportsMidConversationSystem`：对话中间的 `role: system` 消息。
    pub supports_mid_conversation_system: bool,
    /// `supportsJsonSchemaOutput`。
    pub supports_json_schema_output: bool,
    /// `requiresMfjsToolSchema`。
    pub requires_mfjs_tool_schema: bool,
    /// `inputFormat.supportsImage` / `supportsPdf` / `supportsVideo` / `supportsAudio`。
    pub supports_image: bool,
    pub supports_pdf: bool,
    pub supports_video: bool,
    pub supports_audio: bool,
    /// 端点支持延迟加载工具（`defer_loading` 与 `tool_reference`），ToolSearch 因此用原生形式；为假时是本地形式（工作线
    /// F84，docs/specs/rust-cli/tools-more.md §3.2.2；F52 时为假就不开）。工作线 F89（用户决定：暂时不用原生）起平台层与
    /// 显式覆盖的能力规则总给假，只有 debug 构建读的测试声明把显式覆盖的端点设为真；F52 到 F84 是 z.ai / bigmodel.cn 域名下
    /// 的 Anthropic 端点为真。不是 Node 配置里的字段。
    pub supports_tool_search: bool,
    /// `optionSpecs.maxOutputTokens.max`：生效配置的输出上限（工作线 F93，docs/specs/rust-cli/model.md §11）；配置没给时为
    /// None。WebSearch 的旁路请求按它取 `min(4096, 输出上限)`（Node `model.optionSpecs.maxOutputTokens.max`）。与压缩用的
    /// [`TokenLimits::max_output_tokens`] 由平台层同一次解析写出，是同一个值。
    pub max_output_tokens: Option<u64>,
}

/// 能力里没有写窗口时的缺省上下文窗口（token）。
const DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            context_window: DEFAULT_CONTEXT_WINDOW,
            supports_tool_call: true,
            supports_native_web_search: false,
            supports_mid_conversation_system: false,
            supports_json_schema_output: false,
            requires_mfjs_tool_schema: false,
            supports_image: false,
            supports_pdf: false,
            supports_video: false,
            supports_audio: false,
            supports_tool_search: false,
            max_output_tokens: None,
        }
    }
}

/// 目录里的一个可选模型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub provider: String,
    pub provider_name: String,
    pub model: String,
    pub protocol: WireProtocol,
    /// 可选的推理档位（Provider 配置的 `optionSpecs.reasoningLevel.values`），按配置顺序。
    pub reasoning_levels: Vec<String>,
    pub context_window: u64,
    pub max_output_tokens: u64,
    /// 生效配置的能力（修复线 F23）；`context_window` 与上面的同名字段相同。
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    /// 这一项的账号连接（Team Plan 的团队项目，工作线 F49）；没有时为 None。选中时 `connection.id` 进选择。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<CatalogConnection>,
}

/// 目录项的账号连接（docs/specs/rust-cli/platform.md §9.3）：连接本身与显示用的两个名字，前端按界面语言拼说明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogConnection {
    pub id: ModelConnection,
    /// 账号体系的显示名：`Z.AI` / `BigModel`。
    pub vendor: String,
    /// 团队显示名（App `formatTeamPlanDisplayName`：组织名）。
    pub name: String,
}

/// 带版本的模型目录。配置或账号变化后版本加一。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalog {
    pub revision: u64,
    pub models: Vec<CatalogModel>,
    /// 新会话默认使用的模型；目录为空时没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ModelSelection>,
}

/// 读取模型目录。
#[async_trait]
pub trait ModelDirectory: Send + Sync {
    /// 当前的目录快照（实现方在这里发现配置文件的变化）。
    async fn catalog(&self) -> ModelCatalog;
}

/// 压缩用的模型上限（修复线 F61，docs/specs/rust-cli/compact.md §2）：生效配置的 `contextWindow` 与
/// `optionSpecs.maxOutputTokens.max`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenLimits {
    pub context_window: u64,
    /// 配置没给 max 时为 None（显式覆盖的规则里可能没有），压缩的输出上限按卡片回退。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
}

/// 同步的模型上限查询（修复线 F61，README §6 规则 7 的例外）：只读已经发布的快照，不做 IO、不等。压缩的四处判定
/// （`Compactor` 的同步方法拿不到异步的目录）与 `/context` 只用它。答不出（没有这个模型）时为 None。
pub trait ModelLimits: Send + Sync {
    fn limits(&self, selection: &ModelSelection) -> Option<TokenLimits>;
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
