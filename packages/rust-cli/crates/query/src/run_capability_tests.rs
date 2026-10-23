//! 模型能力只取生效配置（修复线 F23，model.md §11、tools-more.md §5）：没有卡片的模型不写卡片字段；WebSearch
//! 按 `supportsNativeWebSearch` 去留。

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelPort, ModelRequest, ModelSelection};
use rust_cli_kernel_api::provider::ModelCapabilities;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::tests::{parts, request, scope};

/// 只有名字的工具；`web_search` 为真时像 WebSearch 一样要服务端 web search；`deferred` 为真时是延迟工具。
struct Named {
    name: &'static str,
    web_search: bool,
    deferred: bool,
}

fn named(name: &'static str) -> Arc<dyn Tool> {
    Arc::new(Named {
        name,
        web_search: false,
        deferred: false,
    })
}

#[async_trait]
impl Tool for Named {
    fn name(&self) -> &str {
        self.name
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &PromptContext) -> String {
        format!("{} tool", self.name)
    }
    fn needs_native_web_search(&self) -> bool {
        self.web_search
    }
    fn should_defer(&self) -> bool {
        self.deferred
    }
    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Unsupported {
            what: "test".to_owned(),
        })
    }
}

/// 按固定的生效配置答能力的模型端口（`None` 是答不出，同测试替身的缺省）。
struct Configured {
    inner: ScriptedModel,
    capabilities: Option<ModelCapabilities>,
}

impl ModelPort for Configured {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        self.inner.stream(request, cancel)
    }

    fn capabilities(
        &self,
        _selection: &ModelSelection,
    ) -> BoxFuture<'static, Option<ModelCapabilities>> {
        let found = self.capabilities;
        Box::pin(async move { found })
    }
}

async fn first_request(
    selection: ModelSelection,
    capabilities: Option<ModelCapabilities>,
) -> ModelRequest {
    let tools: Vec<Arc<dyn Tool>> = vec![
        named("Read"),
        Arc::new(Named {
            name: "WebSearch",
            web_search: true,
            deferred: false,
        }),
    ];
    first_request_with(selection, capabilities, tools).await
}

async fn first_request_with(
    selection: ModelSelection,
    capabilities: Option<ModelCapabilities>,
    tools: Vec<Arc<dyn Tool>>,
) -> ModelRequest {
    let model = Arc::new(Configured {
        inner: ScriptedModel::new(vec![text_response(&["ok"])]),
        capabilities,
    });
    let mut request = request("hi");
    request.selection = selection;
    let (scope, _sink) = scope(parts(model.clone(), tools), &request, Abort::new());
    super::run(scope, request).await.unwrap();
    model.inner.requests().remove(0)
}

fn glm() -> ModelSelection {
    ModelSelection {
        provider: "zai".to_owned(),
        model: "glm-5.1".to_owned(),
        reasoning: Some("enabled".to_owned()),
        connection: None,
    }
}

/// 修 bug：没有卡片的模型以前把选择的档位写进 `output_config.effort`（GLM-5.1 的 `"effort":"enabled"`）；
/// 推理字段只由配置的 option map 写（同 Node）。
#[tokio::test]
async fn a_model_without_a_card_gets_no_card_fields() {
    let request = first_request(glm(), None).await;
    assert_eq!(request.options.effort, None);
    assert_eq!(request.thinking, None);
    assert!(request.options.context_edits.is_empty());
    assert_eq!(request.max_tokens, 32_000);
    // 卡片模型照旧带卡片字段。
    let opus = ModelSelection {
        provider: "anthropic".to_owned(),
        model: "claude-opus-5-5".to_owned(),
        reasoning: Some("high".to_owned()),
        connection: None,
    };
    let request = first_request(opus, None).await;
    assert_eq!(request.options.effort.as_deref(), Some("high"));
    assert!(request.thinking.is_some());
}

fn names(request: &ModelRequest) -> Vec<&str> {
    request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect()
}

/// WebSearch 只在生效配置 `supportsNativeWebSearch` 为真时发给模型（同 Node）；答不出能力时不去掉。
#[tokio::test]
async fn web_search_follows_the_native_web_search_capability() {
    let without = ModelCapabilities {
        supports_native_web_search: false,
        ..ModelCapabilities::default()
    };
    let with = ModelCapabilities {
        supports_native_web_search: true,
        ..ModelCapabilities::default()
    };
    assert_eq!(names(&first_request(glm(), Some(without)).await), ["Read"]);
    assert_eq!(
        names(&first_request(glm(), Some(with)).await),
        ["Read", "WebSearch"]
    );
    assert_eq!(
        names(&first_request(glm(), None).await),
        ["Read", "WebSearch"]
    );
}

fn gateway_opus() -> ModelSelection {
    ModelSelection {
        provider: "gateway".to_owned(),
        model: "claude-opus-5-5".to_owned(),
        reasoning: None,
        connection: None,
    }
}

/// Read、ToolSearch 与一个延迟工具（TaskStop）。
fn search_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        named("Read"),
        named("ToolSearch"),
        Arc::new(Named {
            name: "TaskStop",
            web_search: false,
            deferred: true,
        }),
    ]
}

fn endpoint(tool_search: bool) -> Option<ModelCapabilities> {
    Some(ModelCapabilities {
        supports_tool_search: tool_search,
        ..ModelCapabilities::default()
    })
}

/// 工作线 F52（tools-more.md §3.2，用户决定）：ToolSearch 只看端点的能力，不看模型卡片。修 bug：用户的场景（anthropic
/// 协议的网关上的 claude-opus-5-5，网关后面是 Bedrock）以前按卡片打开 ToolSearch，请求带 `defer_loading`，beta
/// `advanced-tool-use` 被拒绝。现在端点不支持时走“没有 ToolSearch”的分支：全部内联，没有占位工具与 `defer_loading`。
#[tokio::test]
async fn tool_search_follows_the_endpoint_not_the_model_card() {
    let request = first_request_with(gateway_opus(), endpoint(false), search_tools()).await;
    assert_eq!(names(&request), ["Read", "TaskStop"]);
    assert!(
        request
            .options
            .tool_flags
            .values()
            .all(|flags| !flags.defer_loading),
        "{:?}",
        request.options.tool_flags
    );
    // z.ai / bigmodel 端点上的 GLM（没有卡片）：开；占位工具在倒数第二位（tools-more.md §3.3）。
    let request = first_request_with(glm(), endpoint(true), search_tools()).await;
    assert_eq!(
        names(&request),
        ["Read", "DeferredToolPlaceholder", "ToolSearch"]
    );
    assert!(request.options.tool_flags["DeferredToolPlaceholder"].defer_loading);
}

/// 模型端口答不出能力（测试替身）时按卡片回退，与 `system_turns` 的回退相同（model.md §11）。
#[tokio::test]
async fn without_capabilities_the_card_is_the_fallback() {
    let request = first_request_with(gateway_opus(), None, search_tools()).await;
    assert_eq!(
        names(&request),
        ["Read", "DeferredToolPlaceholder", "ToolSearch"]
    );
    let request = first_request_with(glm(), None, search_tools()).await;
    assert_eq!(names(&request), ["Read", "TaskStop"]);
}

/// 工作线 F52（model.md §3.3）：主循环、子代理与压缩的请求都带上发请求的会话（子代理跑在子会话里，`scope.session`
/// 就是子会话），模型适配器据此写 `metadata.user_id`。
#[tokio::test]
async fn the_request_names_the_session_that_sends_it() {
    let request = first_request(glm(), None).await;
    assert_eq!(
        request.options.session_id,
        Some(rust_cli_kernel_api::ids::SessionId::new("sess_1"))
    );
    // 工作线 F64（model.md §2.4）：这次运行（回合）给每请求头的 `x-query-id`；主线程没有子代理。
    assert!(request.options.run.is_some());
    assert_eq!(request.options.agent, None);
}
