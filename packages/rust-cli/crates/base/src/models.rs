//! 模型 ID 与系列规则（工作线 F121，docs/specs/rust-cli/README.md §4.1 第 6 条）：容易变的模型名只定义在这里，产品代码的
//! 判断、缺省值与拼接，测试的模型都引用这里的常量。
//!
//! 优先级（用户决定 2026-10-30）：GLM 是第一方，其次 DeepSeek，再其次 Anthropic、GPT。测试与示例的缺省模型是
//! [`GLM_5_3`]；只有测 Anthropic 模型专有的规则时才用这一组的常量。
//!
//! 边界脚本（`scripts/rust-cli/boundaries/literals.mjs`）拒绝本文件以外的字符串字面量写模型 ID（解析器的输入样本等
//! 例外登记在 `policy.mjs` 的 `VOLATILE_EXCEPTIONS`）。

// ── GLM（第一方）──

/// GLM 模型 ID 的前缀（不分大小写）：这些模型按 opus 的卡片形状发请求（docs/specs/rust-cli/model.md §11.1）。
pub const GLM_PREFIX: &str = "glm-";
/// 测试与示例的缺省模型（内置目录里有：窗口 1,000,000、输出上限 128,000）。
pub const GLM_5_3: &str = "glm-5.3";
pub const GLM_5_3_FLASH: &str = "glm-5.3-flash";
pub const GLM_5_1: &str = "glm-5.1";
pub const GLM_5: &str = "glm-5";
pub const GLM_5_TURBO: &str = "glm-5-turbo";
pub const GLM_4_6: &str = "glm-4.6";
pub const GLM_4_5: &str = "glm-4.5";
/// 内置目录里窗口与输出上限较小的一个（`contextWindow` 131,072、`maxOutputTokens.max` 16,384）。
pub const GLM_4_FLASH: &str = "glm-4-flash-250414";
/// 内置目录里的视觉模型。
pub const GLM_4V_FLASH: &str = "glm-4v-flash";
/// 账号 provider（内置配置的 `zhipu-account`）里的写法：大写的 ID。
pub const GLM_5_3_ACCOUNT: &str = "GLM-5.3";
pub const GLM_5_3_FLASH_ACCOUNT: &str = "GLM-5.3-Flash";
pub const GLM_5_ACCOUNT: &str = "GLM-5";
pub const GLM_4_6_ACCOUNT: &str = "GLM-4.6";
pub const GLM_4_ACCOUNT: &str = "GLM-4";
/// GLM 借这张卡片的行为形状（model.md §11.1）。
pub const GLM_SHAPE_CARD: &str = OPUS_5_5;

/// 模型 ID 以 [`GLM_PREFIX`] 开头（ASCII 不分大小写）。
pub fn is_glm(model: &str) -> bool {
    model
        .get(..GLM_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(GLM_PREFIX))
}

// ── DeepSeek ──

pub const DEEPSEEK_V4_PRO: &str = "deepseek-v4-pro";

// ── Anthropic ──

/// 有模型卡片的模型（`packages/rust-cli/prompts/models.json`）。
pub const OPUS_5_5: &str = "claude-opus-5-5";
pub const OPUS_5: &str = "claude-opus-5";
/// 带日期的 4.5 系列（SDK 能力表里支持 structured output 的一个）。
pub const SONNET_4_5_DATED: &str = "claude-sonnet-4-5-20250929";
pub const OPUS_4_1: &str = "claude-opus-4-1";
pub const SONNET_4_0: &str = "claude-sonnet-4-0";
/// 旧模型（没有卡片；[`ANTHROPIC_FOUR_CHARS_PER_TOKEN`] 里的一个，工作线 F89 之前也在 ToolSearch 的禁用表里）。
pub const HAIKU_3_5_DATED: &str = "claude-3-5-haiku-20241022";

/// 本地估算时按 4 字符 / token 算的旧模型（context-usage.md §3），按 ID 里的名字认（带日期、Bedrock / Vertex 的后缀）。
pub const ANTHROPIC_FOUR_CHARS_PER_TOKEN: [&str; 14] = [
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

/// AI SDK 3.0.81 的能力表里支持 structured output 的模型（按子串认）：兼容端点上带普通工具时发
/// `structured-outputs-2025-11-13`（docs/specs/rust-cli/model.md §3.1）。
pub const ANTHROPIC_STRUCTURED_OUTPUT_MODELS: [&str; 8] = [
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-sonnet-4-6",
    "claude-opus-4-6",
    "claude-sonnet-4-5",
    "claude-opus-4-5",
    "claude-haiku-4-5",
    "claude-opus-4-1",
];

/// 连续过载后不再重试的系列（按 ID 里的系列名认，含 Bedrock 的 `us.anthropic.claude-opus-…`，model.md §5.2）。
pub const ANTHROPIC_REPEATED_OVERLOAD_FAMILIES: [&str; 3] =
    ["claude-opus", "claude-fable", "claude-mythos"];

// ── GPT ──

pub const GPT_5: &str = "gpt-5";
pub const GPT_5_4: &str = "gpt-5.4";
pub const GPT_5_6: &str = "gpt-5.6";
/// OpenAI SDK 不当作推理模型的 GPT-5 变体前缀。
pub const GPT_5_CHAT: &str = "gpt-5-chat";
/// OpenAI SDK 当作推理模型的前缀（system 用 developer 角色，docs/specs/rust-cli/model.md §10.2）。
pub const OPENAI_REASONING_PREFIXES: [&str; 4] = ["o1", "o3", "o4-mini", GPT_5];

// ── 别的厂商 ──

pub const KIMI_K3: &str = "kimi-k3";
pub const QWEN_3_8_MAX: &str = "qwen3.8-max";
