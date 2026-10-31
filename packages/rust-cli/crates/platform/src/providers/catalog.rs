//! 解析结果 → 模型目录与端点模板（docs/specs/rust-cli/platform.md §3.3）。
//!
//! 规则对齐旧 Rust CLI 的 `apps/zcode-cli-rust/crates/model/src/registry_rules.rs`（对齐 Node）：
//! - 可执行的模型都能解析端点；`visibility: hidden` 的 provider 不进目录，但仍能按选择解析；
//! - 推理档位是 `optionSpecs.reasoningLevel.values`；没指定档位时用最后一个（Node `toModelOption`）；
//! - 请求补丁 = reasoningLevel map(档位) + maxOutputTokens map(max)；
//! - 默认选择：Node `resolveInitialModelSelection`——个人配置的 `defaultModelSelection` 可选（档位有效）时用它，
//!   否则目录第一项、最后一个档位；
//! - Team Plan（工作线 F49，platform.md §9.3）：团队 provider 的模型按可用的团队项目展开，每个项目一组，目录项与
//!   选择带团队项目；按选择找端点时团队项目也要相同（选择没带时取这个模型的第一个团队项目）。

use std::collections::HashMap;

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use rust_cli_kernel_api::provider::{
    CatalogConnection, CatalogModel, ModelCapabilities, TokenLimits, WireProtocol,
};
use serde_json::Value;

use super::resolve::Resolved;
use super::rules::{ids, text};
use crate::option_map::evaluate;

/// 一个模型怎样取凭据。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Access {
    /// Provider 配置里的 API key。
    ApiKey(RedactedString),
    /// 官方账号模型：每次请求向账号层取（`access.accountType` / `access.mode`）。
    Account { account_type: String, mode: String },
}

/// 一个可执行模型的端点模板（凭据之外的部分）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Template {
    pub provider: String,
    pub model: String,
    pub protocol: WireProtocol,
    pub base_url: String,
    pub headers: Vec<(String, RedactedString)>,
    pub access: Access,
    pub levels: Vec<String>,
    pub reasoning_map: String,
    pub max_map: String,
    pub max_output_tokens: u64,
    /// 生效配置的能力（修复线 F23，platform.md §3.3）：目录项与端点都从这一份写。
    pub capabilities: ModelCapabilities,
    pub hidden: bool,
    pub provider_name: String,
    /// 团队项目（团队 provider 的模型每个项目一份，工作线 F49）；其他 provider 为 None。
    pub connection: Option<CatalogConnection>,
}

impl Template {
    /// 选择的档位（没指定时用最后一个）对应的请求补丁。`max_output` 是入口读到的 `ZCODE_MAX_OUTPUT_TOKENS`
    /// （cli-args.md §9.1，修复线 F42）与请求交来的上限（`RequestOptions.provider_max_output_tokens`，工作线 F95）的
    /// 较小值：`maxOutputTokens.map` 用它与配置的 max 的较小值（模型目录的上限是模型能力的上限，两者取小）；都没有时用
    /// 配置的 max（Node 主循环的基线）。
    pub fn patches(
        &self,
        reasoning: Option<&str>,
        max_output: Option<u64>,
    ) -> Result<Vec<Value>, ModelError> {
        let level = match reasoning {
            Some(level) if self.levels.iter().any(|known| known == level) => level,
            Some(level) => {
                return Err(ModelError::InvalidRequest {
                    message: format!(
                        "reasoning level {level} is not offered by {}/{}",
                        self.provider, self.model
                    ),
                });
            }
            None => self.levels.last().map(String::as_str).unwrap_or_default(),
        };
        let invalid = |error: anyhow::Error| ModelError::InvalidRequest {
            message: format!("provider option map failed: {error:#}"),
        };
        // GLM 的推理字段由卡片的形状写（工作线 F121，model.md §11.1）：Anthropic 协议上不合并 reasoningLevel 的补丁，否则
        // `thinking` 会被改回配置的写法。输出上限的补丁照常（上限按目录）。
        // 修 bug（F121 的测试查出）：以前不看协议，OpenAI 两种协议的 GLM 也去掉了这个补丁；那两种协议不写卡片的字段
        // （model.md §10），推理只靠这个补丁，去掉后档位不起作用。
        let reasoning = if self.protocol == WireProtocol::AnthropicMessages
            && rust_cli_base::models::is_glm(&self.model)
        {
            Value::Object(serde_json::Map::new())
        } else {
            evaluate(&self.reasoning_map, "reasoningLevel", &Value::from(level)).map_err(invalid)?
        };
        let max_output_tokens = max_output.map_or(self.max_output_tokens, |cap| {
            cap.min(self.max_output_tokens)
        });
        let max = evaluate(
            &self.max_map,
            "maxOutputTokens",
            &Value::from(max_output_tokens),
        )
        .map_err(invalid)?;
        Ok(vec![reasoning, max])
    }

    pub fn catalog_entry(&self) -> CatalogModel {
        CatalogModel {
            provider: self.provider.clone(),
            provider_name: self.provider_name.clone(),
            model: self.model.clone(),
            protocol: self.protocol,
            reasoning_levels: self.levels.clone(),
            context_window: self.capabilities.context_window,
            max_output_tokens: self.max_output_tokens,
            capabilities: self.capabilities,
            connection: self.connection.clone(),
        }
    }
}

/// 一次解析的结果：全部可执行模型的模板与默认选择。
#[derive(Debug, Clone, Default)]
pub(crate) struct Snapshot {
    pub templates: Vec<Template>,
    pub default: Option<ModelSelection>,
    /// 个人配置 `defaultModelSelection` 的模型 ID 原样（不看目录）：显式覆盖没给模型时用它（model.md §6，F42）。
    pub configured_model: Option<String>,
}

impl Snapshot {
    /// 选择对应的模板：provider 与模型相同；选择带团队项目时它也要相同，没带时取第一个（platform.md §9.4）。
    pub fn find(&self, selection: &ModelSelection) -> Option<&Template> {
        let mut candidates = self.templates.iter().filter(|template| {
            template.provider == selection.provider && template.model == selection.model
        });
        match selection.connection.as_deref() {
            Some(wanted) => candidates.find(|template| {
                template
                    .connection
                    .as_ref()
                    .is_some_and(|connection| connection.id == *wanted)
            }),
            None => candidates.next(),
        }
    }

    /// 压缩用的上限（修复线 F61，platform.md §4）：provider 与模型相同的第一个模板（隐藏的也算）。不看团队项目：同一个
    /// 模型在各团队项目下的配置相同，项目过期时窗口也照旧。
    pub fn limits(&self, selection: &ModelSelection) -> Option<TokenLimits> {
        let template = self.templates.iter().find(|template| {
            template.provider == selection.provider && template.model == selection.model
        })?;
        Some(TokenLimits {
            context_window: template.capabilities.context_window,
            max_output_tokens: Some(template.max_output_tokens),
        })
    }

    pub fn catalog(&self) -> Vec<CatalogModel> {
        self.templates
            .iter()
            .filter(|template| !template.hidden)
            .map(Template::catalog_entry)
            .collect()
    }
}

/// 生效配置 `properties` 里的能力（Node 的字段名，docs/specs/rust-cli/model.md §11）。可执行的模型属性齐全
/// （`model_complete`）；缺的布尔按假、窗口按 0（与原来逐项读取相同）。`supports_tool_search` 不是配置字段：工作线 F89
/// 起总为假（docs/specs/rust-cli/tools-more.md §3.2.2：所有端点都用本地 ToolSearch，暂时不用原生）。
///
/// GLM 按 opus 的卡片形状发请求（工作线 F121，用户决定 2026-10-30，model.md §11.1）：对话中间的 system 是形状的一部分，
/// 所以 GLM 的 `supports_mid_conversation_system` 总为真（内置配置给的是假，配置不改）；别的能力照配置。
pub(crate) fn capabilities(model: &str, effective: &Value) -> ModelCapabilities {
    let properties = &effective["properties"];
    let input = &properties["inputFormat"];
    ModelCapabilities {
        context_window: properties["contextWindow"].as_u64().unwrap_or(0),
        supports_tool_call: properties["supportsToolCall"] == true,
        supports_native_web_search: properties["supportsNativeWebSearch"] == true,
        supports_mid_conversation_system: properties["supportsMidConversationSystem"] == true
            || rust_cli_base::models::is_glm(model),
        supports_json_schema_output: properties["supportsJsonSchemaOutput"] == true,
        requires_mfjs_tool_schema: properties["requiresMfjsToolSchema"] == true,
        supports_image: input["supportsImage"] == true,
        supports_pdf: input["supportsPdf"] == true,
        supports_video: input["supportsVideo"] == true,
        supports_audio: input["supportsAudio"] == true,
        // 修改（工作线 F89，用户决定：暂时不用服务器原生的 ToolSearch，含 z.ai / bigmodel）：以前按端点算（F52：协议是
        // anthropic-messages 且 `rule_url::is_zhipu_endpoint`），这些端点用原生。恢复原生时在这里按端点重新算。
        supports_tool_search: false,
        // 工作线 F93：输出上限与模板的 `max_output_tokens`、压缩的 `TokenLimits` 读同一个字段（docs/specs/rust-cli/model.md §11）。
        max_output_tokens: effective["optionSpecs"]["maxOutputTokens"]["max"].as_u64(),
    }
}

fn access(config: &Value) -> Option<Access> {
    let access = &config["access"];
    if access["type"] == "zhipu-account" {
        return Some(Access::Account {
            account_type: text(access, "accountType").to_owned(),
            mode: text(access, "mode").to_owned(),
        });
    }
    access["apiKey"]
        .as_str()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(|key| Access::ApiKey(RedactedString::new(key)))
}

pub(crate) fn snapshot(
    resolved: &Resolved,
    connections: &HashMap<String, Vec<CatalogConnection>>,
) -> Snapshot {
    let mut templates = Vec::new();
    for provider in resolved.providers.iter().filter(|p| p.executable) {
        let config = &provider.effective_config;
        let Some(protocol) = WireProtocol::parse(text(&config["api"], "type")) else {
            continue;
        };
        let Some(access) = access(config) else {
            continue;
        };
        // header 值必须是字符串；有一个不是就跳过这个 provider（与 Node 的 schema 校验相同的结果）。
        let headers: Option<Vec<(String, RedactedString)>> = config["api"]["headers"]
            .as_object()
            .map(|headers| {
                headers
                    .iter()
                    .map(|(name, value)| {
                        value
                            .as_str()
                            .map(|value| (name.clone(), RedactedString::new(value)))
                    })
                    .collect()
            })
            .unwrap_or(Some(Vec::new()));
        let Some(headers) = headers else {
            continue;
        };
        let mut models = Vec::new();
        for model in provider.models.iter().filter(|m| m.executable) {
            let effective = &model.effective_config;
            let specs = &effective["optionSpecs"];
            let base_url = text(&config["api"], "baseUrl");
            models.push(Template {
                provider: provider.provider_id.clone(),
                model: model.model_id.clone(),
                protocol,
                base_url: base_url.to_owned(),
                headers: headers.clone(),
                access: access.clone(),
                levels: ids(&specs["reasoningLevel"]["values"]),
                reasoning_map: text(&specs["reasoningLevel"], "map").to_owned(),
                max_map: text(&specs["maxOutputTokens"], "map").to_owned(),
                max_output_tokens: specs["maxOutputTokens"]["max"].as_u64().unwrap_or(0),
                capabilities: capabilities(&model.model_id, effective),
                hidden: provider.hidden,
                provider_name: provider
                    .provider_name
                    .clone()
                    .unwrap_or_else(|| provider.provider_id.clone()),
                connection: None,
            });
        }
        // 团队 provider：每个可用的团队项目一组（项目在外、模型在内）；没有可用项目时 overlay 已不授权。
        match connections.get(&provider.provider_id) {
            Some(teams) => {
                for team in teams {
                    templates.extend(models.iter().map(|template| Template {
                        connection: Some(team.clone()),
                        ..template.clone()
                    }));
                }
            }
            None => templates.extend(models),
        }
    }
    let default = default_selection(&templates, &resolved.default_selection);
    Snapshot {
        templates,
        default,
        configured_model: configured_model(&resolved.default_selection),
    }
}

/// `defaultModelSelection.modelId`（去掉首尾空白、非空）：Node `saveConfiguredDefault` 写的字段。显式覆盖的端点不在
/// 目录里，所以不看 provider、档位与“可选”（Node `resolveInitialModelSelection` 的判断只对目录里的模型有意义）。
pub(crate) fn configured_model(configured: &Value) -> Option<String> {
    configured["modelId"]
        .as_str()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
}

/// Node `resolveInitialModelSelection`（docs/specs/rust-cli/workspace-trust.md §3.1）：配置的缺省“可选”时原样用它
/// ——provider 不隐藏、模型在目录里、`options.reasoningLevel` 有值且在模型的档位里；否则目录第一项（第一个非隐藏
/// provider 的第一个有档位的模型）、最后一个档位（Node `completeNewModelSelection`）。
///
/// L31 修正：之前缺省没有档位或档位无效时仍用这个模型（取最后一个档位），与 Node 不同——Node 判它不可选、回到
/// 目录第一项（登录写的 `{providerId, modelId}` 也是这样），Node CLI 与本工程的新会话会选到不同的模型。
fn default_selection(templates: &[Template], configured: &Value) -> Option<ModelSelection> {
    let visible = || templates.iter().filter(|template| !template.hidden);
    let selectable = visible().find_map(|template| {
        let level = configured["options"]["reasoningLevel"].as_str()?;
        (configured["providerId"] == template.provider.as_str()
            && configured["modelId"] == template.model.as_str()
            && template.levels.iter().any(|known| known == level))
        .then(|| (template, level.to_owned()))
    });
    let (template, reasoning) = match selectable {
        Some(found) => found,
        None => visible().find_map(|template| Some((template, template.levels.last()?.clone())))?,
    };
    Some(ModelSelection {
        provider: template.provider.clone(),
        model: template.model.clone(),
        reasoning: Some(reasoning),
        // 指向团队 provider 时是这个模型的第一个团队项目（缺省不存团队项目，D-F49-3）。
        connection: template
            .connection
            .as_ref()
            .map(|connection| Box::new(connection.id.clone())),
    })
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tool_search_tests.rs"]
mod tool_search_tests;
