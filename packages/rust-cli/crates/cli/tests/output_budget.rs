//! 修复线 F108（docs/specs/rust-cli/query.md §1.11、model.md §5.1.1、§9）：目录模型请求的输出上限按窗口余量压低；兼容
//! 端点用 OpenAI 的句式报上下文超长时触发反应式压缩。
//!
//! 在进程内照入口的写法组装内核：真实的平台层（个人 Provider 配置）、模型适配器、主循环、会话 actor、压缩（窗口来自
//! 平台层）与 node-store（临时 HOME 下的 db.sqlite），模型请求发到本地 SSE 夹具。模型是个人 provider 上的
//! deepseek-v4-pro：Anthropic 协议，配置的窗口 1,000,000、输出上限 384,000。
//!
//! 修 bug 前：补丁照配置写 `max_tokens: 384000`，不看输入；端点回的 `This model's maximum context length is …` 归成
//! `InvalidRequest`，不压缩，回合失败。
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
use rust_cli_base::secret::RedactedString;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_kernel_api::compaction::context_tokens;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::history;
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, SessionId};
use rust_cli_kernel_api::message::{Message, SystemMessage};
use rust_cli_kernel_api::model::{ModelPort, ModelSelection};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::{ModelEndpoints, ModelLimits};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent, TurnOutcome,
};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::environments;
use rust_cli_model::ProviderModel;
use rust_cli_net::{Egress, NetworkPolicy};
use rust_cli_node_store::{NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_platform::{AccountOrigins, AccountServices, CredentialSecret, Platform};
use rust_cli_runtime::{CompactConfig, ContextConfig, Defaults, LocalKernel, Ports, Surface};
use serde_json::{Value, json};

/// 配置的窗口与输出上限（用户遇到的模型）。
const WINDOW: u64 = 1_000_000;
const CONFIGURED_MAX: u64 = 384_000;
/// 第一次回复的用量里的输入：之后这一步的估算约 671,000。
const LARGE_INPUT: u64 = 671_000;
/// 端点的原文（400，Anthropic 的错误形状）。
const OVERFLOW: &str = "This model's maximum context length is 1048576 tokens. However, you requested 1055008 tokens (671008 in the messages, 384000 in the completion).";
/// 摘要请求的开头（`prompts/compact/control.txt`）。
const SUMMARY_PROMPT: &str = "Your task is to create a detailed summary of the conversation so far";

/// 一次运行的临时 home 与工作区（工作区里有 `a.txt`，模型用 Read 读它）。
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
        std::fs::write(work.join("a.txt"), "alpha\n").unwrap();
        Self {
            _dir: dir,
            home,
            work,
        }
    }

    fn paths(&self) -> StoragePaths {
        StoragePaths::resolve(&StorageSettings::default(), &self.home, &self.work)
    }

    fn store(&self) -> Arc<NodeStore> {
        let ids = Arc::new(NodeIds::new(Arc::new(SystemClock::utc())));
        Arc::new(NodeStore::new(self.paths(), ids))
    }

    /// 个人 Provider 配置：anthropic-messages 的自定义 provider，地址是夹具；模型的窗口与输出上限由模型规则给出。
    fn provider(&self, url: &str) {
        let config = json!({
            "schemaVersion": 1,
            "config": {
                "providerConfigRules": {"providerRules": [{
                    "providerId": "personal:gateway",
                    "providerName": "Gateway",
                    "enabled": true,
                    "config": {
                        "group": "standard-personal",
                        "access": {"type": "api-key", "apiKey": "sk-test"},
                        "api": {"type": "anthropic-messages", "baseUrl": url},
                        "personalModelIds": ["deepseek-v4-pro"]
                    }
                }]},
                "modelConfigRules": {
                    "providerModelRules": [{
                        "providerId": "personal:gateway",
                        "modelId": "deepseek-v4-pro",
                        "config": {
                            "properties": {"contextWindow": WINDOW},
                            "optionSpecs": {"maxOutputTokens": {"max": CONFIGURED_MAX}}
                        }
                    }],
                    "manualProviderModelRules": []
                }
            }
        });
        let file = self.paths().provider_config();
        std::fs::create_dir_all(file.as_path().parent().unwrap()).unwrap();
        std::fs::write(file.as_path(), config.to_string()).unwrap();
    }

    /// 照入口组装内核：模型端口经平台层取端点（补丁）；压缩的窗口也来自平台层（同 `main.rs` 的 `options::compact`）。
    async fn kernel(&self) -> LocalKernel {
        let clock = Arc::new(SystemClock::utc());
        let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
        let origin = "http://127.0.0.1:9".to_owned();
        let services = AccountServices {
            egress: egress.clone(),
            clock: clock.clone(),
            origins: AccountOrigins {
                zcode_api: format!("{origin}/api/v1"),
                zai_api: origin.clone(),
                bigmodel_team_plans: format!("{origin}/coding-plan/team/plans"),
                bigmodel_api: origin,
                client_version: "0.0.0-test".to_owned(),
            },
        };
        let secret = CredentialSecret {
            explicit: Some(RedactedString::new("test-secret")),
            platform: rust_cli_platform::node_platform().to_owned(),
            home: self.home.to_string_lossy().into_owned(),
            username: "dev".to_owned(),
        };
        let platform = Arc::new(Platform::new(&self.paths(), &secret, services));
        let endpoints: Arc<dyn ModelEndpoints> = platform.clone();
        let model: Arc<dyn ModelPort> =
            Arc::new(ProviderModel::new(egress, endpoints, clock.clone()));
        let ids = Arc::new(NodeIds::new(clock.clone()));
        let store = self.store();
        let ports = Ports {
            model,
            environments: environments(Arc::new(LocalEnvironment::new(Some(self.home.clone())))),
            store: store.clone(),
            artifacts: store.clone(),
            clock,
            ids,
        };
        let defaults = Defaults {
            selection: selection(),
            mode: PermissionMode::BypassPermissions,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        };
        let limits: Arc<dyn ModelLimits> = platform;
        let compact = CompactConfig {
            limits: Some(limits),
            ..CompactConfig::default()
        };
        let context = ContextConfig::new(Surface::Interactive);
        LocalKernel::with_options(ports, defaults, context, compact, store)
    }

    /// 开一个会话、提交一条输入，等回合结束；返回会话与回合的结果。
    async fn one_turn(&self, kernel: &LocalKernel) -> (SessionId, TurnOutcome) {
        let id = kernel
            .create_session(CreateSession {
                id: None,
                workspace: WorkspaceRef {
                    environment: EnvironmentId::new("local"),
                    root: EnvPath::new(self.work.to_string_lossy()),
                },
                selection: selection(),
                mode: PermissionMode::BypassPermissions,
                plan: false,
            })
            .await
            .unwrap();
        let handle = kernel.open_session(&id).await.unwrap();
        let mut events = handle.subscribe(Some(0));
        let ack = handle
            .send(CommandEnvelope {
                id: CommandId::new("cmd_budget"),
                command: Command::Submit {
                    text: "read a.txt".to_owned(),
                    attachments: Vec::new(),
                    delivery: Delivery::Queue,
                },
            })
            .await;
        assert!(!matches!(ack, Ack::Rejected { .. }), "{ack:?}");
        // 期限只是测试的看门狗：等的是回合结束的事件，不靠时间对齐状态。
        let outcome = tokio::time::timeout(Duration::from_secs(60), async {
            while let Some(event) = events.next().await {
                if let KernelEvent::TurnFinished { outcome, .. } = event.event {
                    return outcome;
                }
            }
            panic!("the event stream ended before the turn end");
        })
        .await
        .expect("the turn finishes");
        (id, outcome)
    }

    /// 冷读取（经 node-store 从 db.sqlite 重建）的会话消息。
    async fn stored(&self, session: &SessionId) -> Vec<Message> {
        self.store().load(session).await.unwrap().messages
    }
}

fn selection() -> ModelSelection {
    ModelSelection {
        provider: "personal:gateway".to_owned(),
        model: "deepseek-v4-pro".to_owned(),
        reasoning: None,
        connection: None,
    }
}

/// 第一次回复：读 `a.txt`，用量里的输入是 [`LARGE_INPUT`]。
fn large_read(work: &std::path::Path) -> String {
    let input = json!({"file_path": work.join("a.txt")}).to_string();
    sse::tools_reply_with_usage(
        &[sse::tool_block(0, "toolu_read", "Read", &input)],
        LARGE_INPUT,
    )
}

fn is_summary(body: &Value) -> bool {
    body.to_string().contains(SUMMARY_PROMPT)
}

/// 第二次请求之前的估算：冷读取的历史里最后一条 assistant（第二次回复）之前的那一段，按压缩的同一个函数算。
fn estimate_before_last_reply(messages: &[Message]) -> u64 {
    let last = messages
        .iter()
        .rposition(|message| matches!(message, Message::Assistant(_)))
        .expect("the second reply");
    let active = history::active(&messages[..last]);
    let refs: Vec<&Message> = active.iter().map(AsRef::as_ref).collect();
    context_tokens(&refs)
}

/// 目录模型：输入小的第一次请求发配置的上限；带着约 671,000 的输入的第二次请求发 `窗口 − 估算 − 1000`。修前两次都是
/// 384,000。
#[tokio::test(flavor = "multi_thread")]
async fn a_catalog_request_caps_max_tokens_by_the_remaining_window() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(vec![large_read(&sandbox.work), sse::text_reply("done")]);
    sandbox.provider(&fixture.url);
    let kernel = sandbox.kernel().await;
    let (session, outcome) = sandbox.one_turn(&kernel).await;
    assert_eq!(outcome, TurnOutcome::Success);
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 2, "{requests:#?}");
    assert_eq!(requests[0]["max_tokens"], CONFIGURED_MAX);
    let estimated = estimate_before_last_reply(&sandbox.stored(&session).await);
    assert!(
        estimated > LARGE_INPUT && estimated < LARGE_INPUT + 10_000,
        "{estimated}"
    );
    assert_eq!(requests[1]["max_tokens"], WINDOW - estimated - 1000);
}

/// 端点用 OpenAI 的句式报超长（400）：归上下文超限，反应式压缩（摘要请求的输出上限是 20000），再重发，回合成功；
/// 重发的那一步按压缩后的历史估算，发配置的上限。修前回合以 `InvalidRequest` 失败，没有摘要请求。
#[tokio::test(flavor = "multi_thread")]
async fn an_overflow_in_openai_wording_compacts_and_retries() {
    let sandbox = Sandbox::new();
    let overflow =
        json!({"type": "error", "error": {"type": "invalid_request_error", "message": OVERFLOW}});
    let fixture = sse::serve_with(vec![
        sse::Reply::Fixed(large_read(&sandbox.work)),
        sse::Reply::Status {
            status: 400,
            body: overflow.to_string(),
        },
        sse::Reply::Fixed(sse::text_reply("<summary>Read a.txt.</summary>")),
        sse::Reply::Fixed(sse::text_reply("done")),
    ]);
    sandbox.provider(&fixture.url);
    let kernel = sandbox.kernel().await;
    let (session, outcome) = sandbox.one_turn(&kernel).await;
    assert_eq!(outcome, TurnOutcome::Success);
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 4, "{requests:#?}");
    assert!(!is_summary(&requests[1]));
    assert!(is_summary(&requests[2]), "{:#?}", requests[2]);
    assert_eq!(requests[2]["max_tokens"], 20_000);
    assert!(!is_summary(&requests[3]));
    assert_eq!(requests[3]["max_tokens"], CONFIGURED_MAX);
    let stored = sandbox.stored(&session).await;
    assert!(
        stored
            .iter()
            .any(|message| matches!(message, Message::System(SystemMessage::CompactBoundary(_)))),
        "{stored:#?}"
    );
}
