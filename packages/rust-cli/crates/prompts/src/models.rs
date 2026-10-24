//! 模型卡片（`packages/rust-cli/prompts/models.json`，docs/specs/rust-cli/context.md §5）。
//!
//! 模型身份段要用的营销名与知识截止时间是给模型看的文本；其余字段是该
//! 模型的请求默认值（输出上限、思考方式、effort、是否支持对话中间的 system 消息）。
//!
//! 卡片与生效配置的分工（修复线 F23，用户决定 A5，docs/specs/rust-cli/model.md §11）：能力（对话中间的 system、
//! 上下文窗口、媒体、WebSearch）只取 Node 的生效模型配置；卡片只留配置里没有的内容（身份段、请求
//! 默认值、silent turn 提醒、任务工具）。`system_turns` 与 `context_window` 只在模型端口答不出能力时
//! 作回退（[`mid_conversation_system`]、[`context_window`]）。

use std::sync::OnceLock;

use rust_cli_base::include_lf;
use serde::Deserialize;

/// 没有卡片的模型的缺省输出上限（32000）。主循环、prompt hook、`/rename` 都用它（修复线 F58：以前四处各写一份；
/// prompt hook 的注释写成“与旁路请求相同”，旁路是 8192，实际值一直是这个）。WebSearch 在工作线 F90 起照 Node 用 4096。
pub const UNKNOWN_MAX_OUTPUT_TOKENS: u32 = 32_000;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelCard {
    pub id: String,
    /// `You are powered by the model named …` 里的名字；没有时只说模型 ID。
    pub marketing_name: Option<String>,
    pub knowledge_cutoff: Option<String>,
    pub max_output_tokens: u32,
    pub context_window: u64,
    /// `thinking: {type: adaptive, display: updates}`。
    pub adaptive_thinking: bool,
    /// `output_config.effort` 的默认值。
    pub default_effort: Option<String>,
    /// 模型接受对话中间的 `role: system` 消息。
    pub system_turns: bool,
    /// 模型接受挂在 system 消息上的每轮 effort（beta `per-turn-control`）。
    pub per_turn_effort: bool,
    /// 连续几轮没有可见输出时提醒向用户说明进展（按模型开启的 `silent_turn_reminder`）。
    #[serde(default)]
    pub silent_turn_reminder: bool,
}

#[derive(Deserialize)]
struct Catalog {
    models: Vec<ModelCard>,
}

fn catalog() -> &'static [ModelCard] {
    static CATALOG: OnceLock<Vec<ModelCard>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str::<Catalog>(include_lf!("../../../prompts/models.json"))
            .map(|catalog| catalog.models)
            .unwrap_or_default()
    })
}

/// 按模型 ID 查卡片。先去掉 `[1m]` 这类上下文后缀再查。
pub fn card(model: &str) -> Option<&'static ModelCard> {
    let id = model.split_once('[').map_or(model, |(id, _)| id);
    catalog().iter().find(|card| card.id == id)
}

/// 主循环等请求里 `output_config.effort` 的值（修复线 F23，model.md §11）：有卡片时是选择的档位，没有档位时是卡片的缺省；
/// 没有卡片时没有——推理字段只由配置的 option map 写（同 Node）。
///
/// 修 bug：以前选择的档位优先、不看卡片，没有卡片的模型（例如 GLM-5.1 的 `enabled` 档位）也把档位写进
/// `output_config.effort`，而它的 option map 只写 `thinking`，请求体留下 `"effort":"enabled"`。
pub fn card_effort(model: &str, reasoning: Option<&str>) -> Option<String> {
    let card = card(model)?;
    reasoning
        .map(str::to_owned)
        .or_else(|| card.default_effort.clone())
}

/// 模型是否接受对话中间的 `role: system` 消息：`configured` 是生效配置的 `supportsMidConversationSystem`
/// （模型端口答得出时，唯一来源）；答不出时（测试替身、没有配置）按卡片的 `system_turns` 回退（model.md §11）。
pub fn mid_conversation_system(model: &str, configured: Option<bool>) -> bool {
    configured.unwrap_or_else(|| card(model).is_some_and(|card| card.system_turns))
}

/// 没有卡片、配置也没给窗口时的上下文窗口（压缩的窗口与 ToolSearch 的自动门槛共用，docs/specs/rust-cli/compact.md §2）。
pub const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

/// 模型的上下文窗口：`configured` 是生效配置的 `contextWindow`（大于 0 时用它）；否则卡片，再没有时
/// [`DEFAULT_CONTEXT_WINDOW`]（同 [`mid_conversation_system`] 的回退）。
pub fn context_window(model: &str, configured: Option<u64>) -> u64 {
    configured
        .filter(|window| *window > 0)
        .unwrap_or_else(|| card(model).map_or(DEFAULT_CONTEXT_WINDOW, |card| card.context_window))
}

/// ToolSearch 的形式（docs/specs/rust-cli/tools-more.md §3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSearchMode {
    /// 不开：全部工具内联。
    Off,
    /// 本地：端点不支持 `tool_reference`。搜索照样在本机做；发出的请求里已加载的工具是普通定义，ToolSearch 的结果
    /// 换成文字，没有 `defer_loading`、占位工具与 `advanced-tool-use`。
    Local,
    /// 原生：端点支持延迟加载（`defer_loading`、`tool_reference`、占位工具与 `advanced-tool-use`）。
    Native,
}

impl ToolSearchMode {
    /// 这一步开着（原生或本地）。
    pub fn is_on(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// ToolSearch 开着时的形式（docs/specs/rust-cli/tools-more.md §3.2.2）：`configured` 是端点的能力 `supports_tool_search`，
/// 为真时原生，否则本地；开不开由 [`ToolSearchSetting`] 与这一步的工具决定。主循环与子代理发请求、`/context` 的消息计数
/// 共用这一个判定。
///
/// 修改（工作线 F89，用户决定：所有模型、所有端点都用本地，暂时不用原生）：端点的能力在产品路径上总为假（平台层不再按
/// 端点算），只有 debug 构建读的测试声明把它设为真；不再看模型。以前禁用表里的模型（`claude-3-5-haiku`、
/// `claude-3-haiku`）不开，端点答不出能力时按卡片回退成原生；F84 时 z.ai / bigmodel 的端点原生、其余本地。
pub fn tool_search_mode(configured: Option<bool>) -> ToolSearchMode {
    if configured == Some(true) {
        ToolSearchMode::Native
    } else {
        ToolSearchMode::Local
    }
}

/// `ENABLE_TOOL_SEARCH` 的取值（docs/specs/rust-cli/tools-more.md §3.2.1，工作线 F89）：入口读一次，经 runtime 交给内核。
/// 只决定开不开，开时的形式见 [`tool_search_mode`]。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolSearchSetting {
    /// 开（没设时的缺省）。
    #[default]
    On,
    /// 关：ToolSearch 工具不启用，全部工具内联。
    Off,
    /// 自动（`auto` / `auto:N`）：这一步延迟工具的大小达到上下文窗口的 `percent`%（1–99）时开。
    Auto { percent: u8 },
}

impl ToolSearchSetting {
    /// 乐观判断：关以外都算开（ToolSearch 工具启用、stream-json 的 MCP prewait）。
    pub fn allows(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// 这一步开不开：自动时延迟工具的字符数（名字、描述与 schema 的 UTF-16 长度之和，只在自动时算）达到
    /// `floor(floor(窗口 × N / 100) × 2.5)`；`window` 是模型的上下文窗口（[`context_window`]）。
    pub fn reached(self, window: u64, deferred_chars: impl FnOnce() -> u64) -> bool {
        match self {
            Self::On => true,
            Self::Off => false,
            Self::Auto { percent } => {
                let tokens = window.saturating_mul(u64::from(percent)) / 100;
                deferred_chars() >= tokens.saturating_mul(5) / 2
            }
        }
    }
}

/// 按 4 字符 / token 估算的旧模型。
const FOUR_CHARS_PER_TOKEN: [&str; 14] = [
    "claude-3-opus",
    "claude-3-sonnet",
    "claude-3-haiku",
    "claude-3-5-sonnet",
    "claude-3-5-haiku",
    "claude-3-7-sonnet",
    "claude-opus-4-0",
    "claude-opus-4-1",
    "claude-opus-4-5",
    "claude-opus-4-6",
    "claude-sonnet-4-0",
    "claude-sonnet-4-5",
    "claude-sonnet-4-6",
    "claude-haiku-4-5",
];

/// 旧模型 ID 之后允许的部分：没有、日期后缀（`-20241022`）或 Bedrock / Vertex 的版本后缀（`-v2:0`、`@…`）。
fn legacy_suffix(rest: &str) -> bool {
    rest.is_empty()
        || rest.starts_with(':')
        || rest.starts_with('@')
        || rest.starts_with("-v")
        || rest
            .strip_prefix('-')
            .is_some_and(|tail| tail.len() >= 8 && tail[..8].bytes().all(|b| b.is_ascii_digit()))
}

/// 本地估算的每 token 字符数（context-usage.md §3）：只看模型
/// ID。规范化（小写、去掉 `[1m]` 这类后缀、`.` 与 `_` 换成 `-`）后是 14 个旧模型之一时是 4，其余是 3；没有模型时
/// 是 4。没有模型目录（Bedrock / Vertex 的 ID、`behaves_as` 不经目录规范化），按 ID 里的旧模型名认。
pub fn chars_per_token(model: &str) -> u64 {
    if model.is_empty() {
        return 4;
    }
    let id = model.split_once('[').map_or(model, |(id, _)| id);
    let id = id.to_ascii_lowercase().replace(['.', '_'], "-");
    let legacy = FOUR_CHARS_PER_TOKEN.iter().any(|legacy| {
        id.match_indices(legacy).any(|(at, _)| {
            let before = id[..at].chars().next_back();
            before.is_none_or(|c| c == '-' || c == '/') && legacy_suffix(&id[at + legacy.len()..])
        })
    });
    if legacy { 4 } else { 3 }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
