//! 每请求头（工作线 F64，docs/specs/rust-cli/model.md §2.4）：照 Node `createModelRequestAttributionHeaders`
//! （`apps/zcode-cli/packages/adapters/src/model/runner-attribution.ts`）。
//!
//! 请求体只编码一次，发请求的一方（会话、回合、子代理、来源）在编码前取出成 [`Origin`]；每次发送（含重试）再配上新的
//! `x-request-id` 算出这一组头。

use rust_cli_base::rule_url::is_opencode_go_endpoint;
use rust_cli_kernel_api::model::ModelRequest;

use crate::metadata::{external_session, strip_prefixes};

pub(crate) const REQUEST_ID: &str = "x-request-id";
pub(crate) const SESSION_TYPE: &str = "x-zcode-session-type";
pub(crate) const TRACE_ID: &str = "x-zcode-trace-id";
pub(crate) const QUERY_ID: &str = "x-query-id";
/// 请求用途：Provider 配置里的同名头总是去掉（Node `withoutModelQuerySourceHeader`）。
pub(crate) const QUERY_SOURCE: &str = "x-zcode-query-source";
pub(crate) const SESSION_ID: &str = "x-session-id";
pub(crate) const OPENCODE_SESSION: &str = "x-opencode-session";

/// 回合 ID 的内部前缀（Node 去掉 query id 的 `query_`）。
const RUN_PREFIX: &str = "run_";

/// 发这次请求的一方。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Origin {
    /// `x-session-id` 的值（已去掉内部前缀）。
    session: Option<String>,
    /// `x-query-id` 的值（已去掉内部前缀）。
    query: Option<String>,
    /// 子代理发的请求。
    subagent: bool,
    /// `x-zcode-query-source` 的值；不合格时为 None（Node 省略）。
    source: Option<String>,
}

impl Origin {
    pub(crate) fn of(request: &ModelRequest) -> Self {
        let options = &request.options;
        // 请求来源的名字（model.md §2.4 的表）：Node 有对应用途的用 Node 的名字，其余用本工程的名字；与用量表的
        // `query_source` 同一个映射（工作线 F71）。
        let source = request.source.node_name();
        Self {
            session: options
                .session_id
                .as_ref()
                .map(|session| external_session(session.as_str()).to_owned()),
            query: options
                .run
                .as_ref()
                .map(|run| strip_prefixes(run.as_str(), &[RUN_PREFIX]).to_owned()),
            subagent: options.agent.is_some(),
            source: valid_source(&source).then_some(source),
        }
    }

    /// 没有会话、回合与来源的请求（`/context` 的计数）。
    pub(crate) fn none() -> Self {
        Self::default()
    }

    /// `x-zcode-session-type`：Node 按发请求的 runtime 分（`resolveModelRequestSessionTypeFromTaskType`）。
    fn session_type(&self) -> &'static str {
        if self.subagent {
            "subagent"
        } else if self.session.is_some() {
            "main"
        } else {
            "other"
        }
    }
}

/// Node `QUERY_SOURCE_HEADER_VALUE_PATTERN`：`^[A-Za-z0-9_.:-]{1,128}$`（值是观测标识，不合格时省略）。
fn valid_source(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
}

/// 一次发送的每请求头，顺序同 Node 的对象。`base_url` 是端点的地址（OpenCode Go 另带会话头）。
pub(crate) fn headers(
    origin: &Origin,
    base_url: &str,
    trace: &str,
    request_id: String,
) -> Vec<(&'static str, String)> {
    let mut out = vec![
        (REQUEST_ID, request_id),
        (SESSION_TYPE, origin.session_type().to_owned()),
        (TRACE_ID, trace.to_owned()),
    ];
    if let Some(query) = &origin.query {
        out.push((QUERY_ID, query.clone()));
    }
    if let Some(source) = &origin.source {
        out.push((QUERY_SOURCE, source.clone()));
    }
    if let Some(session) = &origin.session {
        out.push((SESSION_ID, session.clone()));
        if is_opencode_go_endpoint(base_url) {
            out.push((OPENCODE_SESSION, session.clone()));
        }
    }
    out
}

#[cfg(test)]
#[path = "attribution_tests.rs"]
mod tests;
