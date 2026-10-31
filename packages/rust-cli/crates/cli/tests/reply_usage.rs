//! 修复线 F99（docs/specs/rust-cli/query.md §1.10）：回复的输入与缓存用量只在结束时给出的端点，一回合之后存储与上下文
//! 用量都带上它们。
//!
//! 在进程内照入口的写法组装内核：真实的模型适配器（解码线上的流）、主循环、会话 actor 与 node-store（临时 HOME 下的
//! db.sqlite），模型请求发到本地 SSE 夹具。两种端点：
//!
//! - Anthropic 兼容端点（GLM 的写法）：`message_start` 的用量是 0，真实的输入与缓存只在 `message_delta` 里；
//! - OpenAI Chat：用量只在最后一个 chunk 里。
//!
//! 修 bug 前组装器只取 `MessageDelta` 的输出：存储里 assistant 的 `tokens.input` 与用量行的输入是 0，上下文用量约等于
//! 输出（footer 的“上下文 N%”一直 0%）。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、读回会话"
)]

mod sse;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_base::models::GLM_5_3_FLASH;
use rust_cli_base::secret::RedactedString;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, SessionId};
use rust_cli_kernel_api::message::{Message, Usage};
use rust_cli_kernel_api::metering::{ActivityQuery, ActivityRange, ModelTokens};
use rust_cli_kernel_api::model::{ModelPort, ModelSelection};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint, WireProtocol};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent,
};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::environments;
use rust_cli_model::{AnthropicEndpoint, FixedEndpoint, ProviderModel};
use rust_cli_net::{Egress, NetworkPolicy};
use rust_cli_node_store::{NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_runtime::{Defaults, LocalKernel, Ports};
use serde_json::json;

/// GLM 的一次回复：`message_start` 的用量是 0，`message_delta` 给真实的输入、缓存与输出。
fn glm_reply() -> String {
    [
        sse::raw_event(
            "message_start",
            json!({"message": {"id": "msg_glm", "type": "message", "role": "assistant",
                "model": GLM_5_3_FLASH, "content": [], "stop_reason": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}}}),
        ),
        sse::raw_event(
            "content_block_start",
            json!({"index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        sse::raw_event(
            "content_block_delta",
            json!({"index": 0, "delta": {"type": "text_delta", "text": "Done."}}),
        ),
        sse::raw_event("content_block_stop", json!({"index": 0})),
        sse::raw_event(
            "message_delta",
            json!({"delta": {"stop_reason": "end_turn"},
                "usage": {"input_tokens": 11_922, "output_tokens": 514, "cache_read_input_tokens": 448}}),
        ),
        sse::raw_event("message_stop", json!({})),
    ]
    .concat()
}

/// OpenAI Chat 的一次回复：正文、停止原因，最后一个 chunk 给用量（输入 1200，其中缓存命中 200）。
fn openai_reply() -> String {
    let chunk = |value: serde_json::Value| format!("data: {value}\n\n");
    [
        chunk(json!({"id": "chatcmpl_1", "object": "chat.completion.chunk", "model": "gpt-test",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Done."}, "finish_reason": null}]})),
        chunk(json!({"id": "chatcmpl_1", "object": "chat.completion.chunk", "model": "gpt-test",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})),
        chunk(json!({"id": "chatcmpl_1", "object": "chat.completion.chunk", "model": "gpt-test",
            "choices": [], "usage": {"prompt_tokens": 1_200, "completion_tokens": 30, "total_tokens": 1_230,
                "prompt_tokens_details": {"cached_tokens": 200}}})),
        "data: [DONE]\n\n".to_owned(),
    ]
    .concat()
}

/// 一次运行的临时 home 与工作区。
struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let (home, work) = (base.join("home"), base.join("work"));
        std::fs::create_dir_all(work.join(".git")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        Self {
            _dir: dir,
            home,
            work,
        }
    }

    fn store(&self) -> Arc<NodeStore> {
        let paths = StoragePaths::resolve(&StorageSettings::default(), &self.home, &self.work);
        let ids = Arc::new(NodeIds::new(Arc::new(SystemClock::utc())));
        Arc::new(NodeStore::new(paths, ids))
    }

    /// 照入口组装内核：模型适配器连夹具（固定端点），存储是临时 HOME 下的 node-store。
    async fn kernel(&self, endpoint: ModelEndpoint, selection: &ModelSelection) -> LocalKernel {
        let clock = Arc::new(SystemClock::utc());
        let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
        let model: Arc<dyn ModelPort> = Arc::new(ProviderModel::new(
            egress,
            Arc::new(FixedEndpoint::new(endpoint)),
            clock.clone(),
        ));
        let ids = Arc::new(NodeIds::new(clock.clone()));
        let store = self.store();
        let ports = Ports {
            model,
            environments: environments(Arc::new(LocalEnvironment::new(Some(self.home.clone())))),
            store: store.clone(),
            artifacts: store,
            clock,
            ids,
        };
        let defaults = Defaults {
            selection: selection.clone(),
            mode: PermissionMode::BypassPermissions,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        };
        LocalKernel::new(ports, defaults)
    }

    /// 开一个会话、提交一条输入，等回合结束；返回会话与回合里最后一次的上下文用量（`ContextUsage.used_tokens`）。
    async fn one_turn(&self, kernel: &LocalKernel, selection: ModelSelection) -> (SessionId, u64) {
        let id = kernel
            .create_session(CreateSession {
                id: None,
                workspace: WorkspaceRef {
                    environment: EnvironmentId::new("local"),
                    root: EnvPath::new(self.work.to_string_lossy()),
                },
                selection,
                mode: PermissionMode::BypassPermissions,
                plan: false,
            })
            .await
            .unwrap();
        let handle = kernel.open_session(&id).await.unwrap();
        let mut events = handle.subscribe(Some(0));
        let ack = handle
            .send(CommandEnvelope {
                id: CommandId::new("cmd_usage"),
                command: Command::Submit {
                    text: "summarize the notes".to_owned(),
                    attachments: Vec::new(),
                    delivery: Delivery::Queue,
                },
            })
            .await;
        assert!(!matches!(ack, Ack::Rejected { .. }), "{ack:?}");
        // 期限只是测试的看门狗：等的是回合结束的事件，不靠时间对齐状态。
        let used = tokio::time::timeout(Duration::from_secs(60), async {
            let mut used = None;
            while let Some(event) = events.next().await {
                match event.event {
                    KernelEvent::ContextUsage { used_tokens, .. } => used = Some(used_tokens),
                    KernelEvent::TurnFinished { .. } => return used,
                    _ => {}
                }
            }
            panic!("the event stream ended before the turn end");
        })
        .await
        .expect("the turn finishes")
        .expect("a context usage after the reply");
        (id, used)
    }

    /// 冷读取（经 node-store 从 db.sqlite 重建）的最后一条 assistant 的用量。存储里是 Node 的 `tokens`
    /// （`input` 含缓存读写，`cache.read` / `cache.write` 另记），冷读取拆回不含缓存的 `input_tokens`。
    async fn stored_usage(&self, session: &SessionId) -> Option<Usage> {
        let loaded = self.store().load(session).await.unwrap();
        loaded
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Assistant(assistant) => Some(assistant.usage),
                _ => None,
            })
            .expect("an assistant message")
    }

    /// 用量表（`model_usage`）里主回合请求按模型的 token（`input` 由行的总输入减去缓存读写得出）。
    async fn usage_rows(&self) -> Vec<ModelTokens> {
        self.store()
            .activity(ActivityQuery {
                range: ActivityRange::All,
                today: "2026-10-26".to_owned(),
                utc_offset_minutes: 0,
            })
            .await
            .unwrap()
            .models
    }
}

fn selection(provider: &str, model: &str) -> ModelSelection {
    ModelSelection {
        provider: provider.to_owned(),
        model: model.to_owned(),
        reasoning: None,
        connection: None,
    }
}

/// Anthropic 兼容端点（GLM）：输入与缓存只在 `message_delta` 里。存储里 assistant 的 `tokens`（Node 形状：输入
/// 11922 + 448 = 12370、`cache.read` 448）与用量行带上它们；上下文用量 = 输入 + 缓存 + 输出。
#[tokio::test(flavor = "multi_thread")]
async fn input_and_cache_reported_in_the_message_delta_are_kept() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(vec![glm_reply()]);
    let endpoint = AnthropicEndpoint {
        base_url: fixture.url.clone(),
        api_key: RedactedString::new("sk-test"),
    };
    let selection = selection("anthropic", GLM_5_3_FLASH);
    let kernel = sandbox.kernel(endpoint.into(), &selection).await;
    let (session, used) = sandbox.one_turn(&kernel, selection).await;
    assert_eq!(fixture.main_requests().len(), 1);
    let reported = Usage {
        input_tokens: 11_922,
        output_tokens: 514,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 448,
    };
    assert_eq!(sandbox.stored_usage(&session).await, Some(reported));
    assert_eq!(
        sandbox.usage_rows().await,
        [ModelTokens {
            model: GLM_5_3_FLASH.to_owned(),
            input: 11_922,
            output: 514,
            cache_read: 448,
            cache_write: 0,
        }]
    );
    // 回复之后没有别的消息：上下文用量就是这次回复的输入 + 缓存 + 输出。
    assert_eq!(used, 11_922 + 448 + 514);
}

/// OpenAI Chat：用量只在最后一个 chunk 里，同样进存储、用量行与上下文用量（输入换成不含缓存命中的口径）。
#[tokio::test(flavor = "multi_thread")]
async fn openai_usage_reported_at_the_end_is_kept() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(vec![openai_reply()]);
    let endpoint = ModelEndpoint {
        protocol: WireProtocol::OpenAiChatCompletions,
        base_url: fixture.url.clone(),
        model: String::new(),
        credential: Credential::ApiKey(RedactedString::new("sk-test")),
        headers: Vec::new(),
        body_patches: Vec::new(),
        mid_conversation_system: true,
        supports_image: true,
        supports_pdf: false,
        supports_video: false,
        supports_audio: false,
        renewable: false,
    };
    let selection = selection("openai-test", "gpt-test");
    let kernel = sandbox.kernel(endpoint, &selection).await;
    let (session, used) = sandbox.one_turn(&kernel, selection).await;
    assert_eq!(fixture.main_requests().len(), 1);
    let reported = Usage {
        input_tokens: 1_000,
        output_tokens: 30,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 200,
    };
    assert_eq!(sandbox.stored_usage(&session).await, Some(reported));
    assert_eq!(
        sandbox.usage_rows().await,
        [ModelTokens {
            model: "gpt-test".to_owned(),
            input: 1_000,
            output: 30,
            cache_read: 200,
            cache_write: 0,
        }]
    );
    assert_eq!(used, 1_000 + 200 + 30);
}
