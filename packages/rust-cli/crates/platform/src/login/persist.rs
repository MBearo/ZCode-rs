// 语义复制自 apps/zcode-cli/packages/bootstrap/src/auth-login-persistence.ts 与 auth-login.ts 的凭据写入，
// packages/provider-node/src/personal-provider-config-repository.ts 的 `update`（33a9541140）；OAuth 登录与退出的键
// 照 App（packages/services/src/oauth/oauthService.ts、oauthProviderLogout.ts，工作线 F50）。
//! 登录与退出写的两个文件（platform.md §5.6）：`credentials.json`（目录锁内读改写，§5.5）与个人配置
//! `provider_config.json` 的 `defaultModelSelection`。

use rust_cli_fs::LockOptions;
use rust_cli_paths::StoragePath;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::LoginError;
use crate::account::{self, CodingPlanProvider};
use crate::credentials::{CredentialError, CredentialStore, Credentials, PRIVATE_WRITE};
use crate::token::Family;

/// 没有账号 Profile 的连接身份取 sha256 十六进制摘要的前几位。
const IDENTITY_DIGEST_HEX_CHARS: usize = 24;

/// `SHARED_ZCODE_CREDENTIAL_KEYS`：退出时全部删除。
const SHARED_KEYS: [&str; 8] = [
    "oauth:active_provider",
    "oauth:bigmodel:access_token",
    "oauth:bigmodel:refresh_token",
    "oauth:bigmodel:user_info",
    "oauth:zai:access_token",
    "oauth:zai:refresh_token",
    "oauth:zai:user_info",
    "zcodejwttoken",
];

/// 文件不存在时的个人配置：Node 空配置的编码（`encodeProviderConfigFile(emptyUpdate)`）。
fn empty_personal() -> Value {
    json!({
        "schemaVersion": 1,
        "config": {
            "providerOrder": [],
            "providerConfigRules": {"providerRules": []},
            "modelConfigRules": {"providerModelRules": [], "manualProviderModelRules": []},
        },
    })
}

/// 没有账号 Profile 的连接身份：`key-` + sha256(secret) 的前 24 个 hex（Node
/// `createStandaloneAccountIdentityFromSecret`）。
pub(crate) fn identity_from_secret(secret: &str) -> String {
    let digest: String = Sha256::digest(secret.trim().as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("key-{}", &digest[..IDENTITY_DIGEST_HEX_CHARS])
}

/// OAuth 登录（platform.md §5.6，工作线 F50）：一次读改写写入 App 轮询登录写的键（`None` 是删除），并删掉这个
/// provider 的手工连接（身份、来源与那份 key）——手工 key 的来源会盖过 OAuth（§5.2），OAuth 登录之后要用 OAuth。
/// 不写 `account-provider:<id>:identity` 与 `auth-source`：App 不写，本工程也不再读 OAuth 的这两个键（以前照 Node
/// 独立 CLI 写身份与 `auth-source = oauth`）。
pub(crate) async fn save_login(
    store: &CredentialStore,
    provider: &CodingPlanProvider,
    writes: &[(&str, Option<String>)],
) -> Result<(), CredentialError> {
    store
        .mutate(|credentials| {
            for key in manual_connection(credentials, &provider.provider) {
                credentials.remove(&key);
            }
            for (key, value) in writes {
                match value {
                    Some(value) => credentials.set(key, value)?,
                    None => credentials.remove(key),
                }
            }
            Ok(())
        })
        .await
}

/// 手工 key 登录（Node `configureCodingPlanApiKey`）：身份、`auth-source = manual` 与按身份存的 key。
pub(crate) async fn save_manual(
    store: &CredentialStore,
    provider: &CodingPlanProvider,
    identity: &str,
    key: &str,
) -> Result<(), CredentialError> {
    let provider = &provider.provider;
    store
        .mutate(|credentials| {
            credentials.set(&account::identity_key(provider), identity)?;
            credentials.set(&account::source_key(provider), "manual")?;
            credentials.set(&account::manual_key(provider, identity), key)
        })
        .await
}

/// 一个 provider 的手工连接（与 Node 独立 CLI 写的 OAuth 身份、来源）的键：身份、来源，来源是 manual 时还有按身份
/// 算出的 key。解不开的身份算不出 key 的键名，只给身份与来源（坏密文不阻止退出与登录）。
fn manual_connection(credentials: &Credentials, provider: &str) -> Vec<String> {
    let manual = credentials
        .present(&account::source_key(provider))
        .is_some_and(|source| source.expose() == "manual");
    let mut keys = Vec::new();
    if manual && let Some(identity) = credentials.present(&account::identity_key(provider)) {
        keys.push(account::manual_key(provider, identity.expose()));
    }
    keys.push(account::identity_key(provider));
    keys.push(account::source_key(provider));
    keys
}

/// 退出（App `OAuthService.logoutAll` 加本工程与 Node 独立 CLI 自己的键，platform.md §5.6）：删掉共享的 OAuth 键；
/// 每个个人 Coding Plan provider 的 App 派生 key（按退出前的 profile id，App `createOAuthProviderLogoutHandler`）与
/// 手工连接。项目位置不删（App 也不删）。修复线 F50：以前不删 App 的派生 key。
pub(crate) async fn forget_connections(
    store: &CredentialStore,
    providers: &[CodingPlanProvider],
) -> Result<(), CredentialError> {
    store
        .mutate(|credentials| {
            let mut keys: Vec<String> = SHARED_KEYS.iter().map(|key| (*key).to_owned()).collect();
            for candidate in providers {
                let provider = &candidate.provider;
                let profile = Family::parse(&candidate.family)
                    .and_then(|family| account::profile_id(credentials, family));
                if let Some(profile) = profile {
                    keys.push(account::manual_key(provider, &profile));
                }
                keys.extend(manual_connection(credentials, provider));
            }
            for key in &keys {
                credentials.remove(key);
            }
            Ok(())
        })
        .await
}

/// 持锁把个人配置的 `defaultModelSelection` 设为 `{providerId, modelId}`；`reasoning` 有值时另带
/// `options: {reasoningLevel}`（`/effort`，Node `saveConfiguredDefault`，tui-manage.md §2.2）。文件不存在时从 Node
/// 的空配置开始；不是 `schemaVersion: 1` 且带 `config` 对象的文件不覆盖（Node 同样拒绝写入无效配置）。其余字段
/// 原样保留。
///
/// 文件不存在、旧 CLI 用户文件 `legacy` 又有 Node 要导入的 provider 或默认模型时不建文件
/// （[`LoginError::LegacyImportPending`]，修复线 F2）：Node 只在文件不存在时导入旧文件，抢先建文件会让旧 provider
/// 永远丢失（`legacy.rs`）。
pub(crate) async fn save_default_selection(
    (file, legacy): (&StoragePath, &StoragePath),
    provider: &str,
    model: &str,
    reasoning: Option<&str>,
) -> Result<(), LoginError> {
    let path = file.as_path();
    let failed = |reason: String| LoginError::ConfigUpdate(reason);
    rust_cli_fs::with_file_lock(path, &LockOptions::default(), async || {
        let mut config = match rust_cli_fs::read_optional(path).await {
            Ok(None) => {
                let old = rust_cli_fs::read_optional(legacy.as_path())
                    .await
                    .ok()
                    .flatten();
                let old = old.and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
                if old.as_ref().is_some_and(super::legacy::pending) {
                    return Err(LoginError::LegacyImportPending {
                        file: path.display().to_string(),
                        legacy: legacy.as_path().display().to_string(),
                    });
                }
                empty_personal()
            }
            Ok(Some(bytes)) => serde_json::from_slice::<Value>(&bytes)
                .map_err(|error| failed(format!("{}: {error}", path.display())))?,
            Err(error) => return Err(failed(format!("{}: {error}", path.display()))),
        };
        let valid = config["schemaVersion"] == 1 && config["config"].is_object();
        let Some(body) = config
            .get_mut("config")
            .and_then(Value::as_object_mut)
            .filter(|_| valid)
        else {
            return Err(failed(format!(
                "{} is not a schemaVersion 1 provider config",
                path.display()
            )));
        };
        let mut selection = Map::new();
        selection.insert("providerId".to_owned(), Value::String(provider.to_owned()));
        selection.insert("modelId".to_owned(), Value::String(model.to_owned()));
        if let Some(level) = reasoning {
            let mut options = Map::new();
            options.insert("reasoningLevel".to_owned(), Value::String(level.to_owned()));
            selection.insert("options".to_owned(), Value::Object(options));
        }
        body.insert("defaultModelSelection".to_owned(), Value::Object(selection));
        let text = crate::cli_config::node_pretty(&config);
        rust_cli_fs::atomic_write(path, text.as_bytes(), PRIVATE_WRITE)
            .await
            .map_err(|error| failed(format!("{}: {error}", path.display())))
    })
    .await
}

impl From<std::io::Error> for LoginError {
    /// 目录锁的错误（超时、权限）。
    fn from(error: std::io::Error) -> Self {
        Self::ConfigUpdate(error.to_string())
    }
}

#[cfg(test)]
#[path = "persist_tests.rs"]
mod tests;
