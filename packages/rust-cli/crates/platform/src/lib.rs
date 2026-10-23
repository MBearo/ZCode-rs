//! 平台能力：Provider registry、账号、设置、每次请求的鉴权（docs/specs/rust-cli/platform.md）。
//!
//! 对内核与模型适配器只暴露 kernel-api 的三个端口：
//!
//! - [`ModelDirectory`]：可用模型目录与默认选择（带版本的快照）；
//! - [`ModelEndpoints`]：按 `ModelSelection` 解析出的端点与凭据。官方账号模型每次请求都从
//!   `credentials.json` 取当前凭据（每请求鉴权；OAuth 登录换短期项目令牌）；
//! - [`ModelLimits`]：压缩用的窗口与输出上限，同步读已发布的快照（修复线 F61）。
//!
//! 另给入口登录与退出（[`Platform::begin_login`]、[`Platform::configure_api_key`]、[`Platform::logout`]）。
//!
//! Provider 配置的格式与语义复制自 Host（`apps/zcode-host/crates/providers`）与 Node 独立 CLI，读写同一份
//! `~/.zcode/v2`；文件经 `rust-cli-fs`（写入持与 Node 同形的目录锁），网络经 `rust-cli-net`，路径由
//! `StoragePaths` 解析，不读进程环境（入口读好后传入）。

mod account;
mod auth;
mod cli_config;
mod credentials;
mod device;
#[cfg(test)]
mod fake_server_tests;
mod history;
mod hook_trust;
mod login;
pub mod model_name;
mod option_map;
mod personal;
pub mod plugin_inventory;
mod providers;
mod settings;
mod team;
#[cfg(test)]
mod test_support;
mod token;

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use rust_cli_kernel_api::provider::{
    Credential, ModelCapabilities, ModelCatalog, ModelDirectory, ModelEndpoint, ModelEndpoints,
    ModelLimits, TokenLimits, WireProtocol,
};
use rust_cli_net::Egress;
use rust_cli_net::header;
use rust_cli_paths::{StoragePath, StoragePaths};

pub use cli_config::{
    ConfigFile, ConfigWriteError, UiLocale, UiTheme, mcp_enabled_text, read_cli_config,
    write_mcp_enabled, write_ui_locale, write_ui_theme,
};
pub use credentials::{CredentialError, CredentialSecret};
pub use device::{DeviceMid, NewUuid};
pub use history::{History, Recorder};
pub use hook_trust::{HookTrustStore, TrustError, TrustLoad, TrustRecord};
pub use login::{LoginError, LoginOutcome, PendingLogin, SetupOutcome};
/// Node 的平台名（定义在 base，F55）；入口经 platform 取用，路径不变。
pub use rust_cli_base::js::node_platform;
pub use settings::Settings;
pub use token::{AccountOrigins, Family, TokenError};

use crate::auth::AccountAuth;
use crate::credentials::CredentialStore;
use crate::providers::catalog::{Access, Template};
pub use crate::providers::site::SiteRules;
use crate::providers::{Plans, Registry};
use crate::token::ProjectTokens;

/// 账号服务要用的出口、时钟与地址（入口组装）。
#[derive(Clone)]
pub struct AccountServices {
    pub egress: Arc<Egress>,
    pub clock: Arc<dyn Clock>,
    pub origins: AccountOrigins,
}

/// 把模型与档位存成新会话的缺省（`/effort`，docs/specs/rust-cli/tui-manage.md §2.2）：写个人 Provider 配置
/// `provider_config.json` 的 `defaultModelSelection`（与 Node `saveConfiguredDefault` 同一个文件与字段）。
pub async fn save_default_model(
    paths: &StoragePaths,
    selection: &ModelSelection,
) -> Result<String, String> {
    login::save_default_model((&paths.provider_config(), &paths.cli_config()), selection).await
}

/// 平台层的组装。
pub struct Platform {
    registry: Registry,
    credentials: CredentialStore,
    auth: AccountAuth,
    settings: Settings,
    services: AccountServices,
    personal_config: StoragePath,
    /// 旧 CLI 用户文件 `~/.zcode/cli/config.json`：个人配置不存在时 Node 从它导入（修复线 F2）。
    legacy_config: StoragePath,
    /// `ZCODE_MAX_OUTPUT_TOKENS`（cli-args.md §9.1，修复线 F42）：`maxOutputTokens.map` 取它与配置的较小值。
    max_output_tokens: Option<u64>,
}

impl Platform {
    /// `paths` 已按入口读到的 `ZCODE_DATA_BASE_DIR`、`ZCODE_BUILTIN_PROVIDER_CONFIG_FILE` 调整过；`secret` 是
    /// 凭据密钥的材料；`services` 给账号服务（项目令牌、登录）用。
    pub fn new(paths: &StoragePaths, secret: &CredentialSecret, services: AccountServices) -> Self {
        let credentials = CredentialStore::new(paths.credentials(), secret);
        // 项目令牌的缓存由个人订阅查询与请求期共用（F50）：取目录时换到的令牌，第一次请求直接用。
        let tokens = Arc::new(ProjectTokens::new(
            services.egress.clone(),
            services.clock.clone(),
            services.origins.clone(),
        ));
        let settings = Settings::new(paths.settings().clone());
        let plans = Plans {
            personal: Arc::new(personal::PersonalPlans::new(
                services.egress.clone(),
                services.origins.clone(),
                tokens.clone(),
                credentials.clone(),
            )),
            teams: Arc::new(team::TeamPlans::new(
                services.egress.clone(),
                services.origins.clone(),
            )),
        };
        Self {
            registry: Registry::new(
                paths.builtin_provider_config().cloned(),
                paths.provider_config(),
                credentials.clone(),
                plans,
            ),
            auth: AccountAuth::new(credentials.clone(), tokens),
            credentials,
            settings,
            services,
            personal_config: paths.provider_config(),
            legacy_config: paths.cli_config(),
            max_output_tokens: None,
        }
    }

    /// 入口读到的 `ZCODE_MAX_OUTPUT_TOKENS`（cli-args.md §9.1，修复线 F42）：每个模型的 `maxOutputTokens.map` 用
    /// min(配置的 max, 它) 求值（model.md §9）。
    #[must_use]
    pub fn with_max_output_tokens(mut self, limit: Option<u32>) -> Self {
        self.max_output_tokens = limit.map(u64::from);
        self
    }

    /// 个人 Provider 配置 `defaultModelSelection` 的模型 ID（与 Node 同一个文件与字段，经同一次解析）：显式覆盖
    /// （`ZCODE_API_KEY`）没给模型时用它（model.md §6，用户决定 2026-10-20，修复线 F42）。
    pub async fn configured_default_model(&self) -> Option<String> {
        self.registry
            .current()
            .await
            .snapshot
            .configured_model
            .clone()
    }

    /// 凭据文件 `credentials.json` 的位置。
    pub fn credentials_file(&self) -> &StoragePath {
        self.credentials.file()
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// 把当前模型与档位存成新会话的缺省（`/effort`，Node `rememberCurrentModelSelection`，
    /// docs/specs/rust-cli/tui-manage.md §2.2）：写个人配置的 `defaultModelSelection`。返回文件路径。
    pub async fn save_default_model(&self, selection: &ModelSelection) -> Result<String, String> {
        login::save_default_model((&self.personal_config, &self.legacy_config), selection).await
    }

    /// 内置配置的通用规则（显式覆盖的端点按它算能力，model.md §6，修复线 F23）。内置配置读不了时为空规则
    /// （能力是缺省值）。
    pub async fn site_rules(&self) -> SiteRules {
        match self.registry.builtin_config().await {
            Ok(builtin) => SiteRules::new(&builtin),
            Err(_) => SiteRules::default(),
        }
    }

    /// 团队项目不可用的提示（已过期 / 未分配：App 的文案加管理链接，platform.md §9.2）；`-p` 选不出模型时用它代替
    /// 通用的报错。`provider` 是 `--model` 指的 provider（没给时看所有团队 provider）。
    pub async fn team_notice(&self, provider: Option<&str>) -> Option<String> {
        self.registry.team_notice(provider, None).await
    }

    /// 新会话默认使用的模型；没有可用 Provider 时为空。
    pub async fn default_selection(&self) -> Option<ModelSelection> {
        self.registry.current().await.snapshot.default.clone()
    }

    async fn credential(
        &self,
        template: &Template,
        rejected: Option<&RedactedString>,
    ) -> Result<RedactedString, ModelError> {
        match &template.access {
            Access::ApiKey(key) => Ok(key.clone()),
            Access::Account { account_type, mode } => {
                let team = template
                    .connection
                    .as_ref()
                    .map(|connection| &connection.id);
                self.auth
                    .credential((&template.provider, account_type, mode), team, rejected)
                    .await
            }
        }
    }
}

#[async_trait]
impl ModelDirectory for Platform {
    async fn catalog(&self) -> ModelCatalog {
        let current = self.registry.current().await;
        ModelCatalog {
            revision: current.revision,
            models: current.snapshot.catalog(),
            default: current.snapshot.default.clone(),
        }
    }
}

/// 压缩的窗口与输出上限（修复线 F61，platform.md §4）：只读已发布的快照，不重读文件；配置的变化由取目录、取端点时
/// 的重读带进来，最多晚一步。
impl ModelLimits for Platform {
    fn limits(&self, selection: &ModelSelection) -> Option<TokenLimits> {
        self.registry.published().snapshot.limits(selection)
    }
}

#[async_trait]
impl ModelEndpoints for Platform {
    async fn endpoint(
        &self,
        selection: &ModelSelection,
        rejected: Option<&ModelEndpoint>,
    ) -> Result<ModelEndpoint, ModelError> {
        let current = self.registry.current().await;
        let Some(template) = current.snapshot.find(selection) else {
            // 这个团队项目已过期 / 未分配：照 App 的文案提示（platform.md §9.2）。
            let team = selection.connection.as_deref();
            if let Some(notice) = self
                .registry
                .team_notice(Some(&selection.provider), team)
                .await
            {
                return Err(ModelError::Authentication { message: notice });
            }
            return Err(ModelError::InvalidRequest {
                message: format!(
                    "model {}/{} is not available (check the provider configuration or log in)",
                    selection.provider, selection.model
                ),
            });
        };
        let body_patches =
            template.patches(selection.reasoning.as_deref(), self.max_output_tokens)?;
        let previous = match rejected.map(|endpoint| &endpoint.credential) {
            Some(Credential::ApiKey(previous)) => Some(previous),
            Some(Credential::None) | None => None,
        };
        let key = self.credential(template, previous).await?;
        // 被拒的凭据重新读到的还是同一个（手工 key 不会自己变）：不再重试，直接报鉴权失败。
        if let Some(previous) = previous
            && previous.expose() == key.expose()
        {
            return Err(ModelError::Authentication {
                message: format!("the credential of {} was rejected", selection.provider),
            });
        }
        let mut headers = template.headers.clone();
        // 账号登录的 provider：配置里静态的鉴权头不发，凭据只来自每请求鉴权（Node `withoutModelAuthHeaders`，工作线
        // F64，model.md §2.4）。以前静态的 `Authorization` 会盖过账号的凭据。
        if matches!(template.access, Access::Account { .. }) {
            without_auth_headers(&mut headers);
        }
        // Node 的 Provider 适配器对 Anthropic 兼容网关同时发 Bearer；配置里显式给了 Authorization 时以配置为准。
        if template.protocol == WireProtocol::AnthropicMessages
            && !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(header::AUTHORIZATION))
        {
            headers.insert(
                0,
                (
                    header::AUTHORIZATION.to_owned(),
                    header::bearer(key.expose()),
                ),
            );
        }
        let capabilities = &template.capabilities;
        Ok(ModelEndpoint {
            protocol: template.protocol,
            base_url: template.base_url.clone(),
            model: template.model.clone(),
            credential: Credential::ApiKey(key),
            headers,
            body_patches,
            mid_conversation_system: capabilities.supports_mid_conversation_system,
            supports_image: capabilities.supports_image,
            supports_pdf: capabilities.supports_pdf,
            supports_video: capabilities.supports_video,
            supports_audio: capabilities.supports_audio,
            renewable: matches!(template.access, Access::Account { .. }),
        })
    }

    /// 模板的能力（隐藏的 provider 也能答，与 `endpoint` 相同的查找），不取凭据（修复线 F23）。
    async fn capabilities(&self, selection: &ModelSelection) -> Option<ModelCapabilities> {
        let current = self.registry.current().await;
        current
            .snapshot
            .find(selection)
            .map(|template| template.capabilities)
    }
}

/// 去掉 `authorization` 与 `x-api-key`（名字不分大小写，Node `MODEL_AUTH_HEADER_NAMES`）。
fn without_auth_headers(headers: &mut Vec<(String, RedactedString)>) {
    headers.retain(|(name, _)| {
        !name.eq_ignore_ascii_case(header::AUTHORIZATION) && !name.eq_ignore_ascii_case("x-api-key")
    });
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
