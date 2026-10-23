//! `/context` 的分析（context-usage.md §2–§4）：经内核 API 调用，计数请求的集合、缓存、估算回退与会话的
//! 用量锚点。模型端口用记录计数请求的替身（不连真实服务）。
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::context_usage::{ContextUsageQuery, UsageCategory};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
use rust_cli_kernel_api::model::{
    CountTokensRequest, ModelError, ModelEvent, ModelPort, ModelRequest, ModelSelection,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent,
};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, text_response,
};
use tokio_util::sync::CancellationToken;

use crate::{Defaults, LocalKernel, Ports};

/// 记录计数请求；`supported` 为假时像 OpenAI 协议一样不支持计数。
struct Counting {
    inner: ScriptedModel,
    counts: Mutex<Vec<CountTokensRequest>>,
    supported: bool,
}

impl ModelPort for Counting {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        self.inner.stream(request, cancel)
    }

    fn count_tokens(
        &self,
        request: CountTokensRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<u64, ModelError>> {
        let tokens = serde_json::to_string(&request).unwrap().len() as u64 / 4;
        self.counts.lock().unwrap().push(request);
        let supported = self.supported;
        async move {
            if supported {
                Ok(tokens)
            } else {
                Err(ModelError::Unsupported {
                    what: "count_tokens".to_owned(),
                })
            }
        }
        .boxed()
    }
}

fn opus() -> ModelSelection {
    ModelSelection {
        provider: "anthropic".to_owned(),
        model: "claude-opus-5-5".to_owned(),
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
            env: Arc::new(UnusedEnvironment),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: opus(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        },
    )
}

fn counting(supported: bool, responses: Vec<Vec<Result<ModelEvent, ModelError>>>) -> Arc<Counting> {
    Arc::new(Counting {
        inner: ScriptedModel::new(responses),
        counts: Mutex::default(),
        supported,
    })
}

fn query(session: Option<rust_cli_kernel_api::ids::SessionId>) -> ContextUsageQuery {
    ContextUsageQuery {
        session,
        workspace: workspace(),
        selection: opus(),
        mode: PermissionMode::Default,
    }
}

fn find<'a>(categories: &'a [UsageCategory], name: &str) -> Option<&'a UsageCategory> {
    categories.iter().find(|category| category.name == name)
}

fn tool_names(request: &CountTokensRequest) -> Vec<&str> {
    request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect()
}

#[tokio::test]
async fn a_new_session_counts_each_part_once_and_caches() {
    let model = counting(true, Vec::new());
    let kernel = setup(model.clone());
    let usage = kernel.context_usage(query(None)).await.unwrap();
    let counts = model.counts.lock().unwrap().clone();
    // MCP（没有 MCP 工具也发）与消息（没有消息）各一次 `foo`：没有消息、没有工具。
    let empty = counts
        .iter()
        .filter(|request| request.messages.is_empty() && request.tools.is_empty())
        .count();
    assert_eq!(empty, 2);
    assert!(counts[0].messages.is_empty() && counts[0].tools.is_empty());
    // Skill 单独一次；延迟的内置工具各一次；列出的内置工具一起一次（不含占位工具）。
    assert!(
        counts
            .iter()
            .any(|request| tool_names(request) == ["Skill"])
    );
    let singles: Vec<&str> = counts
        .iter()
        .filter(|request| request.tools.len() == 1)
        .map(|request| request.tools[0].name.as_str())
        .collect();
    assert!(singles.contains(&"CronCreate"), "{singles:?}");
    assert!(singles.contains(&"WebFetch"), "{singles:?}");
    let listed = counts
        .iter()
        .find(|request| request.tools.len() > 1)
        .unwrap();
    assert!(tool_names(listed).contains(&"ToolSearch"));
    assert!(!tool_names(listed).contains(&"CronCreate"));
    assert!(!tool_names(listed).contains(&"DeferredToolPlaceholder"));
    // system prompt 的每段一次：一条 user 消息、没有工具。
    let sections = counts
        .iter()
        .filter(|request| request.messages.len() == 1 && request.tools.is_empty())
        .count();
    assert!(sections >= 8, "{sections}");
    // 修复线 F51（context.md §6、context-usage.md D12）：没有环境段，也不计数。
    let sent = serde_json::to_string(&counts).unwrap();
    assert!(!sent.contains("# Environment"), "{sent}");
    for name in [
        "System prompt",
        "System tools",
        "System tools (deferred)",
        "Messages",
    ] {
        assert!(find(&usage.categories, name).is_some(), "{name}");
    }
    assert_eq!(usage.raw_max_tokens, 1_000_000);
    assert_eq!(
        find(&usage.categories, "Autocompact buffer")
            .unwrap()
            .tokens,
        33_000
    );
    // 再执行一次：只有消息的计数重新发出（其余命中缓存）。
    let before = counts.len();
    let again = kernel.context_usage(query(None)).await.unwrap();
    assert_eq!(model.counts.lock().unwrap().len(), before + 1);
    assert_eq!(again, usage);
}

#[tokio::test]
async fn without_count_tokens_the_parts_are_estimated() {
    let model = counting(false, Vec::new());
    let kernel = setup(model);
    let usage = kernel.context_usage(query(None)).await.unwrap();
    let prompt = find(&usage.categories, "System prompt").unwrap();
    assert!(prompt.tokens > 500, "{prompt:?}");
    // 消息为空：估算为 0，不列出。
    assert!(find(&usage.categories, "Messages").is_none());
}

/// 会话里有带用量的响应：消息按锚点测量，不再计数消息。
#[tokio::test]
async fn a_session_with_usage_measures_messages_from_the_anchor() {
    let model = counting(true, vec![text_response(&["hi"])]);
    let kernel = setup(model.clone());
    let id = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: opus(),
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
    let counts = model.counts.lock().unwrap().clone();
    // 没有消息计数：只有一次 `foo`（MCP）。
    let empty = counts
        .iter()
        .filter(|request| request.messages.is_empty() && request.tools.is_empty())
        .count();
    assert_eq!(empty, 1);
    assert!(usage.total_tokens > 0);
    assert!(find(&usage.categories, "Free space").is_some());
}
