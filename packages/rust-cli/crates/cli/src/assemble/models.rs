//! 模型的组装（docs/specs/rust-cli/model.md §6、platform.md §1）：显式覆盖或平台层。
//!
//! - `ZCODE_API_KEY` 非空：显式覆盖（本地对照测试依赖它），Anthropic 端点 `ZCODE_MODEL_BASE_URL`
//!   （空白时官方 API），模型依次取 `--model`、`ZCODE_MODEL`、个人 Provider 配置的 `defaultModelSelection`。
//!   名字都是 `ZCODE_*`（修复线 F42，config.md §2.5），Anthropic SDK 的 `ANTHROPIC_*` 不读。
//! - 否则：平台层的 Provider registry。`--model` 是 `<provider>/<model>` 或目录里的模型 ID（取第一个
//!   匹配），没有时用目录的默认选择。
//!
//! 选不出模型时不在启动时失败：模型端口对每次请求报鉴权错误并说明原因（与没有配置时的 M0 行为相同）。

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_base::TaskScope;

use rust_cli_base::names::DOT_DIR;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::default_model::{DefaultModel, unselected as unselected_default};
use rust_cli_kernel_api::model::{ModelPort, ModelSelection};
use rust_cli_kernel_api::model_name::{self, LookupError};
use rust_cli_kernel_api::provider::{
    ModelCatalog, ModelDirectory, ModelEndpoints, ModelLimits, TokenLimits, WireProtocol,
};
use rust_cli_model::{
    AnthropicEndpoint, CapabilityRules, FixedEndpoint, ProviderModel, RouteOptions,
    UnconfiguredModel,
};
use rust_cli_net::Egress;
use rust_cli_paths::StoragePath;
use rust_cli_platform::{Platform, SiteRules};

use crate::assemble::options::Assumptions;

/// 显式覆盖用的环境变量（启动时捕获，controller 的事实）。只给测试的声明不在这里（`facts::TestHooks`，F129）。
pub(crate) struct Override {
    pub base_url: Option<String>,
    pub api_key: Option<RedactedString>,
    pub model: Option<String>,
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
    assume: &Assumptions,
) -> (CapabilityRules, Arc<dyn ModelLimits>) {
    let url = if assume.first_party {
        OFFICIAL_RULE_URL.to_owned()
    } else {
        rule_base_url(base_url)
    };
    let limits = Arc::new(ExplicitLimits {
        rules: Arc::new(rules),
        url,
        rule_model: assume.rule_model.clone(),
    });
    let shared = limits.clone();
    let tool_search = assume.tool_search;
    let capabilities: CapabilityRules = Arc::new(move |model: &str| {
        let mut capabilities = shared.rules.capabilities(
            WireProtocol::AnthropicMessages,
            &shared.url,
            shared.rule_model.as_deref().unwrap_or(model),
        );
        capabilities.supports_tool_search |= tool_search;
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
    /// 只给测试用的 `_ZCODE_ASSUME_RULE_MODEL`（工作线 F121）：按这个模型 ID 查规则。
    rule_model: Option<String>,
}

impl ModelLimits for ExplicitLimits {
    fn limits(&self, selection: &ModelSelection) -> Option<TokenLimits> {
        let model = self.rule_model.as_deref().unwrap_or(&selection.model);
        self.rules
            .limits(WireProtocol::AnthropicMessages, &self.url, model)
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
    crate::commands::effort::apply(&mut selection, level, offered.as_deref());
    selection
}

/// 新会话的缺省选择：现在就有，或要等平台层的模型目录（工作线 F140，performance.md §12.3）。
pub(crate) enum Chosen {
    Now(ModelSelection),
    /// 目录要联网查套餐：TUI 不等它画第一帧（[`default_model`]），`-p` 与 App 模式当场等（[`Chosen::wait`]）。选不出
    /// 模型时，原因写进模型端口（[`Assembled::late`]）。`flags` 是选模型的参数（上次的结论按它对得上才用，§12.4）。
    Later {
        flags: memory::Flags,
        choosing: BoxFuture<'static, ModelSelection>,
    },
}

/// 组装的结果：端点来源（选不出模型时是给用户的原因）、新会话的默认选择与压缩的上限查询（修复线 F61：两种模式都有，
/// compact.md §2）。
pub(crate) struct Assembled {
    pub endpoints: Result<Arc<dyn ModelEndpoints>, String>,
    pub selection: Chosen,
    pub limits: Arc<dyn ModelLimits>,
    /// 测试声明“按官方端点处理”（`_ZCODE_ASSUME_FIRST_PARTY_BASE_URL`，model.md §6）只对显式覆盖的端点生效：它同时决定
    /// 方案表（transport 按官方 API 的地址查，model.md §3.2）。
    pub assume_first_party: bool,
    /// 平台层的选择晚到（工作线 F140）：选不出模型时，选模型的任务把原因写在这里，模型端口之后的每次请求都报它。端口在
    /// [`Assembled::port`] 才建，比选择早，所以两边共用这一格。
    pub late: Arc<OnceLock<String>>,
}

impl Assembled {
    /// 模型端口：选出了端点时是 `ProviderModel`（带入口级的选项），否则每次请求都报原因（F129，审查 E10）。
    pub(crate) fn port(
        &self,
        egress: Arc<Egress>,
        clock: Arc<dyn Clock>,
        options: RouteOptions,
    ) -> Arc<dyn ModelPort> {
        match &self.endpoints {
            Ok(endpoints) => Arc::new(
                ProviderModel::new(
                    egress,
                    endpoints.clone(),
                    clock,
                    RouteOptions {
                        assume_first_party: self.assume_first_party,
                        ..options
                    },
                )
                .with_late_reason(self.late.clone()),
            ),
            Err(reason) => Arc::new(UnconfiguredModel::new(reason.clone())),
        }
    }
}

/// 选不出模型时新会话的选择：没有 provider，模型是 `--model` 的原样（`/model` 还能换，tui.md §6）。
fn unselected(flag: Option<String>) -> ModelSelection {
    ModelSelection::new(String::new(), flag.unwrap_or_default())
}

/// 端点来源与新会话的默认选择。`flags` 是 `--model` 与 `--effort`（cli-args.md §1.4：新会话的档位，覆盖配置的缺省）。
///
/// `assume` 是只给测试的声明（model.md §6）。F129：以前显式覆盖与声明一起被移进来，入口要在这之前读完别的声明。
/// 平台层的选择不在这里等（工作线 F140）：要联网查套餐，交给 [`default_model`]。
pub(crate) async fn assemble(
    platform: Arc<Platform>,
    explicit: &Override,
    assume: &Assumptions,
    (flag, effort): (Option<String>, Option<&'static str>),
) -> Assembled {
    let unselected_now = |reason: String, limits: Arc<dyn ModelLimits>| Assembled {
        endpoints: Err(reason),
        selection: Chosen::Now(unselected(flag.clone())),
        limits,
        assume_first_party: false,
        late: Arc::default(),
    };
    if let Some(endpoint) = anthropic_endpoint(explicit.base_url.clone(), explicit.api_key.clone())
    {
        // 规则先算：没选出模型时 `/model` 还能换（tui.md §6），压缩的上限照样按规则。
        let (capabilities, limits) =
            explicit_rules(platform.site_rules().await, &endpoint.base_url, assume);
        let configured = match (&flag, &explicit.model) {
            (None, None) => platform.configured_default_model().await,
            _ => None,
        };
        let model = match explicit_model(flag.clone(), explicit.model.clone(), configured) {
            Ok(model) => model,
            Err(reason) => return unselected_now(reason, limits),
        };
        let selection = with_effort(
            ModelSelection::new("anthropic".to_owned(), model),
            effort,
            None,
        );
        let fixed = FixedEndpoint::new(endpoint).with_capabilities(capabilities);
        return Assembled {
            endpoints: Ok(Arc::new(fixed)),
            selection: Chosen::Now(selection),
            limits,
            assume_first_party: assume.first_party,
            late: Arc::default(),
        };
    }
    // 平台层的上限读目录发布的快照：会话要等缺省模型定下（即目录取到之后）才建，第一次判定时已有值。
    let limits: Arc<dyn ModelLimits> = platform.clone();
    let late: Arc<OnceLock<String>> = Arc::default();
    let flags = (flag.clone(), effort);
    let choosing = {
        let (platform, late) = (platform.clone(), late.clone());
        async move {
            match choose(&platform, flag.as_deref(), effort).await {
                Ok(selection) => selection,
                Err(reason) => {
                    let _ = late.set(reason);
                    unselected(flag)
                }
            }
        }
    };
    Assembled {
        endpoints: Ok(platform),
        selection: Chosen::Later {
            flags,
            choosing: choosing.boxed(),
        },
        limits,
        assume_first_party: false,
        late,
    }
}

/// 平台层的缺省选择：取模型目录（每次启动联网查套餐）、按 `--model` 选、写上档位。选不出时是原因：团队项目已过期或
/// 未分配时照 App 的文案（platform.md §9.2）——目录为空（没给 `--model`），或 `--model` 指的 provider 有这样的团队项目时
///（工作线 F49：不再看 App 的当前连接）。
async fn choose(
    platform: &Platform,
    flag: Option<&str>,
    effort: Option<&str>,
) -> Result<ModelSelection, String> {
    let catalog = platform.catalog().await;
    let reason = match pick(&catalog, flag) {
        Ok(selection) => return Ok(with_effort(selection, effort, Some(&catalog))),
        Err(reason) => reason,
    };
    let provider = flag
        .and_then(|flag| flag.trim().split_once('/'))
        .map(|(provider, _)| provider);
    let notice = match provider {
        Some(provider) => platform.team_notice(Some(provider)).await,
        None if catalog.default.is_none() => platform.team_notice(None).await,
        None => None,
    };
    Err(notice.unwrap_or(reason))
}

/// 缺省模型（performance.md §12.3、§12.4）：现在就有的、或 `wait`（`-p`）时当场定下；否则先用上次的结论（`memory`
/// 文件）当暂定值，在返回的任务作用域里等目录，定下后记下这次的结论。定下时记 `startup.model`（logging.md §4，
/// `started` 是进程启动的时刻）。
pub(crate) async fn default_model(
    chosen: Chosen,
    memory: StoragePath,
    wait: bool,
    started: Instant,
) -> (DefaultModel, TaskScope) {
    let scope = TaskScope::root("default-model", |_| {});
    let memory = memory.as_path().to_path_buf();
    let settled = move |selection: ModelSelection, guessed: bool| {
        tracing::info!(
            event = "startup.model",
            since_start_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            chosen = !selection.provider.is_empty(),
            guessed,
            "default model settled"
        );
        selection
    };
    let (flags, choosing) = match chosen {
        Chosen::Now(selection) => return (settled(selection, false).into(), scope),
        Chosen::Later { flags, choosing } => (flags, choosing),
    };
    if wait {
        let selection = settled(choosing.await, false);
        memory::remember(&memory, &flags, &selection).await;
        return (selection.into(), scope);
    }
    let guess = memory::recall(&memory, &flags).await;
    let guessed = guess.is_some();
    let (settle, default) = DefaultModel::guessed(guess.unwrap_or_else(unselected_default));
    let _task = scope.spawner().spawn("default-model", async move {
        let selection = settled(choosing.await, guessed);
        settle.settle(selection.clone());
        memory::remember(&memory, &flags, &selection).await;
    });
    (default, scope)
}

#[path = "models_memory.rs"]
mod memory;

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
