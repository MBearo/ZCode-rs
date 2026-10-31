//! 主机与地址（工作线 F121，docs/specs/rust-cli/README.md §4.1 第 6 条）：模型端点、账号服务、npm 注册表等容易变的
//! 地址只定义在这里，产品代码与测试都引用这里的常量。
//!
//! 放在 base：所有 crate 都能依赖它（README §3.1）。边界脚本（`scripts/rust-cli/boundaries/literals.mjs`）拒绝本文件
//! 以外的字符串字面量写这些主机名（解析器的输入样本等例外登记在 `policy.mjs` 的 `VOLATILE_EXCEPTIONS`）。
//!
//! 测试环境的主机（`*.z.ai`、`bigmodel.cn`）在导出到开源仓库时由共用的文本改写换成生产主机
//! （docs/specs/rust-cli/export.md §8）。

/// Anthropic 官方 API 的 origin，给 `concat!` 用（`const` 里拼地址）；别处写 [`ANTHROPIC_API`]。
#[macro_export]
macro_rules! anthropic_api {
    () => {
        "https://api.anthropic.com"
    };
}

/// Anthropic 官方 API 的主机名。
pub const ANTHROPIC_API_HOST: &str = "api.anthropic.com";
/// Anthropic 官方 API 的 origin（SDK 的缺省 base URL）。
pub const ANTHROPIC_API: &str = anthropic_api!();
/// Node 配置里 Anthropic 官方 API 的写法（带 `/v1`）。
pub const ANTHROPIC_API_V1: &str = concat!(anthropic_api!(), "/v1");

/// 智谱的两个根域名（Node / App `MODEL_PROVIDER_FAMILY_SPECS` 的 `rootDomain`）。
pub const ZAI_DOMAIN: &str = "z.ai";
pub const BIGMODEL_DOMAIN: &str = "bigmodel.cn";

/// z.ai 的业务与项目接口（生产）。
pub const ZAI_API: &str = "https://api.z.ai";
/// z.ai 业务接口的测试环境。
pub const ZAI_API_TEST: &str = "https://api.z.ai";
/// z.ai 的 Anthropic 协议端点。
pub const ZAI_ANTHROPIC: &str = "https://api.z.ai/api/anthropic";
/// z.ai 的 OpenAI 协议端点。
pub const ZAI_OPENAI: &str = "https://api.z.ai/api/paas/v4";
/// z.ai 管理团队套餐的页面（App `teamCodingPlanManageUrl`，没有测试环境的变体）。
pub const ZAI_SUBSCRIPTION: &str = "https://z.ai/manage-apikey/subscription";
/// ZCode 服务（CLI OAuth 登录与套餐网关）的 origin。
pub const ZCODE_ORIGIN: &str = "https://zcode.z.ai";
/// ZCode 服务的测试环境。
pub const ZCODE_ORIGIN_TEST: &str = "https://zcode.z.ai";
/// ZCode 套餐网关上 Anthropic 协议的三条路（`zcode-plan`、`highspeed`、`off-peak`）。
pub const ZCODE_PLAN_ANTHROPIC: &str = "https://zcode.z.ai/api/v1/zcode-plan/anthropic";
pub const ZCODE_HIGHSPEED_ANTHROPIC: &str = "https://zcode.z.ai/api/v1/highspeed/anthropic";
pub const ZCODE_OFF_PEAK_ANTHROPIC: &str = "https://zcode.z.ai/api/v1/off-peak/anthropic";

/// BigModel 账号服务的 origin。
pub const BIGMODEL_ORIGIN: &str = "https://bigmodel.cn";
/// BigModel 账号服务的测试环境。
pub const BIGMODEL_ORIGIN_TEST: &str = "https://bigmodel.cn";
/// BigModel 开放平台的 Anthropic 协议端点。
pub const BIGMODEL_ANTHROPIC: &str = "https://open.bigmodel.cn/api/anthropic";
/// BigModel 开放平台的 OpenAI 协议端点。
pub const BIGMODEL_OPENAI: &str = "https://open.bigmodel.cn/api/paas/v4";

/// DeepSeek 的 Anthropic 协议端点。
pub const DEEPSEEK_ANTHROPIC: &str = "https://api.deepseek.com/anthropic";

/// OpenAI 官方 API。
pub const OPENAI_API: &str = "https://api.openai.com/v1";

/// OpenRouter 的根域名与 API。
pub const OPENROUTER_DOMAIN: &str = "openrouter.ai";
pub const OPENROUTER_API: &str = "https://openrouter.ai/api/v1";

/// OpenCode 的根域名。
pub const OPENCODE_DOMAIN: &str = "opencode.ai";
/// OpenCode Go 的端点（模型请求另带 `x-opencode-session`）。
pub const OPENCODE_GO: &str = "https://opencode.ai/zen/go/v1";

/// npm 官方注册表。
pub const NPM_REGISTRY: &str = "https://registry.npmjs.org";
