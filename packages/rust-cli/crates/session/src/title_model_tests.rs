//! 标题请求的形状与辅助选项（titles.md §3）。
use async_trait::async_trait;
use rust_cli_kernel_api::provider::{CatalogModel, ModelCatalog, WireProtocol};
use rust_cli_kernel_api::testing::{FixedClock, ScriptedModel, selection, text_response};

use super::*;

/// 只有会话模型一项的目录；字段是这个模型配置的输出上限。
struct Directory(u64);

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
                max_output_tokens: self.0,
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
        directory: Some(Arc::new(Directory(4_096))),
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
    // 工作线 F97（titles.md §3）：同一个上限交给 Provider 补丁。
    assert_eq!(request.options.provider_max_output_tokens, Some(4_096));
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
    // 显式覆盖的端点没有补丁，这个值不起作用；声明的仍是这次请求自己的上限（同 WebSearch）。
    assert_eq!(
        model.requests()[0].options.provider_max_output_tokens,
        Some(5_000)
    );
    assert!(long_enough("0123456789"));
    assert!(!long_enough("  short  "));
}

/// 工作线 F97（titles.md §3）：目录模型的上限高于 5000 时，请求自己的上限是 5000，并把它交给 Provider 补丁
/// （`provider_max_output_tokens`），目录模型上发出的才是 Node 的 `min(5000, 上限)`。修改前不交，补丁写配置的上限。
#[tokio::test]
async fn a_catalog_cap_above_5000_hands_5000_to_the_provider_patch() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["Hook trust"])]));
    let titles = TitleModel {
        model: model.clone(),
        directory: Some(Arc::new(Directory(128_000))),
    };
    titles
        .generate(
            &selection(),
            (&SessionId::new("sess_title"), None),
            "trust the project hooks",
            (&FixedClock(0), MessageId::new("msg_title")),
            CancellationToken::new(),
        )
        .await;
    let request = &model.requests()[0];
    assert_eq!(request.max_tokens, 5_000);
    assert_eq!(request.options.provider_max_output_tokens, Some(5_000));
}

/// 一次标题请求的用量：脚本模型的文字回复，`message_start` 的用量是 `started`，`message_delta` 带适配器合并好的累计
/// 用量 `reported`。
async fn title_usage(started: Usage, reported: Usage) -> Option<Usage> {
    let mut events = text_response(&["Hook trust"]);
    for event in &mut events {
        match event {
            Ok(ModelEvent::MessageStart { usage, .. }) => *usage = started,
            Ok(ModelEvent::MessageDelta { usage, .. }) => *usage = reported,
            _ => {}
        }
    }
    let titles = TitleModel {
        model: Arc::new(ScriptedModel::new(vec![events])),
        directory: None,
    };
    titles
        .generate(
            &selection(),
            (&SessionId::new("sess_title"), None),
            "trust the project hooks",
            (&FixedClock(0), MessageId::new("msg_title")),
            CancellationToken::new(),
        )
        .await
        .call
        .usage
}

/// 修复线 F99（query.md §1.10、titles.md §4）：输入与缓存只在 `message_delta` 里的端点，标题请求的用量行带上它们。
/// 修 bug 前只取输出，输入与缓存是 0。
#[tokio::test]
async fn usage_reported_only_in_the_message_delta_is_kept() {
    let reported = Usage {
        input_tokens: 11_922,
        output_tokens: 514,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 448,
    };
    assert_eq!(
        title_usage(Usage::default(), reported).await,
        Some(reported)
    );
}

/// 修复线 F99：输入在 `message_start` 里的端点，累计用量带着同样的输入与缓存，结果与以前相同。
#[tokio::test]
async fn usage_with_the_input_in_the_message_start_is_unchanged() {
    let started = Usage {
        input_tokens: 20,
        output_tokens: 1,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 4,
    };
    let reported = Usage {
        output_tokens: 6,
        ..started
    };
    assert_eq!(title_usage(started, reported).await, Some(reported));
}
