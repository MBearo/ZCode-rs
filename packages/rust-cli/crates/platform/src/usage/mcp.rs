// 语义复制自 packages/services/src/usage-stats/providers/zcodeMcpQuotaProvider.ts 的 `fetchMcpQuotaSnapshot`、
// `buildMcpQuotaAggregateLimit`，与 packages/services/src/official-mcp/officialMcpCredentials.ts 的
// `buildOfficialMcpAuthHeaders`（staging 420d27c977）。
//! ZCode MCP 的额度（docs/specs/rust-cli/platform.md §11.4）：`GET {zcode 服务}/mcp/usage`。
//!
//! 范围取被问的计划（D-U-3）：App 取设置里选中的连接，再与面板的计划比对；Puffin 每次问一个计划，直接用它。

use rust_cli_base::secret::RedactedString;
use rust_cli_net::HeaderValue;
use serde_json::Value;

use super::quota::QuotaLimit;
use super::{Context, Login, PlanTarget, number};
use crate::token::http::Reply;
use rust_cli_base::account::Family;

/// App `MCP_USAGE_QUOTA_LIMIT_TYPE`。
pub(super) const MCP_LIMIT: &str = "MCP_USAGE";

pub(super) async fn fetch(
    cx: &Context<'_>,
    target: &PlanTarget,
    login: &Login,
) -> Option<QuotaLimit> {
    // 官方 MCP 的凭据只属于当前登录的 family（App `resolveOfficialMcpCredentials`）。
    if login.active.as_ref().map(RedactedString::expose) != Some(target.family.as_str()) {
        return None;
    }
    let (Some(jwt), Some(token)) = (&login.zcode_jwt, &login.token) else {
        return None;
    };
    let mut headers = vec![
        ("Authorization", HeaderValue::Secret(bearer(jwt.expose()))),
        (
            "X-Bigmodel-Authorization",
            HeaderValue::Secret(bearer(token.expose())),
        ),
    ];
    match (&target.team, target.family) {
        (None, _) => headers.push((
            "Bigmodel-Target-Type",
            HeaderValue::Plain("PERSONAL".into()),
        )),
        // zai 的团队没有 wire scope（App `toWirePlanScope`）：不发范围头。
        (Some(_), Family::Zai) => {}
        (Some(team), Family::Bigmodel) => {
            headers.push(("Bigmodel-Target-Type", HeaderValue::Plain("TEAM".into())));
            headers.push((
                "Bigmodel-Organization",
                HeaderValue::Plain(team.organization_id.clone()),
            ));
            headers.push((
                "Bigmodel-Project",
                HeaderValue::Plain(team.project_id.clone()),
            ));
        }
    }
    let url = format!("{}/mcp/usage", cx.origins.zcode_api);
    let payload = match cx.http.get_headers(url, headers).await {
        Ok(Reply::Json(payload)) => payload,
        Ok(Reply::NotFound) | Err(_) => {
            tracing::warn!("the MCP quota request failed");
            return None;
        }
    };
    let limit = aggregate(&payload);
    if limit.is_none() {
        tracing::warn!("the MCP quota response has no usable total_usage");
    }
    limit
}

fn bearer(token: &str) -> RedactedString {
    RedactedString::new(format!("Bearer {token}"))
}

/// 信封与总额度（App `mcpUsageEnvelopeSchema` + `buildMcpQuotaAggregateLimit`）。
pub(super) fn aggregate(payload: &Value) -> Option<QuotaLimit> {
    if number(&payload["code"]) != Some(0.0) {
        return None;
    }
    let data = &payload["data"];
    number(&data["server_time"])?;
    let total = &data["total_usage"];
    // `used` 只校验形状（zod 要求它是数字），额度条只看剩余。
    number(&total["used"])?;
    let (limit, remaining) = (number(&total["limit"])?, number(&total["remaining"])?);
    let limit = limit.max(0.0);
    if limit <= 0.0 {
        return None;
    }
    let remaining = remaining.max(0.0).min(limit);
    Some(QuotaLimit {
        kind: MCP_LIMIT.to_owned(),
        remaining: Some(remaining),
        percentage: Some((100.0 - remaining / limit * 100.0).clamp(0.0, 100.0)),
        next_reset_ms: number(&data["next_refresh_at"]).map(|seconds| seconds * 1000.0),
        ..QuotaLimit::default()
    })
}
