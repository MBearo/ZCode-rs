//! 修复线 F113（docs/specs/rust-cli/provider-errors.md §8.3）：厂商错误的分类与处置经真实的平台层（个人 Provider 配置）、
//! 模型适配器、主循环、会话 actor 与 node-store（临时 HOME 下的 db.sqlite），模型请求发到本地 SSE 夹具。
//!
//! 夹具在 127.0.0.1，按只给测试的声明（`with_assumed_base_url`，D13）把它当成智谱的地址，走到智谱的码表；不带声明时是
//! 通用的协议层。每个场景断言请求次数、回合的结局与库里的消息。另有一条真实进程的 `-p`：声明经环境变量
//! `_ZCODE_ASSUME_BASE_URL` 进来，退出码与输出。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、读回会话"
)]

mod sse;
mod system_env;
use system_env::KeepSystem as _;

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_base::secret::RedactedString;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, Message, SystemMessage};
use rust_cli_kernel_api::model::{CONTENT_BLOCKED, ModelPort, ModelSelection};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::{ModelEndpoints, ModelLimits};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent, TurnOutcome,
};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::environments;
use rust_cli_model::{ProviderModel, RetryPolicy};
use rust_cli_net::{Egress, NetworkPolicy};
use rust_cli_node_store::{NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_platform::{AccountOrigins, AccountServices, CredentialSecret, Platform};
use rust_cli_runtime::{CompactConfig, ContextConfig, Defaults, LocalKernel, Ports, Surface};
use serde_json::{Value, json};

/// 智谱 Anthropic 协议的地址（只用来查方案表与厂商，请求仍发到夹具）。
const ZHIPU: &str = "https://open.bigmodel.cn/api/anthropic";
/// 摘要请求的开头（`prompts/compact/control.txt`）。
const SUMMARY_PROMPT: &str = "Your task is to create a detailed summary of the conversation so far";

/// 端点的协议（个人 Provider 配置的 `api.type`）。
#[derive(Clone, Copy)]
enum Api {
    Anthropic,
    OpenAiChat,
}

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

    /// 个人 Provider 配置：一个自定义 provider，地址是夹具，模型 `glm-5`。
    fn provider(&self, url: &str, api: Api) {
        let kind = match api {
            Api::Anthropic => "anthropic-messages",
            Api::OpenAiChat => "openai-chat-completions",
        };
        let config = json!({
            "schemaVersion": 1,
            "config": {
                "providerConfigRules": {"providerRules": [{
                    "providerId": "personal:glm",
                    "providerName": "GLM",
                    "enabled": true,
                    "config": {
                        "group": "standard-personal",
                        "access": {"type": "api-key", "apiKey": "sk-test"},
                        "api": {"type": kind, "baseUrl": url},
                        "personalModelIds": ["glm-5"]
                    }
                }]},
                "modelConfigRules": {
                    "providerModelRules": [{
                        "providerId": "personal:glm",
                        "modelId": "glm-5",
                        "config": {"properties": {"contextWindow": 200_000}}
                    }],
                    "manualProviderModelRules": []
                }
            }
        });
        let file = self.paths().provider_config();
        std::fs::create_dir_all(file.as_path().parent().unwrap()).unwrap();
        std::fs::write(file.as_path(), config.to_string()).unwrap();
    }

    /// 照入口组装内核；`assumed` 是只给测试的声明（按这个地址查方案表与厂商）。重试的等待调到几毫秒。
    async fn kernel(&self, assumed: Option<&str>) -> LocalKernel {
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
        let retry = RetryPolicy {
            base_delay: Duration::from_millis(1),
            max_backoff: Duration::from_millis(4),
            jitter_ratio: 0.0,
            ..RetryPolicy::default()
        };
        let mut model = ProviderModel::new(egress, endpoints, clock.clone()).with_retry(retry);
        if let Some(base_url) = assumed {
            model = model.with_assumed_base_url(base_url);
        }
        let model: Arc<dyn ModelPort> = Arc::new(model);
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
                id: CommandId::new("cmd_errors"),
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
        provider: "personal:glm".to_owned(),
        model: "glm-5".to_owned(),
        reasoning: None,
        connection: None,
    }
}

/// 库里 assistant 消息的文字。
fn assistant_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => Some(
                assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

fn is_summary(body: &Value) -> bool {
    body.to_string().contains(SUMMARY_PROMPT)
}

/// 智谱的错误正文。
fn coded(status: u16, code: &str, message: &str) -> sse::Reply {
    sse::Reply::Status {
        status,
        body: json!({"error": {"code": code, "message": message}}).to_string(),
    }
}

/// 智谱的超长码（400 + 1261）：反应式压缩、重发、回合成功（以前码不认识，归参数错误，回合失败）。
#[tokio::test(flavor = "multi_thread")]
async fn a_zhipu_overflow_code_compacts_and_retries() {
    let sandbox = Sandbox::new();
    let input = json!({"file_path": sandbox.work.join("a.txt")}).to_string();
    let fixture = sse::serve_with(vec![
        sse::Reply::Fixed(sse::tools_reply(&[sse::tool_block(
            0,
            "toolu_read",
            "Read",
            &input,
        )])),
        coded(400, "1261", "Prompt exceeds max length"),
        sse::Reply::Fixed(sse::text_reply("<summary>Read a.txt.</summary>")),
        sse::Reply::Fixed(sse::text_reply("done")),
    ]);
    sandbox.provider(&fixture.url, Api::Anthropic);
    let kernel = sandbox.kernel(Some(ZHIPU)).await;
    let (session, outcome) = sandbox.one_turn(&kernel).await;
    assert_eq!(outcome, TurnOutcome::Success);
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 4, "{requests:#?}");
    assert!(is_summary(&requests[2]));
    let stored = sandbox.stored(&session).await;
    assert!(
        stored
            .iter()
            .any(|message| matches!(message, Message::System(SystemMessage::CompactBoundary(_))))
    );
}

/// 智谱的限流码在 HTTP 200 的流里、还没有内容：重发，第二次成功；库里只有第二次的回复（以前回合以错误结束）。
#[tokio::test(flavor = "multi_thread")]
async fn a_zhipu_rate_limit_in_the_stream_is_resent() {
    let sandbox = Sandbox::new();
    let limited = sse::reply_of(&[], "end_turn");
    let start = limited
        .split("event: message_delta")
        .next()
        .unwrap()
        .to_owned();
    let limited = format!(
        "{start}data: {}\n\n",
        json!({"error": {"code": "1302", "message": "并发数过高"}})
    );
    let fixture = sse::serve(vec![limited, sse::text_reply("hello")]);
    sandbox.provider(&fixture.url, Api::Anthropic);
    let kernel = sandbox.kernel(Some(ZHIPU)).await;
    let (session, outcome) = sandbox.one_turn(&kernel).await;
    assert_eq!(outcome, TurnOutcome::Success);
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["messages"], requests[1]["messages"]);
    assert_eq!(assistant_texts(&sandbox.stored(&session).await), ["hello"]);
}

/// 智谱的配额码（429 + 1308）：不重试、不压缩；回合失败，文字是配额用尽加端点原文（以前按 429 重试十次）。不带声明的
/// 同一个端点按 429 重试。
#[tokio::test(flavor = "multi_thread")]
async fn a_zhipu_quota_code_ends_the_turn_at_once() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve_with(vec![coded(429, "1308", "已达到套餐用量上限")]);
    sandbox.provider(&fixture.url, Api::Anthropic);
    let kernel = sandbox.kernel(Some(ZHIPU)).await;
    let (_, outcome) = sandbox.one_turn(&kernel).await;
    assert_eq!(
        outcome,
        TurnOutcome::Failed {
            message: "API Error: Credit balance or quota exhausted · 已达到套餐用量上限".to_owned()
        }
    );
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 1);
    assert!(!requests.iter().any(is_summary));

    let generic = Sandbox::new();
    let fixture = sse::serve_with(vec![
        coded(429, "1308", "已达到套餐用量上限"),
        sse::Reply::Fixed(sse::text_reply("hello")),
    ]);
    generic.provider(&fixture.url, Api::Anthropic);
    let kernel = generic.kernel(None).await;
    let (_, outcome) = generic.one_turn(&kernel).await;
    assert_eq!(outcome, TurnOutcome::Success);
    assert_eq!(fixture.main_requests().len(), 2);
}

/// 智谱的 `sensitive`，已流出一段文字：文字照常提交，回合以错误结束、给原因文字（已定；以前静默按成功结束）。
#[tokio::test(flavor = "multi_thread")]
async fn zhipu_sensitive_keeps_the_text_and_fails_with_the_reason() {
    let sandbox = Sandbox::new();
    let text = sse::text_reply("Partial answer");
    let blocked = text.replace(
        r#""stop_reason":"end_turn""#,
        r#""stop_reason":"sensitive""#,
    );
    assert_ne!(text, blocked);
    let fixture = sse::serve(vec![blocked]);
    sandbox.provider(&fixture.url, Api::Anthropic);
    let kernel = sandbox.kernel(Some(ZHIPU)).await;
    let (session, outcome) = sandbox.one_turn(&kernel).await;
    assert_eq!(
        outcome,
        TurnOutcome::Failed {
            message: CONTENT_BLOCKED.to_owned()
        }
    );
    assert_eq!(fixture.main_requests().len(), 1);
    assert_eq!(
        assistant_texts(&sandbox.stored(&session).await),
        ["Partial answer"]
    );
}

/// OpenAI 协议的 Chat 流：一段文字与结束原因。
fn chat_reply(text: &str, finish: &str) -> String {
    let chunks = [
        json!({"id": "c1", "model": "glm-5", "choices": [{"index": 0, "delta": {"content": text}}]}),
        json!({"id": "c1", "model": "glm-5", "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}),
    ];
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    body
}

/// OpenAI 协议的通用端点：`insufficient_quota` 不重试（以前按 429 重试十次）；`content_filter` 以错误结束、给原因文字，
/// 已流出的文字提交（以前静默按成功结束）。
#[tokio::test(flavor = "multi_thread")]
async fn openai_quota_and_content_filter() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve_with(vec![sse::Reply::Status {
        status: 429,
        body: json!({"error": {"message": "You exceeded your current quota", "type": "insufficient_quota",
            "code": "insufficient_quota"}})
        .to_string(),
    }]);
    sandbox.provider(&fixture.url, Api::OpenAiChat);
    let kernel = sandbox.kernel(None).await;
    let (_, outcome) = sandbox.one_turn(&kernel).await;
    assert_eq!(
        outcome,
        TurnOutcome::Failed {
            message:
                "API Error: Credit balance or quota exhausted · You exceeded your current quota"
                    .to_owned()
        }
    );
    assert_eq!(fixture.main_requests().len(), 1);

    let sandbox = Sandbox::new();
    let fixture = sse::serve(vec![chat_reply("Some text", "content_filter")]);
    sandbox.provider(&fixture.url, Api::OpenAiChat);
    let kernel = sandbox.kernel(None).await;
    let (session, outcome) = sandbox.one_turn(&kernel).await;
    assert_eq!(
        outcome,
        TurnOutcome::Failed {
            message: CONTENT_BLOCKED.to_owned()
        }
    );
    assert_eq!(fixture.main_requests().len(), 1);
    assert_eq!(
        assistant_texts(&sandbox.stored(&session).await),
        ["Some text"]
    );
}

/// 真实进程：`-p` 经显式覆盖连夹具，只给测试的 `_ZCODE_ASSUME_BASE_URL` 让它按智谱的码表分类；配额码不重试，退出码 1，
/// 输出是配额用尽的说明。release 构建不读测试声明（model.md §6），只在 debug 构建跑。
#[cfg(debug_assertions)]
#[test]
fn print_mode_reads_the_assumed_base_url() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve_with(vec![coded(429, "1308", "已达到套餐用量上限")]);
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(["-p", "hi"])
        .current_dir(&sandbox.work)
        .env_clear()
        .keep_system()
        .env("HOME", &sandbox.home)
        .env("USERPROFILE", &sandbox.home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &sandbox.home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "glm-5")
        .env("_ZCODE_ASSUME_BASE_URL", ZHIPU)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("API Error: Credit balance or quota exhausted · 已达到套餐用量上限"),
        "{stdout}"
    );
    assert_eq!(fixture.main_requests().len(), 1);
}
