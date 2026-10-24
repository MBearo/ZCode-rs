//! 每种请求的单次超时（修复线 F58，README §4.1）：`HttpRequest.timeout` 只限制等待响应头，响应体靠调用方的取消令牌
//! 结束（model.md §2.2）。按服务各有来源，值只定义在这里；发请求的适配器（model、platform、cli）取这里。
//!
//! 内核 crate 不能依赖 net：WebFetch 的 60 s 在 tools，经执行环境的端口传下来。MCP HTTP 的 POST 没有单次
//! 超时，只有 RPC 级的上限（ext.md §3.2；按 MCP 的配置另算的超时没有做，见 README §11 F58 的“未做”）。

use std::time::Duration;

/// 模型请求（Anthropic 与 OpenAI 两种协议）：600 s（`@anthropic-ai/sdk` 的 `timeout: 600000`）。
pub const MODEL: Duration = Duration::from_secs(600);

/// 账号接口（项目令牌、customerInfo、团队列表与权益、订阅查询）：15 s（Node `coding-plan-api-key.ts`、App
/// `codingPlanProviderAvailability.ts`）。
pub const ACCOUNT: Duration = Duration::from_secs(15);

/// CLI 的 OAuth 登录（init 与轮询）：180 s（Node 的 HTTP 客户端缺省，`adapters/src/http/index.ts`，CLI 登录用它）。
/// 以前没有单次超时，请求挂住时一直等。
pub const OAUTH: Duration = Duration::from_secs(180);

/// npm registry 的版本查询（update.md）：5 s。Node / App 不查 npm，本机没有 Codex 的源码；取常见的版本检查超时
/// 5 s。以前没有单次超时。
pub const NPM_REGISTRY: Duration = Duration::from_secs(5);

#[cfg(test)]
#[path = "timeout_tests.rs"]
mod tests;
