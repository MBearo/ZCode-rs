//! `/context` 的分析（context-usage.md §2–§4）：经内核 API 调用，各部分的本地估算（工作线 F93 起不发计数）与会话的
//! 用量锚点。模型端口用脚本模型的替身（不连真实服务）；F93 删掉模型端口的计数之后，“不发计数”由类型保证。
use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use rust_cli_base::models::OPUS_5_5;
use rust_cli_kernel_api::context_usage::{ContextUsageQuery, UsageCategory};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelPort, ModelRequest, ModelSelection};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::ModelCapabilities;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent,
};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, environments,
    text_response,
};
use tokio_util::sync::CancellationToken;

use crate::{Defaults, LocalKernel, Ports};

/// 脚本模型加端点的能力（None 是答不出）。工作线 F93 之前它还记录计数请求（`supported` 为假时像 OpenAI 协议一样不支持）。
struct Counting {
    inner: ScriptedModel,
    capabilities: Option<ModelCapabilities>,
}

impl ModelPort for Counting {
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
        async move { found }.boxed()
    }
}

/// Anthropic 的有卡片的模型：窗口按卡片是 1,000,000（下面的窗口与缓冲按它算；卡片的窗口只给有卡片的模型）。
fn carded() -> ModelSelection {
    ModelSelection {
        provider: "anthropic".to_owned(),
        model: OPUS_5_5.to_owned(),
        reasoning: None,
        connection: None,
    }
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/project"),
    }
}

fn setup(model: Arc<Counting>) -> LocalKernel {
    let store = Arc::new(MemoryStore::default());
    LocalKernel::new(
        Ports {
            model,
            environments: environments(Arc::new(UnusedEnvironment)),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: carded(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        },
    )
}

fn counting(responses: Vec<Vec<Result<ModelEvent, ModelError>>>) -> Arc<Counting> {
    Arc::new(Counting {
        inner: ScriptedModel::new(responses),
        capabilities: None,
    })
}

fn query(session: Option<rust_cli_kernel_api::ids::SessionId>) -> ContextUsageQuery {
    ContextUsageQuery {
        session,
        workspace: workspace(),
        selection: carded(),
        mode: PermissionMode::Default,
    }
}

fn find<'a>(categories: &'a [UsageCategory], name: &str) -> Option<&'a UsageCategory> {
    categories.iter().find(|category| category.name == name)
}

/// 工作线 F93（context-usage.md §3、D13，用户决定 2026-10-24）：`/context` 不发 count_tokens，各部分是本地估算（同一份
/// 输入两次结果相同，不靠缓存）。修改前每部分各发一次计数（MCP 与消息各一次 `foo`、Skill、列出的与每个延迟的内置工具、
/// system prompt 的每段），数字取计数接口的结果；那时这里的替身答得出计数、并断言一次也没有调用（删掉模型端口的计数之后
/// 由类型保证）。
#[tokio::test]
async fn context_usage_sends_no_count_requests() {
    let model = counting(Vec::new());
    let kernel = setup(model);
    let usage = kernel.context_usage(query(None)).await.unwrap();
    for name in ["System prompt", "System tools", "System tools (deferred)"] {
        assert!(find(&usage.categories, name).is_some(), "{name}");
    }
    // 消息为空：估算为 0，不列出（修改前取计数接口给 `foo` 的数）。
    assert!(find(&usage.categories, "Messages").is_none());
    assert_eq!(usage.raw_max_tokens, 1_000_000);
    assert_eq!(
        find(&usage.categories, "Autocompact buffer")
            .unwrap()
            .tokens,
        33_000
    );
    let again = kernel.context_usage(query(None)).await.unwrap();
    assert_eq!(again, usage);
}

/// 会话里有带用量的响应：消息按锚点测量，不再计数消息。
#[tokio::test]
async fn a_session_with_usage_measures_messages_from_the_anchor() {
    let model = counting(vec![text_response(&["hi"])]);
    let kernel = setup(model.clone());
    let id = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: carded(),
            mode: PermissionMode::Default,
            plan: false,
        })
        .await
        .unwrap();
    let session = kernel.open_session(&id).await.unwrap();
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("submit"),
            command: Command::Submit {
                text: "hello".to_owned(),
                attachments: Vec::new(),
                delivery: Delivery::Queue,
            },
        })
        .await;
    while let Some(event) = futures_util::StreamExt::next(&mut events).await {
        if matches!(event.event, KernelEvent::TurnFinished { .. }) {
            break;
        }
    }
    let usage = kernel.context_usage(query(Some(id))).await.unwrap();
    assert!(usage.total_tokens > 0);
    assert!(find(&usage.categories, "Free space").is_some());
}
