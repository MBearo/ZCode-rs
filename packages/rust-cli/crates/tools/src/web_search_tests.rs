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
    // 请求形状：身份句在前、`tool_choice: auto`、不带 thinking、max_tokens 与 effort
    // 取模型卡片（脚本模型没有卡片：32000、没有 effort）。
    let system: Vec<&str> = request
        .system
        .iter()
        .map(|block| block.text.as_str())
        .collect();
    assert_eq!(system, [rust_cli_prompts::texts::identity(), SYSTEM]);
    assert_eq!(request.tool_choice, Some(ToolChoice::Auto));
    assert_eq!(request.thinking, None);
    assert_eq!(request.max_tokens, 32_000);
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

/// 有模型卡片时 max_tokens 与 effort 取卡片（`claude-opus-5-5` 是 128000 与 medium）。
#[tokio::test]
async fn the_side_request_follows_the_model_card() {
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
    assert_eq!(request.max_tokens, 128_000);
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
