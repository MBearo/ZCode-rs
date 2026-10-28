//! 目录模型的输出预算（修复线 F108，query.md §1.11、model.md §9）：主线程、子代理与压缩摘要的请求交给 Provider 补丁的
//! 输出上限按窗口余量压低。估算是最近一次带用量的 assistant 的用量加之后消息的粗估（与压缩的判定同一个函数）。

use std::sync::Arc;

use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::compaction::rough_tokens;
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::{
    ModelError, ModelEvent, ModelPort, ModelRequest, ModelSelection, QuerySource,
};
use rust_cli_kernel_api::provider::ModelCapabilities;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio_util::sync::CancellationToken;

use super::tests::{parts, request, scope};
use crate::RequestLimits;

/// 用户遇到的模型：窗口 1,000,000、配置的输出上限 384,000。
const WINDOW: u64 = 1_000_000;
const CONFIGURED_MAX: u64 = 384_000;
/// 最后一条 user 消息的文字（粗估 1 个 token）。
const NEXT: &str = "next";

/// 按固定的生效配置答能力的模型端口（`None` 是答不出）。
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

fn configured(context_window: u64) -> Option<ModelCapabilities> {
    Some(ModelCapabilities {
        context_window,
        max_output_tokens: Some(CONFIGURED_MAX),
        ..ModelCapabilities::default()
    })
}

/// 一段历史：一次输入、一条带用量的回复（输入 `input`、输出 `output`）、再一次输入 `next`。
fn history(input: u64, output: u64) -> Vec<Message> {
    let user = |id: &str, text: &str| {
        Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            origin: UserOrigin::Human,
        })
    };
    vec![
        user("msg_first", "read the logs"),
        Message::Assistant(AssistantMessage {
            id: MessageId::new("msg_reply"),
            content: vec![ContentBlock::Text {
                text: "done".to_owned(),
            }],
            model: "gateway-model".to_owned(),
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(Usage {
                input_tokens: input,
                output_tokens: output,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            }),
        }),
        user("msg_next", NEXT),
    ]
}

/// 这段历史的估算：回复的用量（输入 + 输出）加之后那条输入的粗估。
fn estimate(input: u64, output: u64) -> u64 {
    input + output + rough_tokens(NEXT)
}

/// 跑一次主循环，返回第一次请求。
async fn first_request(
    capabilities: Option<ModelCapabilities>,
    messages: Vec<Message>,
    source: QuerySource,
    limits: RequestLimits,
) -> ModelRequest {
    let model = Arc::new(Configured {
        inner: ScriptedModel::new(vec![text_response(&["ok"])]),
        capabilities,
    });
    let mut request = request("unused");
    request.messages = messages;
    request.source = source;
    request.selection = ModelSelection {
        provider: "gateway".to_owned(),
        model: "gateway-model".to_owned(),
        reasoning: None,
        connection: None,
    };
    let mut parts = parts(model.clone(), Vec::new());
    parts.limits = limits;
    let (scope, _sink) = scope(parts, &request, Abort::new());
    super::run(scope, request).await.unwrap();
    model.inner.requests().remove(0)
}

async fn budget(
    capabilities: Option<ModelCapabilities>,
    (input, output): (u64, u64),
    source: QuerySource,
    limits: RequestLimits,
) -> Option<u32> {
    first_request(capabilities, history(input, output), source, limits)
        .await
        .options
        .provider_max_output_tokens
}

fn env(value: Option<u32>) -> RequestLimits {
    RequestLimits {
        max_output_tokens: value,
        ..RequestLimits::default()
    }
}

fn expected(value: u64) -> Option<u32> {
    Some(u32::try_from(value).unwrap())
}

/// 用户的数字：输入估算约 671,000 时交给补丁的是 `窗口 − 估算 − 1000`（约 327,992），不是配置的 384,000。
#[tokio::test]
async fn a_large_input_lowers_the_output_limit_by_the_remaining_window() {
    let used = (671_000, 7);
    let estimated = estimate(used.0, used.1);
    assert_eq!(estimated, 671_008);
    let got = budget(configured(WINDOW), used, QuerySource::MainThread, env(None)).await;
    assert_eq!(got, expected(WINDOW - estimated - 1000));
    assert_eq!(got, Some(327_992));
}

/// 输入很小：余量比配置的上限大，交配置的上限。
#[tokio::test]
async fn a_small_input_keeps_the_configured_limit() {
    let got = budget(
        configured(WINDOW),
        (20_000, 100),
        QuerySource::MainThread,
        env(None),
    )
    .await;
    assert_eq!(got, expected(CONFIGURED_MAX));
}

/// 窗口未知（配置里没有 `contextWindow`，平台层写 0）：交基线；模型端口答不出能力时不给。
#[tokio::test]
async fn an_unknown_window_keeps_the_baseline() {
    let got = budget(
        configured(0),
        (671_000, 7),
        QuerySource::MainThread,
        env(None),
    )
    .await;
    assert_eq!(got, expected(CONFIGURED_MAX));
    let none = budget(None, (671_000, 7), QuerySource::MainThread, env(None)).await;
    assert_eq!(none, None);
}

/// 余量不是正数（估算已到窗口 − 1000）：交基线，由端点报超长、走反应式压缩。
#[tokio::test]
async fn no_room_left_keeps_the_baseline() {
    for input in [WINDOW - 1000 - rough_tokens(NEXT), WINDOW, WINDOW + 50_000] {
        let got = budget(
            configured(WINDOW),
            (input, 0),
            QuerySource::MainThread,
            env(None),
        )
        .await;
        assert_eq!(got, expected(CONFIGURED_MAX), "{input}");
    }
    // 余量正好 1 个 token 时取它。
    let input = WINDOW - 1000 - rough_tokens(NEXT) - 1;
    let got = budget(
        configured(WINDOW),
        (input, 0),
        QuerySource::MainThread,
        env(None),
    )
    .await;
    assert_eq!(got, Some(1));
}

/// `ZCODE_MAX_OUTPUT_TOKENS=32000`：基线是它与配置的上限的较小值；余量更小时取余量。
#[tokio::test]
async fn the_environment_limit_lowers_the_baseline() {
    let got = budget(
        configured(WINDOW),
        (20_000, 100),
        QuerySource::MainThread,
        env(Some(32_000)),
    )
    .await;
    assert_eq!(got, Some(32_000));
    let used = (980_000, 0);
    let got = budget(
        configured(WINDOW),
        used,
        QuerySource::MainThread,
        env(Some(32_000)),
    )
    .await;
    assert_eq!(got, expected(WINDOW - estimate(used.0, used.1) - 1000));
}

/// 子代理的请求同样压低。
#[tokio::test]
async fn subagent_requests_are_lowered_too() {
    let used = (671_000, 7);
    let source = QuerySource::Subagent("general-purpose".to_owned());
    let got = budget(configured(WINDOW), used, source, env(None)).await;
    assert_eq!(got, expected(WINDOW - estimate(used.0, used.1) - 1000));
}

/// 压缩摘要：基线是 `min(配置的上限, 20000)`，再套同样的预检。
#[tokio::test]
async fn summary_requests_take_at_most_20000() {
    let got = budget(
        configured(WINDOW),
        (671_000, 7),
        QuerySource::Compact,
        env(None),
    )
    .await;
    assert_eq!(got, Some(20_000));
    let used = (985_000, 0);
    let got = budget(configured(WINDOW), used, QuerySource::Compact, env(None)).await;
    assert_eq!(got, expected(WINDOW - estimate(used.0, used.1) - 1000));
    // 配置的上限比 20000 小时取配置的。
    let small = Some(ModelCapabilities {
        context_window: WINDOW,
        max_output_tokens: Some(8_192),
        ..ModelCapabilities::default()
    });
    let got = budget(small, (1_000, 0), QuerySource::Compact, env(None)).await;
    assert_eq!(got, Some(8_192));
}

/// 配置没给输出上限时基线是 32000（Node 的缺省）。
#[tokio::test]
async fn a_missing_configured_limit_uses_32000() {
    let unset = Some(ModelCapabilities {
        context_window: WINDOW,
        max_output_tokens: None,
        ..ModelCapabilities::default()
    });
    let got = budget(unset, (1_000, 0), QuerySource::MainThread, env(None)).await;
    assert_eq!(got, Some(32_000));
}

/// 请求体自己的 `max_tokens` 不变：卡片模型照旧发卡片的上限（显式覆盖的端点没有补丁，只看它）。
#[tokio::test]
async fn the_request_max_tokens_itself_is_unchanged() {
    let messages = history(990_000, 0);
    let model = Arc::new(Configured {
        inner: ScriptedModel::new(vec![text_response(&["ok"])]),
        capabilities: configured(WINDOW),
    });
    let mut request = request("unused");
    request.messages = messages;
    request.selection = ModelSelection {
        provider: "anthropic".to_owned(),
        model: "claude-opus-5-5".to_owned(),
        reasoning: None,
        connection: None,
    };
    let (scope, _sink) = scope(parts(model.clone(), Vec::new()), &request, Abort::new());
    super::run(scope, request).await.unwrap();
    let sent = model.inner.requests().remove(0);
    assert_eq!(sent.max_tokens, 128_000);
}
