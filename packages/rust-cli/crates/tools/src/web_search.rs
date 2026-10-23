//! WebSearch（docs/specs/rust-cli/tools-more.md §5）。
//!
//! 客户端工具：`call` 发一次旁路请求，请求里带 Anthropic 的服务端工具 `web_search`，从响应的
//! `server_tool_use` / `web_search_tool_result` / 文本块整理出结果。
//! 非 Anthropic 协议由模型适配器报 `Unsupported`，工具把它作为错误交给模型。

use futures_util::StreamExt;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, BlockDelta, ModelEvent, ModelRequest, QuerySource,
    RequestOptions, ServerTool, SystemBlock, ToolChoice,
};
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionRule};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::common::{Definition, definition, failed, text};
use crate::web_fetch::SideModel;

pub const NAME: &str = "WebSearch";

/// 每次请求最多搜索的次数。
const MAX_USES: u32 = 8;
/// 没有模型卡片时的输出上限（同主循环的缺省，prompts 的一个常量，F58）。
const DEFAULT_MAX_TOKENS: u32 = rust_cli_prompts::models::UNKNOWN_MAX_OUTPUT_TOKENS;
const SYSTEM: &str = "You are an assistant for performing a web search tool use";
const REMINDER: &str = "REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.";

pub struct WebSearchTool {
    definition: Definition,
    side: SideModel,
}

impl WebSearchTool {
    pub fn new(side: SideModel) -> Self {
        Self {
            definition: definition(NAME),
            side,
        }
    }
}

/// 一条结果：模型的文字，或一次搜索的链接（`results` 数组）。
enum Item {
    Text(String),
    Links(Vec<Value>),
}

fn domains(input: &Value, key: &str) -> Option<Vec<String>> {
    input.get(key).and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect()
    })
}

/// 按块的顺序，搜索调用之间的文字各成一条。
fn items(blocks: Vec<ContentBlock>) -> Vec<Item> {
    let mut out = Vec::new();
    let mut pending = String::new();
    for block in blocks {
        match block {
            ContentBlock::ServerToolUse { .. } => {
                if !pending.trim().is_empty() {
                    out.push(Item::Text(pending.trim().to_owned()));
                }
                pending.clear();
            }
            ContentBlock::WebSearchToolResult { content, .. } => match content.as_array() {
                Some(results) => out.push(Item::Links(
                    results
                        .iter()
                        .map(|result| json!({"title": result["title"], "url": result["url"]}))
                        .collect(),
                )),
                None => {
                    let code = content["error_code"].as_str().unwrap_or("undefined");
                    out.push(Item::Text(format!("Web search error: {code}")));
                }
            },
            ContentBlock::Text { text } => pending.push_str(&text),
            _ => {}
        }
    }
    if !pending.trim().is_empty() {
        out.push(Item::Text(pending.trim().to_owned()));
    }
    out
}

/// 结果文本。
fn result_text(query: &str, items: &[Item]) -> String {
    let mut out = format!("Web search results for query: \"{query}\"\n\n");
    for item in items {
        match item {
            Item::Text(text) => out.push_str(&format!("{text}\n\n")),
            Item::Links(links) if !links.is_empty() => {
                out.push_str(&format!("Links: {}\n\n", Value::Array(links.clone())));
            }
            Item::Links(_) => out.push_str("No links found.\n\n"),
        }
    }
    out.push('\n');
    out.push_str(REMINDER);
    out.trim().to_owned()
}

impl WebSearchTool {
    async fn search(
        &self,
        input: &Value,
        context: &ToolUseContext,
    ) -> Result<Vec<Item>, ToolError> {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // 修 bug（L13 发现）：以前用组装时的 `side.selection`（启动时的默认模型），`/model` 切换后不跟。
        // 用主循环的当前模型，所以在请求时向会话 actor 取当前主模型，不经小模型
        // 槽位（model.md §6.1）；没有会话 owner 的测试替身取不到时才用组装时的值。
        let selection = context
            .interactions
            .current_selection(&context.session)
            .await
            .unwrap_or_else(|| self.side.selection.clone());
        // 请求形状（L17 修正）：system 先是 CLI 的身份句、
        // `tool_choice: auto`、不带 thinking，max_tokens 与 effort 取模型卡片。以前是旧的写法
        // （只有一段 system、强制 `web_search`、thinking disabled、max_tokens 8192）。
        let card = rust_cli_prompts::models::card(&selection.model);
        // 修 bug（F23）：没有卡片的模型以前也写选择的档位（`card_effort` 的注释）。
        let effort =
            rust_cli_prompts::models::card_effort(&selection.model, selection.reasoning.as_deref());
        let max_tokens = card.map_or(DEFAULT_MAX_TOKENS, |card| card.max_output_tokens);
        let block = |text: &str| SystemBlock {
            text: text.to_owned(),
            cache_control: None,
        };
        let request = ModelRequest {
            selection,
            system: vec![block(rust_cli_prompts::texts::identity()), block(SYSTEM)],
            messages: vec![ApiMessage {
                role: ApiRole::User,
                content: vec![ApiContent {
                    block: ContentBlock::Text {
                        text: format!("Perform a web search for the query: {query}"),
                    },
                    cache_control: None,
                }],
                output_config: None,
            }],
            tools: Vec::new(),
            tool_choice: Some(ToolChoice::Auto),
            max_tokens,
            thinking: None,
            temperature: None,
            betas: Vec::new(),
            source: QuerySource::Side("web_search_tool".to_owned()),
            options: RequestOptions {
                effort,
                server_tools: vec![ServerTool::WebSearch {
                    allowed_domains: domains(input, "allowed_domains"),
                    blocked_domains: domains(input, "blocked_domains"),
                    max_uses: MAX_USES,
                }],
                // 工作线 F52：`metadata.user_id` 的 `session_id`（model.md §3.3）。
                session_id: Some(context.session.clone()),
                // 工作线 F64：每请求头的回合与子代理（model.md §2.4）。
                run: Some(context.run.clone()),
                agent: context.agent.clone(),
                ..RequestOptions::default()
            },
        };
        let mut stream = self.side.model.stream(request, context.cancel.clone());
        let mut blocks: Vec<(usize, ContentBlock)> = Vec::new();
        while let Some(event) = stream.next().await {
            match event.map_err(|error| failed(error.to_string()))? {
                ModelEvent::BlockStart { index, block } => blocks.push((index, block)),
                ModelEvent::BlockDelta {
                    index,
                    delta: BlockDelta::Text(more),
                } => {
                    if let Some((_, ContentBlock::Text { text })) =
                        blocks.iter_mut().find(|(at, _)| *at == index)
                    {
                        text.push_str(&more);
                    }
                }
                _ => {}
            }
        }
        if context.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        Ok(items(blocks.into_iter().map(|(_, block)| block).collect()))
    }
}

#[async_trait::async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        NAME
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn should_defer(&self) -> bool {
        true
    }

    /// 只在当前模型的生效配置 `supportsNativeWebSearch` 为真时发给模型（修复线 F23，tools-more.md §5）。
    fn needs_native_web_search(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("search the web for current information")
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(100_000)
    }

    async fn validate_input(&self, input: &Value, _context: &ToolUseContext) -> Validation {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let invalid = |message: &str, code: i32| Validation::Invalid {
            message: message.to_owned(),
            error_code: Some(code),
        };
        if query.is_empty() {
            return invalid("Error: Missing query", 1);
        }
        let given = |key: &str| domains(input, key).is_some_and(|list| !list.is_empty());
        if given("allowed_domains") && given("blocked_domains") {
            return invalid(
                "Error: Cannot specify both allowed_domains and blocked_domains in the same request",
                2,
            );
        }
        Validation::Ok
    }

    /// 返回 passthrough 并建议规则 `WebSearch`：给出询问文本与建议。
    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Ask {
            message: rust_cli_policy::texts::request_message(NAME, None),
            suggestions: vec![PermissionRule {
                tool: NAME.to_owned(),
                content: None,
            }],
        }
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let items = self.search(&input, &context).await?;
        Ok(text(result_text(query, &items)))
    }
}

#[cfg(test)]
#[path = "web_search_tests.rs"]
mod tests;
