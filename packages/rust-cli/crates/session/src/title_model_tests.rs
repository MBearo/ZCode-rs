//! 标题请求的形状与辅助选项（titles.md §3）。
use async_trait::async_trait;
use rust_cli_kernel_api::provider::{CatalogModel, ModelCatalog, WireProtocol};
use rust_cli_kernel_api::testing::{FixedClock, ScriptedModel, selection, text_response};

use super::*;

struct Directory;

#[async_trait]
impl ModelDirectory for Directory {
    async fn catalog(&self) -> ModelCatalog {
        let main = selection();
        ModelCatalog {
            revision: 1,
            models: vec![CatalogModel {
                capabilities: Default::default(),
                provider: main.provider.clone(),
                provider_name: "Test".to_owned(),
                model: main.model,
                protocol: WireProtocol::AnthropicMessages,
                reasoning_levels: vec!["off".to_owned(), "high".to_owned()],
                context_window: 200_000,
                max_output_tokens: 4_096,
                connection: None,
            }],
            default: None,
        }
    }
}

#[tokio::test]
async fn the_request_uses_the_title_prompt_and_node_auxiliary_options() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&[
        "{\"title\": \"Config ",
        "files\"}",
    ])]));
    let titles = TitleModel {
        model: model.clone(),
        directory: Some(Arc::new(Directory)),
    };
    let generated = titles
        .generate(
            &selection(),
            (
                &SessionId::new("sess_title"),
                Some(&RunId::new("run_title")),
            ),
            "  read the   Node config files  ",
            (&FixedClock(7_000), MessageId::new("msg_title")),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(generated.title.as_deref(), Some("Config files"));
    // 用量事实（工作线 F73，usage.md §2.1.1）：请求时的选择与来源、注入的时钟、没有首字时间。
    let call = &generated.call;
    assert_eq!(call.message, MessageId::new("msg_title"));
    assert_eq!(call.selection, model.requests()[0].selection);
    assert_eq!(call.source, model.requests()[0].source);
    assert_eq!((call.started_at_ms, call.completed_at_ms), (7_000, 7_000));
    assert_eq!(call.first_token_at_ms, None);
    assert_eq!(call.status, CallStatus::Completed);
    let request = &model.requests()[0];
    // 工作线 F52：带上会话（`metadata.user_id` 的 `session_id`）。
    assert_eq!(
        request.options.session_id,
        Some(SessionId::new("sess_title"))
    );
    // 工作线 F64：提交这条输入的回合（每请求头的 `x-query-id`）。
    assert_eq!(request.options.run, Some(RunId::new("run_title")));
    assert_eq!(request.system[0].text, title::system());
    let ContentBlock::Text { text } = &request.messages[0].content[0].block else {
        panic!("text expected");
    };
    assert_eq!(
        text,
        &format!(
            "<session>\nread the Node config files\n</session>\n\n{}",
            title::language_default()
        )
    );
    assert!(request.tools.is_empty());
    assert_eq!(request.thinking, None);
    assert_eq!(request.max_tokens, 4_096);
    assert_eq!(request.selection.reasoning.as_deref(), Some("off"));
    assert_eq!(request.selection.model, selection().model);
    assert_eq!(
        request.source,
        QuerySource::Side("generate_session_title".to_owned())
    );
}

#[tokio::test]
async fn without_a_directory_the_output_cap_is_5000() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["Hook trust"])]));
    let titles = TitleModel {
        model: model.clone(),
        directory: None,
    };
    let generated = titles
        .generate(
            &selection(),
            (&SessionId::new("sess_title"), None),
            "trust the project hooks",
            (&FixedClock(0), MessageId::new("msg_title")),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(generated.title.as_deref(), Some("Hook trust"));
    assert_eq!(model.requests()[0].max_tokens, 5_000);
    assert!(long_enough("0123456789"));
    assert!(!long_enough("  short  "));
}
