use std::sync::Arc;

use rust_cli_kernel_api::message::{StopReason, Usage};
use rust_cli_kernel_api::model::ModelEvent;
use rust_cli_kernel_api::testing::{ScriptedInteractions, ScriptedModel, selection};
use rust_cli_kernel_api::tool::{Tool, Validation};
use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::test_more::{Ctx, output_text};

fn start(index: usize, block: ContentBlock) -> ModelEvent {
    ModelEvent::BlockStart { index, block }
}

fn search_response() -> Vec<Result<ModelEvent, rust_cli_kernel_api::model::ModelError>> {
    vec![
        Ok(ModelEvent::MessageStart {
            id: "msg_1".to_owned(),
            model: "m".to_owned(),
            usage: Usage::default(),
        }),
        Ok(start(
            0,
            ContentBlock::Text {
                text: String::new(),
            },
        )),
        Ok(ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Text("I'll search.".to_owned()),
        }),
        Ok(ModelEvent::BlockStop { index: 0 }),
        Ok(start(
            1,
            ContentBlock::ServerToolUse {
                id: "srvtoolu_1".to_owned(),
                name: "web_search".to_owned(),
                input: json!({}),
            },
        )),
        Ok(ModelEvent::BlockStop { index: 1 }),
        Ok(start(
            2,
            ContentBlock::WebSearchToolResult {
                tool_use_id: "srvtoolu_1".to_owned(),
                content: json!([{"type": "web_search_result", "title": "Rust", "url": "https://www.rust-lang.org/", "encrypted_content": "x"}]),
            },
        )),
        Ok(ModelEvent::BlockStop { index: 2 }),
        Ok(start(
            3,
            ContentBlock::Text {
                text: "Found it.".to_owned(),
            },
        )),
        Ok(ModelEvent::BlockStop { index: 3 }),
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ]
}

#[tokio::test]
async fn a_side_request_with_the_server_tool_becomes_the_result() {
    let model = Arc::new(ScriptedModel::new(vec![search_response()]));
    let tool = WebSearchTool::new(SideModel {
        model: model.clone(),
        selection: selection(),
    });
    let (progress, _) = mpsc::channel(1);
    let context = Ctx::new(Arc::new(ScriptedInteractions::default())).build();
    let input = json!({"query": "rust lang", "blocked_domains": ["example.org"]});
    let output = tool.call(input, context, progress).await.unwrap();
    assert_eq!(
        output_text(&output),
        "Web search results for query: \"rust lang\"\n\nI'll search.\n\nLinks: [{\"title\":\"Rust\",\"url\":\"https://www.rust-lang.org/\"}]\n\nFound it.\n\n\nREMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks."
    );
    let request = &model.requests()[0];
    assert_eq!(
        request.source,
        QuerySource::Side("web_search_tool".to_owned())
    );
    // 工作线 F52：带上工具所在的会话（`metadata.user_id` 的 `session_id`，model.md §3.3）。
    assert_eq!(
        request.options.session_id,
        Some(rust_cli_kernel_api::ids::SessionId::new("sess_1"))
    );
    // 工作线 F64：工具所在的回合（每请求头的 `x-query-id`，model.md §2.4）。
    assert!(request.options.run.is_some());
    // 请求形状照 Node（工作线 F90，tools-more.md §5）：system 只有 Node 的一句（带句号，没有身份句）、不写
    // `tool_choice`、max_tokens 4096、不带 thinking；effort 取模型卡片（脚本模型没有卡片：没有 effort）。
    // 修改：L17 到 F90 之间是身份句加不带句号的一句、`tool_choice: auto`、max_tokens 取卡片（没有卡片 32000）。
    let system: Vec<&str> = request
        .system
        .iter()
        .map(|block| block.text.as_str())
        .collect();
    assert_eq!(
        system,
        ["You are an assistant for performing a web search tool use."]
    );
    assert_eq!(
        request.messages[0].content[0].block,
        ContentBlock::Text {
            text: "Perform a web search for the query: rust lang".to_owned()
        }
    );
    assert_eq!(request.tool_choice, None);
    assert_eq!(request.thinking, None);
    assert_eq!(request.max_tokens, 4096);
    assert_eq!(request.options.effort, None);
    assert_eq!(
        request.options.server_tools,
        [ServerTool::WebSearch {
            allowed_domains: None,
            blocked_domains: Some(vec!["example.org".to_owned()]),
            max_uses: 8,
        }]
    );
}

/// 每条结果后接两个换行，再接 `\nREMINDER…`，所以提醒前是三个换行。
#[test]
fn search_errors_and_empty_results_have_their_own_lines() {
    let items = items(vec![
        ContentBlock::WebSearchToolResult {
            tool_use_id: "s".to_owned(),
            content: json!({"type": "web_search_tool_result_error", "error_code": "max_uses_exceeded"}),
        },
        ContentBlock::WebSearchToolResult {
            tool_use_id: "s".to_owned(),
            content: json!([]),
        },
    ]);
    assert_eq!(
        result_text("q", &items),
        format!(
            "Web search results for query: \"q\"\n\nWeb search error: max_uses_exceeded\n\nNo links found.\n\n\n{REMINDER}"
        )
    );
}

#[tokio::test]
async fn validation_and_permissions_follow_the_spec() {
    let tool = WebSearchTool::new(SideModel {
        model: Arc::new(ScriptedModel::new(Vec::new())),
        selection: selection(),
    });
    let context = Ctx::new(Arc::new(ScriptedInteractions::default())).build();
    assert_eq!(
        tool.validate_input(&json!({"query": ""}), &context).await,
        Validation::Invalid {
            message: "Error: Missing query".to_owned(),
            error_code: Some(1),
        }
    );
    let both = json!({"query": "q", "allowed_domains": ["a.com"], "blocked_domains": ["b.com"]});
    assert!(matches!(
        tool.validate_input(&both, &context).await,
        Validation::Invalid {
            error_code: Some(2),
            ..
        }
    ));
    match tool
        .check_permissions(&json!({"query": "q"}), &context)
        .await
    {
        PermissionDecision::Ask { suggestions, .. } => assert_eq!(
            suggestions,
            [PermissionRule {
                tool: NAME.to_owned(),
                content: None
            }]
        ),
        other => panic!("unexpected {other:?}"),
    }
    assert!(tool.should_defer());
}

/// 会话 actor 给出当前主模型的交互替身（`/model` 切换之后）。
struct Switched(rust_cli_kernel_api::model::ModelSelection);

#[async_trait::async_trait]
impl rust_cli_kernel_api::orchestration::Interactions for Switched {
    async fn ask(
        &self,
        _session: &rust_cli_kernel_api::ids::SessionId,
        _request: rust_cli_kernel_api::orchestration::InteractionRequest,
    ) -> Result<
        rust_cli_kernel_api::orchestration::InteractionAnswer,
        rust_cli_kernel_api::orchestration::OrchestrationError,
    > {
        unreachable!("WebSearch does not ask")
    }

    async fn current_selection(
        &self,
        _session: &rust_cli_kernel_api::ids::SessionId,
    ) -> Option<rust_cli_kernel_api::model::ModelSelection> {
        Some(self.0.clone())
    }
}

/// L13 发现的 bug：WebSearch 用启动时的默认模型。应该用主循环的当前模型，所以按请求时会话当前的主模型
/// （不经小模型槽位）。
#[tokio::test]
async fn the_side_request_uses_the_current_main_model() {
    let model = Arc::new(ScriptedModel::new(vec![search_response()]));
    let tool = WebSearchTool::new(SideModel {
        model: model.clone(),
        selection: selection(),
    });
    let current = rust_cli_kernel_api::model::ModelSelection {
        provider: "fixture".to_owned(),
        model: "switched".to_owned(),
        reasoning: Some("high".to_owned()),
        connection: None,
    };
    let (progress, _) = mpsc::channel(1);
    let context = Ctx::new(Arc::new(Switched(current.clone()))).build();
    tool.call(json!({"query": "rust lang"}), context, progress)
        .await
        .unwrap();
    assert_eq!(model.requests()[0].selection, current);
}

/// 有模型卡片时 effort 取卡片（`claude-opus-5-5` 是 medium）；max_tokens 不取卡片，照 Node 是 4096（工作线 F90，
/// 修改：以前取卡片的 128000）。
#[tokio::test]
async fn the_side_request_takes_the_effort_from_the_model_card() {
    let model = Arc::new(ScriptedModel::new(vec![search_response()]));
    let tool = WebSearchTool::new(SideModel {
        model: model.clone(),
        selection: rust_cli_kernel_api::model::ModelSelection {
            provider: "anthropic".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            reasoning: None,
            connection: None,
        },
    });
    let (progress, _) = mpsc::channel(1);
    let context = Ctx::new(Arc::new(ScriptedInteractions::default())).build();
    tool.call(json!({"query": "q"}), context, progress)
        .await
        .unwrap();
    let request = &model.requests()[0];
    assert_eq!(request.max_tokens, 4096);
    assert_eq!(request.options.effort.as_deref(), Some("medium"));
}

/// 修复线 F23：WebSearch 只在生效配置 `supportsNativeWebSearch` 为真时发给模型（主循环按它去掉，tools-more.md §5）；
/// 其他内置工具不受影响。没有卡片的模型（这里是 switched）不写 effort，即使会话选了档位（推理字段只由 option map 写）。
#[tokio::test]
async fn web_search_needs_the_native_web_search_capability() {
    let model = Arc::new(ScriptedModel::new(vec![search_response()]));
    let tool = WebSearchTool::new(SideModel {
        model: model.clone(),
        selection: selection(),
    });
    assert!(tool.needs_native_web_search());
    let fetch = crate::web_fetch::WebFetchTool::new(
        None,
        Arc::new(rust_cli_kernel_api::testing::FixedClock(0)),
    );
    assert!(!fetch.needs_native_web_search());
    let current = rust_cli_kernel_api::model::ModelSelection {
        provider: "fixture".to_owned(),
        model: "switched".to_owned(),
        reasoning: Some("high".to_owned()),
        connection: None,
    };
    let (progress, _) = mpsc::channel(1);
    let context = Ctx::new(Arc::new(Switched(current))).build();
    tool.call(json!({"query": "q"}), context, progress)
        .await
        .unwrap();
    assert_eq!(model.requests()[0].options.effort, None);
}

/// 模型端口替身：流交给脚本模型，能力只回答 `supportsNativeWebSearch` 与输出上限（`web_search` 为 None 是答不出）。
struct Capable {
    scripted: Arc<ScriptedModel>,
    web_search: Option<bool>,
    max_output_tokens: Option<u64>,
}

impl rust_cli_kernel_api::model::ModelPort for Capable {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<ModelEvent, rust_cli_kernel_api::model::ModelError>,
    > {
        self.scripted.stream(request, cancel)
    }

    fn capabilities(
        &self,
        _selection: &rust_cli_kernel_api::model::ModelSelection,
    ) -> futures_util::future::BoxFuture<
        'static,
        Option<rust_cli_kernel_api::provider::ModelCapabilities>,
    > {
        let (web_search, max_output_tokens) = (self.web_search, self.max_output_tokens);
        Box::pin(async move {
            web_search.map(|supports_native_web_search| {
                rust_cli_kernel_api::provider::ModelCapabilities {
                    supports_native_web_search,
                    max_output_tokens,
                    ..Default::default()
                }
            })
        })
    }
}

/// 工作线 F90（tools-more.md §5 的“能力检查”，照 Node `websearch.ts`）：调用时当前模型的生效配置
/// `supportsNativeWebSearch` 为假，不发旁路请求，结果是 Node 的可恢复错误；为真或答不出时照常请求。修改前不检查，
/// 模型从历史里看到 WebSearch 时会对不支持的模型发带服务端工具的请求。
#[tokio::test]
async fn a_model_without_native_web_search_gets_the_recoverable_error() {
    let run = |web_search: Option<bool>| async move {
        let scripted = Arc::new(ScriptedModel::new(vec![search_response()]));
        let tool = WebSearchTool::new(SideModel {
            model: Arc::new(Capable {
                scripted: scripted.clone(),
                web_search,
                max_output_tokens: None,
            }),
            selection: selection(),
        });
        let (progress, _) = mpsc::channel(1);
        let context = Ctx::new(Arc::new(ScriptedInteractions::default())).build();
        let outcome = tool.call(json!({"query": "q"}), context, progress).await;
        (outcome, scripted.requests().len())
    };
    let (outcome, requests) = run(Some(false)).await;
    assert_eq!(requests, 0);
    match outcome {
        Err(ToolError::Failed { message }) => {
            assert_eq!(message, "Current model does not support native WebSearch");
        }
        other => panic!("{other:?}"),
    }
    for web_search in [Some(true), None] {
        let (outcome, requests) = run(web_search).await;
        assert!(outcome.is_ok(), "{web_search:?}");
        assert_eq!(requests, 1);
    }
}

/// 工作线 F93（tools-more.md §5，照 Node `Math.min(4096, model.optionSpecs.maxOutputTokens.max)`）：旁路请求的
/// `max_tokens` 是 4096 与当前模型生效配置的输出上限的较小值；答不出能力或配置没给上限时是 4096。修改前固定 4096，
/// 上限 1024 的模型（glm-4v-flash）也发 4096。
#[tokio::test]
async fn the_side_request_caps_max_tokens_at_the_configured_output_limit() {
    let run = |web_search: Option<bool>, max_output_tokens: Option<u64>| async move {
        let scripted = Arc::new(ScriptedModel::new(vec![search_response()]));
        let tool = WebSearchTool::new(SideModel {
            model: Arc::new(Capable {
                scripted: scripted.clone(),
                web_search,
                max_output_tokens,
            }),
            selection: selection(),
        });
        let (progress, _) = mpsc::channel(1);
        let context = Ctx::new(Arc::new(ScriptedInteractions::default())).build();
        tool.call(json!({"query": "q"}), context, progress)
            .await
            .unwrap();
        scripted.requests()[0].max_tokens
    };
    assert_eq!(run(Some(true), Some(1024)).await, 1024);
    assert_eq!(run(Some(true), Some(128_000)).await, 4096);
    assert_eq!(run(Some(true), Some(4096)).await, 4096);
    assert_eq!(run(Some(true), None).await, 4096);
    assert_eq!(run(None, None).await, 4096);
}
