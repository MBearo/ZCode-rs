//! 模型的组装（docs/specs/rust-cli/model.md §6、platform.md §1）：显式覆盖或平台层。
//!
//! - `ZCODE_API_KEY` 非空：显式覆盖（本地对照测试依赖它），Anthropic 端点 `ZCODE_MODEL_BASE_URL`
//!   （空白时官方 API），模型依次取 `--model`、`ZCODE_MODEL`、个人 Provider 配置的 `defaultModelSelection`。
//!   名字都是 `ZCODE_*`（修复线 F42，config.md §2.5），Anthropic SDK 的 `ANTHROPIC_*` 不读。
//! - 否则：平台层的 Provider registry。`--model` 是 `<provider>/<model>` 或目录里的模型 ID（取第一个
//!   匹配），没有时用目录的默认选择。
//!
//! 选不出模型时不在启动时失败：模型端口对每次请求报鉴权错误并说明原因（与没有配置时的 M0 行为相同）。

use std::sync::Arc;

use rust_cli_base::names::DOT_DIR;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::provider::{
    ModelCatalog, ModelDirectory, ModelLimits, TokenLimits, WireProtocol,
};
use rust_cli_model::{
    AnthropicEndpoint, CapabilityRules, DeviceIdSource, FixedEndpoint, ProviderModel,
};
use rust_cli_net::Egress;
use rust_cli_platform::model_name::{self, LookupError};
use rust_cli_platform::{Platform, SiteRules};

use crate::options::Assumptions;

/// 显式覆盖用的环境变量（启动时捕获）。
pub(crate) struct Override {
    pub base_url: Option<String>,
    pub api_key: Option<RedactedString>,
    pub model: Option<String>,
    /// 只给测试用的两个声明（model.md §6，只在 debug 构建读）。
    pub assume: Assumptions,
    /// 只给测试的 SSH 入口（remote-exec.md §19.11，只在 debug 构建读）。
    pub ssh: Option<crate::options::SshAssumption>,
}

/// Node 配置里 Anthropic 官方 API 的写法（能力规则按它匹配，model.md §6；与请求方案共用 model 的定义，F55）。
const OFFICIAL_RULE_URL: &str = rust_cli_model::ANTHROPIC_OFFICIAL_BASE_URL;

/// 显式覆盖的 base URL 在能力规则里的写法：按 SDK 补 `/v1`（Node 的 Anthropic provider 写带 `/v1` 的地址，例如
/// 官方 API 的站点规则是 `https://api.anthropic.com/v1`），规范化在规则里做。规则与请求地址相同（model 的
/// `anthropic_v1_base_url`，F55）。
pub(crate) fn rule_base_url(base_url: &str) -> String {
    rust_cli_model::anthropic_v1_base_url(base_url)
}

/// 显式覆盖的能力与压缩上限（修复线 F23、F61，model.md §6、§11，compact.md §2）：内置配置的通用规则，同一份规则与
/// 规则地址，按选择里的模型算（`/model` 可以换）。`assume`：两个测试声明（`_ZCODE_ASSUME_FIRST_PARTY_BASE_URL`、
/// `_ZCODE_ASSUME_TOOL_SEARCH`）。
fn explicit_rules(
    rules: SiteRules,
    base_url: &str,
    assume: Assumptions,
) -> (CapabilityRules, Arc<dyn ModelLimits>) {
    let url = if assume.first_party {
        OFFICIAL_RULE_URL.to_owned()
    } else {
        rule_base_url(base_url)
    };
    let limits = Arc::new(ExplicitLimits {
        rules: Arc::new(rules),
        url,
    });
    let shared = limits.clone();
    let capabilities: CapabilityRules = Arc::new(move |model: &str| {
        let mut capabilities =
            shared
                .rules
                .capabilities(WireProtocol::AnthropicMessages, &shared.url, model);
        capabilities.supports_tool_search |= assume.tool_search;
        capabilities
    });
    (capabilities, limits)
}

/// 显式覆盖的压缩上限（修复线 F61）：规则的 `contextWindow` 与 `optionSpecs.maxOutputTokens.max`。
///
/// 修 bug：以前显式覆盖没有模型目录，压缩与 `/context` 按卡片或默认窗口 200,000，配置 1M 的 glm-5.3 在约 167k 就压缩。
struct ExplicitLimits {
    rules: Arc<SiteRules>,
    url: String,
}

impl ModelLimits for ExplicitLimits {
    fn limits(&self, selection: &ModelSelection) -> Option<TokenLimits> {
        self.rules
            .limits(WireProtocol::AnthropicMessages, &self.url, &selection.model)
    }
}

/// 显式覆盖的 Anthropic 端点：没有 API key 时为空。
pub(crate) fn anthropic_endpoint(
    base_url: Option<String>,
    api_key: Option<RedactedString>,
) -> Option<AnthropicEndpoint> {
    let api_key = api_key.filter(|key| !key.expose().is_empty())?;
    let base_url = base_url
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| AnthropicEndpoint::DEFAULT_BASE_URL.to_owned());
    Some(AnthropicEndpoint { base_url, api_key })
}

/// 按 `--model` 在目录里选模型；没有 `--model` 时用默认选择。写法见 cli-args.md §1.3（团队套餐加 `[<团队>]`，
/// 工作线 F49，platform `model_name`）。
pub(crate) fn pick(catalog: &ModelCatalog, flag: Option<&str>) -> Result<ModelSelection, String> {
    let Some(flag) = flag.map(str::trim).filter(|flag| !flag.is_empty()) else {
        return catalog.default.clone().ok_or_else(|| {
            format!(
                "no model provider configured: add one to ~/{DOT_DIR}/v2/provider_config.json, \
                 log in with the ZCode app, or set ZCODE_API_KEY"
            )
        });
    };
    match model_name::lookup(&catalog.models, flag) {
        Ok(entry) => {
            let mut selection = model_name::selection(entry);
            // 默认选择正好是这一项时沿用它的档位，否则由平台层取最后一个档位。
            selection.reasoning = catalog
                .default
                .as_ref()
                .filter(|default| model_name::is_entry(default, entry))
                .and_then(|default| default.reasoning.clone());
            Ok(selection)
        }
        Err(LookupError::Missing) => {
            let available: Vec<String> = catalog
                .models
                .iter()
                .map(|entry| model_name::written(entry, false))
                .collect();
            Err(format!(
                "model {flag} is not configured (available: {})",
                if available.is_empty() {
                    "none".to_owned()
                } else {
                    available.join(", ")
                }
            ))
        }
        Err(error) => Err(error.to_string()),
    }
}

/// 显式覆盖的模型（model.md §6，用户决定 2026-10-20）：`--model` → `ZCODE_MODEL` → 个人 Provider 配置的
/// `defaultModelSelection.modelId`（与 Node 同一个文件与字段）。都没有时报错提示 `--model`。
///
/// 修复线 F42：以前没有模型就报错；用户决定用 zcode 配置里的缺省，不写死缺省模型。
pub(crate) fn explicit_model(
    flag: Option<String>,
    env: Option<String>,
    configured: Option<String>,
) -> Result<String, String> {
    flag.or(env)
        .or(configured)
        .ok_or_else(|| "ZCODE_API_KEY is set: pass --model or set ZCODE_MODEL".to_owned())
}

/// `--effort` 写进新会话的选择（cli-args.md §1.4）。`catalog` 是平台层的模型目录（显式覆盖没有目录）：有目录时按这个
/// 模型的档位取舍（不在目录里的选择没有可用的档位，不改）。
pub(crate) fn with_effort(
    mut selection: ModelSelection,
    effort: Option<&str>,
    catalog: Option<&ModelCatalog>,
) -> ModelSelection {
    let Some(level) = effort else {
        return selection;
    };
    let offered = catalog.map(|catalog| {
        catalog
            .models
            .iter()
            .find(|entry| model_name::is_entry(&selection, entry))
            .map(|entry| entry.reasoning_levels.clone())
            .unwrap_or_default()
    });
    crate::effort::apply(&mut selection, level, offered.as_deref());
    selection
}

/// 组装的结果：模型端口、新会话的默认选择与压缩的上限查询（修复线 F61：两种模式都有，compact.md §2）。
pub(crate) struct Assembled {
    pub model: ProviderModel,
    pub selection: ModelSelection,
    pub limits: Arc<dyn ModelLimits>,
}

/// 模型端口与新会话的默认选择。`device` 是设备身份的来源（工作线 F52：每个请求的 `metadata.user_id`，model.md §3.3）。
/// `flags` 是 `--model` 与 `--effort`（cli-args.md §1.4：新会话的档位，覆盖配置的缺省）。
pub(crate) async fn assemble(
    egress: Arc<Egress>,
    platform: Arc<Platform>,
    explicit: Override,
    (flag, effort): (Option<String>, Option<&str>),
    (clock, device): (Arc<dyn Clock>, DeviceIdSource),
) -> Assembled {
    let mut assembled = assemble_model(egress, platform, explicit, (flag, effort), clock).await;
    assembled.model = assembled.model.with_device_id(device);
    assembled
}

async fn assemble_model(
    egress: Arc<Egress>,
    platform: Arc<Platform>,
    explicit: Override,
    (flag, effort): (Option<String>, Option<&str>),
    clock: Arc<dyn Clock>,
) -> Assembled {
    let unselected = |reason: String, limits: Arc<dyn ModelLimits>| Assembled {
        model: ProviderModel::unconfigured(reason),
        selection: ModelSelection {
            provider: String::new(),
            model: flag.clone().unwrap_or_default(),
            reasoning: None,
            connection: None,
        },
        limits,
    };
    let assume = explicit.assume;
    if let Some(endpoint) = anthropic_endpoint(explicit.base_url, explicit.api_key) {
        // 规则先算：没选出模型时 `/model` 还能换（tui.md §6），压缩的上限照样按规则。
        let (capabilities, limits) =
            explicit_rules(platform.site_rules().await, &endpoint.base_url, assume);
        let configured = match (&flag, &explicit.model) {
            (None, None) => platform.configured_default_model().await,
            _ => None,
        };
        let model = match explicit_model(flag.clone(), explicit.model, configured) {
            Ok(model) => model,
            Err(reason) => return unselected(reason, limits),
        };
        let selection = with_effort(
            ModelSelection {
                provider: "anthropic".to_owned(),
                model,
                reasoning: None,
                connection: None,
            },
            effort,
            None,
        );
        let fixed = FixedEndpoint::new(endpoint).with_capabilities(capabilities);
        let model = ProviderModel::new(egress, Arc::new(fixed), clock);
        // 测试声明同时决定方案表（transport 按官方 API 的地址查，model.md §3.2）。
        let model = if assume.first_party {
            model.with_assumed_first_party()
        } else {
            model
        };
        return Assembled {
            model,
            selection,
            limits,
        };
    }
    // 平台层的上限读已发布的快照：这里先取一次目录，启动后的第一次判定就有值。
    let catalog = platform.catalog().await;
    let limits: Arc<dyn ModelLimits> = platform.clone();
    match pick(&catalog, flag.as_deref()) {
        Ok(selection) => Assembled {
            model: ProviderModel::new(egress, platform, clock),
            selection: with_effort(selection, effort, Some(&catalog)),
            limits,
        },
        // 团队项目已过期或未分配：照 App 的文案（platform.md §9.2）。目录为空（没给 `--model`），或 `--model` 指的
        // provider 有这样的团队项目时（工作线 F49：不再看 App 的当前连接）。
        Err(reason) => {
            let provider = flag
                .as_deref()
                .and_then(|flag| flag.trim().split_once('/'))
                .map(|(provider, _)| provider);
            let notice = match provider {
                Some(provider) => platform.team_notice(Some(provider)).await,
                None if catalog.default.is_none() => platform.team_notice(None).await,
                None => None,
            };
            unselected(notice.unwrap_or(reason), limits)
        }
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
