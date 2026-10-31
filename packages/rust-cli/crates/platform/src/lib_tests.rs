//! 平台层对内核暴露的两个端口（platform.md §4）：目录、默认选择与每请求的端点。

use rust_cli_base::{hosts, models};
use serde_json::json;

use super::*;
use crate::test_support::write;

const PERSONAL: &str = include_str!("../tests/fixtures/personal.json");
const ZAI: &str = "account:zai-individual-coding-plan";
const IDENTITY: &str = "key-0123456789abcdef01234567";

fn secret() -> CredentialSecret {
    CredentialSecret {
        explicit: Some(RedactedString::new("test-secret")),
        platform: "linux".to_owned(),
        home: "/home/dev".to_owned(),
        username: "dev".to_owned(),
    }
}

async fn platform(dir: &std::path::Path) -> (Platform, StoragePaths) {
    let paths = StoragePaths::resolve(&Default::default(), dir, dir);
    write(paths.provider_config().as_path(), PERSONAL).await;
    let services = crate::test_support::offline_services().await;
    (Platform::new(&paths, &secret(), services), paths)
}

fn selection(provider: &str, model: &str, reasoning: Option<&str>) -> ModelSelection {
    ModelSelection {
        provider: provider.to_owned(),
        model: model.to_owned(),
        reasoning: reasoning.map(str::to_owned),
        connection: None,
    }
}

#[tokio::test]
async fn the_catalog_and_default_come_from_the_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, _paths) = platform(dir.path()).await;
    let catalog = platform.catalog().await;
    assert_eq!(catalog.revision, 1);
    let names: Vec<String> = catalog
        .models
        .iter()
        .map(|model| format!("{}/{}", model.provider, model.model))
        .collect();
    assert_eq!(
        names,
        [
            format!("personal:responses/{}", models::GPT_5_4),
            format!("personal:gateway/{}", models::GLM_5),
            "personal:gateway/custom-chat".to_owned(),
            format!("personal:anthropic/{}", models::OPUS_5),
        ]
    );
    assert_eq!(
        platform.default_selection().await,
        Some(selection("personal:gateway", "custom-chat", Some("low")))
    );
}

#[tokio::test]
async fn api_key_providers_resolve_with_headers_and_patches() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, _paths) = platform(dir.path()).await;
    let endpoint = platform
        .endpoint(
            &selection("personal:gateway", "custom-chat", Some("high")),
            None,
        )
        .await
        .unwrap();
    assert_eq!(endpoint.protocol, WireProtocol::OpenAiChatCompletions);
    assert_eq!(endpoint.base_url, "https://gateway.example.com/v1");
    assert_eq!(endpoint.model, "custom-chat");
    assert_eq!(
        endpoint.credential,
        Credential::ApiKey(RedactedString::new("sk-personal-fixture"))
    );
    assert_eq!(
        endpoint.headers,
        vec![("X-Team".to_owned(), RedactedString::new("dev"))]
    );
    assert_eq!(
        endpoint.body_patches[1],
        json!({"max_completion_tokens": 8192})
    );
    assert!(!endpoint.renewable && !endpoint.mid_conversation_system);
    // `inputFormat.supportsImage` 为假：适配器把图片换成占位文字（model.md §10.5）。
    assert!(!endpoint.supports_image);

    // Anthropic 协议的 provider 另带 Bearer（Node 的 Provider 适配器）；claude-opus-5 支持中途 system 消息。
    let opus = platform
        .endpoint(&selection("personal:anthropic", models::OPUS_5, None), None)
        .await
        .unwrap();
    assert_eq!(
        opus.headers,
        vec![(
            "authorization".to_owned(),
            RedactedString::new("Bearer sk-ant-fixture")
        )]
    );
    assert!(opus.mid_conversation_system);
    assert!(opus.supports_image);

    let missing = platform
        .endpoint(&selection("personal:nokey", models::OPUS_5, None), None)
        .await;
    assert!(matches!(missing, Err(ModelError::InvalidRequest { .. })));
}

/// 请求交来的输出上限进 `maxOutputTokens` 补丁的输入（工作线 F95，platform.md §3.3）：取配置的 max、
/// `ZCODE_MAX_OUTPUT_TOKENS` 与它三者的最小值；不给时照旧是配置的 max（custom-chat 是 8192）。
#[tokio::test]
async fn the_request_output_limit_feeds_the_max_output_tokens_patch() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, _paths) = platform(dir.path()).await;
    let chat = selection("personal:gateway", "custom-chat", Some("high"));
    let patch = |endpoint: ModelEndpoint| endpoint.body_patches[1].clone();
    let limited = |limit| platform.endpoint_with_output_limit(&chat, None, limit);
    assert_eq!(
        patch(limited(Some(4096)).await.unwrap()),
        json!({"max_completion_tokens": 4096})
    );
    assert_eq!(
        patch(limited(Some(500_000)).await.unwrap()),
        json!({"max_completion_tokens": 8192})
    );
    assert_eq!(
        patch(limited(None).await.unwrap()),
        patch(platform.endpoint(&chat, None).await.unwrap())
    );
    let capped = platform.with_max_output_tokens(Some(2000));
    let both = capped
        .endpoint_with_output_limit(&chat, None, Some(4096))
        .await
        .unwrap();
    assert_eq!(patch(both), json!({"max_completion_tokens": 2000}));
}

#[tokio::test]
async fn account_models_take_the_credential_on_every_request() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, paths) = platform(dir.path()).await;
    let select = selection(ZAI, models::GLM_5_3_ACCOUNT, Some("high"));
    assert!(matches!(
        platform.endpoint(&select, None).await,
        Err(ModelError::InvalidRequest { .. })
    ));
    let login = |key: &str| {
        json!({
            account::identity_key(ZAI): IDENTITY,
            account::source_key(ZAI): "manual",
            account::manual_key(ZAI, IDENTITY): key,
        })
        .to_string()
    };
    write(paths.credentials().as_path(), login("sk-first")).await;
    let first = platform.endpoint(&select, None).await.unwrap();
    assert_eq!(
        first.credential,
        Credential::ApiKey(RedactedString::new("sk-first"))
    );
    assert!(first.renewable);
    assert_eq!(first.base_url, hosts::ZAI_ANTHROPIC);
    // 同一个 key 被拒：不能续期。
    assert!(matches!(
        platform.endpoint(&select, Some(&first)).await,
        Err(ModelError::Authentication { .. })
    ));
    // 凭据换了：下一次请求拿到新的。
    write(paths.credentials().as_path(), login("sk-second")).await;
    let renewed = platform.endpoint(&select, Some(&first)).await.unwrap();
    assert_eq!(
        renewed.credential,
        Credential::ApiKey(RedactedString::new("sk-second"))
    );
    assert_eq!(
        platform.catalog().await.models[0].model,
        models::GLM_5_3_ACCOUNT,
        "account models come first"
    );
}

/// 缺口 G8（app-protocol.md §5.2）：注入的来源记下每次请求，给固定的 key 与一个请求头。
struct Supplied(std::sync::Mutex<Vec<header_source::HeaderRequest>>);

#[async_trait]
impl header_source::HeaderSource for Supplied {
    async fn request_auth(
        &self,
        request: header_source::HeaderRequest,
    ) -> Result<header_source::SuppliedAuth, ModelError> {
        let count = {
            let mut seen = self.0.lock().unwrap();
            seen.push(request);
            seen.len()
        };
        Ok(header_source::SuppliedAuth {
            api_key: Some(RedactedString::new(format!("sk-host-{count}"))),
            headers: vec![("x-host-sign".to_owned(), RedactedString::new("signed"))],
        })
    }
}

/// 有注入的来源时，账号模型的凭据与请求头都向它要（带上会话与被拒凭据的指纹）；API key 的 provider 不问它。
#[tokio::test]
async fn an_injected_header_source_supplies_account_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, paths) = platform(dir.path()).await;
    let source = Arc::new(Supplied(std::sync::Mutex::new(Vec::new())));
    let platform = platform.with_header_source(source.clone());
    write(
        paths.credentials().as_path(),
        json!({
            account::identity_key(ZAI): IDENTITY,
            account::source_key(ZAI): "manual",
            account::manual_key(ZAI, IDENTITY): "sk-own",
        })
        .to_string(),
    )
    .await;
    let select = selection(ZAI, models::GLM_5_3_ACCOUNT, Some("high"));
    let context = EndpointContext {
        session: Some(rust_cli_kernel_api::ids::SessionId::new("session_1")),
    };
    let first = platform
        .endpoint_in(&select, None, None, &context)
        .await
        .unwrap();
    assert_eq!(
        first.credential,
        Credential::ApiKey(RedactedString::new("sk-host-1"))
    );
    let header = |endpoint: &ModelEndpoint, name: &str| {
        endpoint
            .headers
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, value)| value.expose().to_owned())
    };
    assert_eq!(header(&first, "x-host-sign").as_deref(), Some("signed"));
    assert_eq!(
        header(&first, "authorization").as_deref(),
        Some("Bearer sk-host-1")
    );
    let renewed = platform
        .endpoint_in(&select, Some(&first), None, &context)
        .await
        .unwrap();
    assert_eq!(
        renewed.credential,
        Credential::ApiKey(RedactedString::new("sk-host-2"))
    );
    let seen = source.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].session, context.session);
    assert_eq!(seen[0].rejected_fingerprint, None);
    assert_eq!(
        seen[1].rejected_fingerprint.as_deref(),
        Some(token::fingerprint("sk-host-1").as_str())
    );
    assert!(!seen[0].account_type.is_empty() && !seen[0].mode.is_empty());
    // API key 的 provider 照旧用配置里的 key，不问来源。
    let personal = platform
        .endpoint_in(
            &selection("personal:gateway", "custom-chat", Some("high")),
            None,
            None,
            &context,
        )
        .await
        .unwrap();
    assert!(matches!(personal.credential, Credential::ApiKey(_)));
    assert_eq!(source.0.lock().unwrap().len(), 2);
}

/// 修复线 F61（platform.md §4）：压缩的上限同步读已发布的快照——还没取过目录时答不出；取过之后是配置的
/// `contextWindow` 与 `maxOutputTokens.max`，不看团队项目；改了配置不重读，下一次取目录或端点之后才是新值。
#[tokio::test]
async fn limits_come_from_the_published_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, paths) = platform(dir.path()).await;
    let chat = selection("personal:gateway", "custom-chat", None);
    assert_eq!(platform.limits(&chat), None);
    platform.catalog().await;
    let expected = TokenLimits {
        context_window: 64_000,
        max_output_tokens: Some(8_192),
    };
    assert_eq!(platform.limits(&chat), Some(expected));
    // 内置规则 `.*glm-5.*`：200000 / 64000。
    let glm = platform.limits(&selection("personal:gateway", models::GLM_5, None));
    assert_eq!(
        glm,
        Some(TokenLimits {
            context_window: 200_000,
            max_output_tokens: Some(64_000),
        })
    );
    let mut teamed = chat.clone();
    teamed.connection = Some(Box::new(rust_cli_kernel_api::model::ModelConnection {
        product_id: "prod".to_owned(),
        organization_id: "org".to_owned(),
        project_id: "proj".to_owned(),
    }));
    assert_eq!(platform.limits(&teamed), Some(expected));
    assert_eq!(
        platform.limits(&selection("personal:gateway", "missing", None)),
        None
    );
    // 改配置：同步查询不重读；取端点（每个请求都取）之后是新值。
    let changed = PERSONAL.replace("\"contextWindow\": 64000", "\"contextWindow\": 100000");
    assert_ne!(changed, PERSONAL);
    write(paths.provider_config().as_path(), changed).await;
    assert_eq!(platform.limits(&chat), Some(expected));
    let _ = platform.endpoint(&chat, None).await;
    assert_eq!(
        platform.limits(&chat).map(|limits| limits.context_window),
        Some(100_000)
    );
}

/// 工作线 F64（model.md §2.4）：账号登录的 provider 去掉配置里静态的 `authorization` 与 `x-api-key`（不分大小写），其余照发。
#[test]
fn account_providers_drop_static_auth_headers() {
    let header = |name: &str, value: &str| (name.to_owned(), RedactedString::new(value));
    let mut headers = vec![
        header("Authorization", "Bearer static"),
        header("X-API-KEY", "static"),
        header("X-Team", "dev"),
        header("x-api-key-id", "kept"),
    ];
    without_auth_headers(&mut headers);
    assert_eq!(
        headers,
        [header("X-Team", "dev"), header("x-api-key-id", "kept")]
    );
}
