//! 工作线 F97（docs/specs/rust-cli/titles.md §3、model.md §9）：自动标题请求的 `max_tokens` 在目录模型上也是 Node 的
//! `min(5000, 上限)`。
//!
//! `-p` 不生成标题（titles.md §2），所以不跑二进制：在进程内照入口的写法组装打开标题的内核——真实的平台层（个人
//! Provider 配置）、模型适配器与会话 actor，模型请求发到本地 SSE 夹具，看线上的请求体。两种模型来源都看：
//!
//! - 目录模型：个人 provider 上的 glm-5.3（内置规则给 `maxOutputTokens.max: 128000`，补丁 `{'max_tokens': …}`）。
//!   修前标题请求不把自己的上限交给补丁，补丁按配置的上限把 5000 改写成 128000。
//! - 显式覆盖（固定端点，没有补丁）：标题请求原样发 5000，F97 前后相同。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具"
)]

mod sse;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_base::models::GLM_5_3;
use rust_cli_base::secret::RedactedString;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
use rust_cli_kernel_api::model::{ModelPort, ModelSelection};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::{ModelDirectory, ModelEndpoints};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent,
};
use rust_cli_kernel_api::testing::environments;
use rust_cli_model::{AnthropicEndpoint, FixedEndpoint, ProviderModel};
use rust_cli_net::{Egress, NetworkPolicy};
use rust_cli_node_store::{NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_platform::{AccountOrigins, AccountServices, CredentialSecret, Platform};
use rust_cli_runtime::{Defaults, LocalKernel, Ports, TitleModel};
use serde_json::{Value, json};

/// 夹具对旁路请求回的固定文字：生成的标题就是它（titles.md §3 的解析）。
const GENERATED: &str = "Side";

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

    fn paths(&self) -> StoragePaths {
        StoragePaths::resolve(&StorageSettings::default(), &self.home, &self.work)
    }

    /// 个人 Provider 配置：一个 anthropic-messages 的自定义 provider，地址是夹具（同 `model_limits.rs`）。
    fn provider(&self, url: &str, model: &str) {
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
                        "personalModelIds": [model]
                    }
                }]},
                "modelConfigRules": {"providerModelRules": [], "manualProviderModelRules": []}
            }
        });
        let file = self.paths().provider_config();
        std::fs::create_dir_all(file.as_path().parent().unwrap()).unwrap();
        std::fs::write(file.as_path(), config.to_string()).unwrap();
    }

    /// 照入口（cli `main.rs`）组装打开标题的内核：模型端口与标题用同一个适配器，目录模型时标题另带平台层的目录。
    fn kernel(
        &self,
        model: Arc<dyn ModelPort>,
        directory: Option<Arc<dyn ModelDirectory>>,
        selection: &ModelSelection,
        clock: Arc<SystemClock>,
    ) -> LocalKernel {
        let ids = Arc::new(NodeIds::new(clock.clone()));
        let store = Arc::new(NodeStore::new(self.paths(), ids.clone()));
        let ports = Ports {
            model: model.clone(),
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
        LocalKernel::new(ports, defaults).with_titles(TitleModel { model, directory })
    }

    /// 开一个交互会话、提交首条输入，等到生成的标题与回合结束（标题请求与主请求都已回复，夹具记下了它们；两者并发，
    /// 先后不定）。
    async fn first_input(&self, kernel: &LocalKernel, selection: ModelSelection) {
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
                id: CommandId::new("cmd_title"),
                command: Command::Submit {
                    text: "read the provider configuration files".to_owned(),
                    attachments: Vec::new(),
                    delivery: Delivery::Queue,
                },
            })
            .await;
        assert!(!matches!(ack, Ack::Rejected { .. }), "{ack:?}");
        // 期限只是测试的看门狗：等的是两个事件，不靠时间对齐状态。
        tokio::time::timeout(Duration::from_secs(60), async {
            let (mut titled, mut finished) = (false, false);
            while let Some(event) = events.next().await {
                match &event.event {
                    KernelEvent::TitleChanged { title } if title == GENERATED => titled = true,
                    KernelEvent::TurnFinished { .. } => finished = true,
                    _ => {}
                }
                if titled && finished {
                    return;
                }
            }
            panic!("the event stream ended before the generated title and the turn end");
        })
        .await
        .expect("the title request and the turn are answered");
    }
}

/// 不联网的账号服务：api-key 的 provider 不查账号，地址不会被访问。
async fn services(clock: Arc<dyn Clock>) -> (Arc<Egress>, AccountServices) {
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let origin = "http://127.0.0.1:9".to_owned();
    let services = AccountServices {
        egress: egress.clone(),
        clock,
        origins: AccountOrigins {
            zcode_api: format!("{origin}/api/v1"),
            zai_api: origin.clone(),
            bigmodel_team_plans: format!("{origin}/coding-plan/team/plans"),
            bigmodel_api: origin,
            client_version: "0.0.0-test".to_owned(),
        },
    };
    (egress, services)
}

/// 夹具收到的标题请求（system 是标题提示词）。
fn title_requests(fixture: &sse::Fixture) -> Vec<Value> {
    let system = rust_cli_prompts::title::system();
    fixture
        .side_requests()
        .into_iter()
        .chain(fixture.main_requests())
        .filter(|body| body["system"][0]["text"] == system)
        .collect()
}

/// 目录模型（个人 provider 上的 glm-5.3，配置的上限 128000）：标题请求经补丁发出的是 `min(5000, 128000)` = 5000，
/// 同 Node；同一次运行的主请求不给 `provider_max_output_tokens`，仍是配置的上限。修前标题请求也发 128000。
#[tokio::test(flavor = "multi_thread")]
async fn a_catalog_title_request_sends_the_node_output_limit() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(vec![sse::text_reply("done")]);
    sandbox.provider(&fixture.url, GLM_5_3);
    let clock = Arc::new(SystemClock::utc());
    let (egress, services) = services(clock.clone()).await;
    let secret = CredentialSecret {
        explicit: Some(RedactedString::new("test-secret")),
        platform: rust_cli_platform::node_platform().to_owned(),
        home: sandbox.home.to_string_lossy().into_owned(),
        username: "dev".to_owned(),
    };
    let platform = Arc::new(Platform::new(&sandbox.paths(), &secret, services));
    let endpoints: Arc<dyn ModelEndpoints> = platform.clone();
    let model: Arc<dyn ModelPort> = Arc::new(ProviderModel::new(egress, endpoints, clock.clone()));
    let selection = ModelSelection {
        provider: "personal:gateway".to_owned(),
        model: GLM_5_3.to_owned(),
        reasoning: None,
        connection: None,
    };
    let directory: Arc<dyn ModelDirectory> = platform;
    let kernel = sandbox.kernel(model, Some(directory), &selection, clock);
    sandbox.first_input(&kernel, selection).await;
    let titles = title_requests(&fixture);
    assert_eq!(titles.len(), 1, "{:#?}", fixture.side_requests());
    assert_eq!(titles[0]["max_tokens"], 5_000);
    let main = fixture.main_requests();
    assert_eq!(main.len(), 1, "{main:#?}");
    assert_eq!(main[0]["max_tokens"], 128_000);
}

/// 显式覆盖（固定端点，没有补丁）：标题请求发自己的 5000，F97 前后相同。
#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_title_request_still_sends_5000() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(vec![sse::text_reply("done")]);
    let clock = Arc::new(SystemClock::utc());
    let (egress, _services) = services(clock.clone()).await;
    let endpoint = FixedEndpoint::new(AnthropicEndpoint {
        base_url: fixture.url.clone(),
        api_key: RedactedString::new("sk-test"),
    });
    let model: Arc<dyn ModelPort> = Arc::new(ProviderModel::new(
        egress,
        Arc::new(endpoint),
        clock.clone(),
    ));
    let selection = ModelSelection {
        provider: "anthropic".to_owned(),
        model: GLM_5_3.to_owned(),
        reasoning: None,
        connection: None,
    };
    let kernel = sandbox.kernel(model, None, &selection, clock);
    sandbox.first_input(&kernel, selection).await;
    let titles = title_requests(&fixture);
    assert_eq!(titles.len(), 1, "{:#?}", fixture.side_requests());
    assert_eq!(titles[0]["max_tokens"], 5_000);
}
