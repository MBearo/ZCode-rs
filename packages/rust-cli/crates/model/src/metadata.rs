//! 请求的 `metadata.user_id`（docs/specs/rust-cli/model.md §3.3，工作线 F52，用户决定 2026-10-22：“请求里有个 metadata
//! 字段，也得补上，只要 user_id”）。只有 Anthropic 协议发（工作线 F52b，用户决定“算了，只给 anthropic 协议加吧”）。
//!
//! 文本照 Node `createAnthropicRequestMetadataUserId`（`apps/zcode-cli/packages/adapters/src/model/
//! anthropic-request-metadata.ts`）：`JSON.stringify({device_id, account_uuid: "", session_id})`；`session_id` 去掉内部前缀
//! （Node `normalizeModelSessionIdForAttribution`，`runner-attribution.ts` 81–91）。

use futures_util::future::BoxFuture;
use std::sync::Arc;

/// 设备身份的来源（入口注入 platform 的 `DeviceMid`）：第一次调用可能读写 `telemetry-state.json`，之后同一个值；
/// 出错时也给出一个值（本进程生成的），不挡请求。
pub type DeviceIdSource = Arc<dyn Fn() -> BoxFuture<'static, String> + Send + Sync>;

/// Node `SESSION_ID_INTERNAL_PREFIX`、`SESSION_ID_SUBAGENT_PREFIX`，按这个顺序各去一次。
const SESSION_PREFIXES: [&str; 2] = ["sess_", "subagent_agent_"];

/// Node `stripHeaderInternalPrefixes`：按顺序各去一次，去掉后还有内容时才去。每请求头的会话与回合也用它（F64）。
pub(crate) fn strip_prefixes<'a>(value: &'a str, prefixes: &[&str]) -> &'a str {
    prefixes
        .iter()
        .fold(value, |value, prefix| match value.strip_prefix(prefix) {
            Some(rest) if !rest.is_empty() => rest,
            _ => value,
        })
}

/// 会话 ID 去掉内部前缀（Node `normalizeModelSessionIdForAttribution`）：`metadata.user_id` 与 `x-session-id` 共用。
pub(crate) fn external_session(session: &str) -> &str {
    strip_prefixes(session, &SESSION_PREFIXES)
}

/// `metadata.user_id` 的文本。`session` 是发请求的会话 ID（Node 的写法）；没有时 `session_id` 是空字符串。
pub(crate) fn user_id(device_id: &str, session: Option<&str>) -> String {
    let mut out = String::from("{\"device_id\":");
    rust_cli_base::js::json::write_str(&mut out, device_id);
    out.push_str(",\"account_uuid\":\"\",\"session_id\":");
    rust_cli_base::js::json::write_str(&mut out, session.map_or("", external_session));
    out.push('}');
    out
}

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
