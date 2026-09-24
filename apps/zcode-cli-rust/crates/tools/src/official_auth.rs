//! Official MCP identity headers (Node `official-mcp-auth-port.ts`,
//! `official-mcp-auth.ts` `isOfficialMcpOriginTrusted`): the Host is the
//! identity authority, asked through the engine for every request. Headers
//! are never stored or logged. Spec rust-m10-plugins §3.11.
use crate::contract::{Event, RunEvent};
use serde_json::{Map, Value, json};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot};

pub(super) const META_KEY: &str = "com.zcode/official-mcp-auth";
pub(super) const UNAVAILABLE: &str = "official_auth_unavailable";
pub(super) const UNTRUSTED: &str = "official_mcp_origin_untrusted";
const REASONS: [&str; 3] = [UNAVAILABLE, "official_auth_plan_required", UNTRUSTED];
const DEV_ORIGINS_ENV: &str = "ZCODE_OFFICIAL_MCP_DEV_TRUSTED_ORIGINS";
const IDENTITY_ENV: &str = "ZCODE_WORKSPACE_IDENTITY";
/// Owner of the Host waits; never released before shutdown.
const OWNER: &str = "official-mcp-auth";

/// The plugin identity of an official server (host-generated provenance).
#[derive(Clone, Debug)]
pub(super) struct Official {
    pub plugin_id: String,
    pub mcp_key: String,
}

impl Official {
    pub fn from_config(raw: &Value) -> Option<Self> {
        (raw["auth"]["type"] == "zcode_official").then_some(())?;
        Some(Self {
            plugin_id: raw["official"]["pluginId"].as_str()?.to_owned(),
            mcp_key: raw["official"]["mcpKey"].as_str()?.to_owned(),
        })
    }
}

pub(super) struct OfficialAuth {
    events: OnceLock<mpsc::Sender<RunEvent>>,
    sequence: AtomicU64,
    env: zcode_cli_net::RuntimeEnv,
    /// The Host's workspace path (the engine's), else the tools working directory.
    workspace_path: OnceLock<String>,
    cwd: String,
}

/// `ok` headers or the failure reason (Node `OfficialMcpAuthHeadersResult`).
pub(super) type Headers = Result<Map<String, Value>, &'static str>;

fn loopback_origin(candidate: &str) -> Option<String> {
    let url = url::Url::parse(candidate).ok()?;
    let host = url.host_str()?;
    let loopback = matches!(host, "127.0.0.1" | "localhost" | "[::1]");
    (url.scheme() == "http" && loopback && url.username().is_empty() && url.password().is_none())
        .then(|| url.origin().ascii_serialization())
}

fn https_origin(candidate: &str) -> Option<String> {
    let url = url::Url::parse(candidate).ok()?;
    (url.scheme() == "https" && url.username().is_empty() && url.password().is_none())
        .then(|| url.origin().ascii_serialization())
}

/// Node `isOfficialMcpOriginTrusted`: `(trusted, detail)`.
pub(super) fn trusted(
    origin: &str,
    dev: Option<&str>,
    zcode: Option<&str>,
) -> (bool, &'static str) {
    let origin = origin.trim();
    if origin.is_empty() {
        return (false, "invalid_input");
    }
    // 本地自测开关只放开 http 回环；目标本身必须先是回环，否则两侧都为空会误放行。
    if let Some(loopback) = loopback_origin(origin) {
        let listed = dev
            .into_iter()
            .flat_map(|raw| raw.split(','))
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .any(|candidate| loopback_origin(candidate).as_ref() == Some(&loopback));
        if listed {
            return (true, "ok");
        }
    }
    let Some(expected) = zcode.and_then(https_origin) else {
        return (false, "zcode_origin_unresolved");
    };
    if https_origin(origin).as_ref() != Some(&expected) {
        return (false, "origin_mismatch");
    }
    (true, "ok")
}

impl OfficialAuth {
    pub fn new(env: zcode_cli_net::RuntimeEnv, workspace_path: String) -> Self {
        Self {
            events: OnceLock::new(),
            sequence: AtomicU64::new(0),
            env,
            workspace_path: OnceLock::new(),
            cwd: workspace_path,
        }
    }

    pub fn for_workspace(
        egress: &zcode_cli_net::Egress,
        cwd: &std::path::Path,
    ) -> std::sync::Arc<Self> {
        let path = cwd.to_string_lossy().into_owned();
        std::sync::Arc::new(Self::new(egress.runtime_env().clone(), path))
    }

    pub fn attach(&self, events: mpsc::Sender<RunEvent>, workspace_path: &str) {
        let _ = self.events.set(events);
        let _ = self.workspace_path.set(workspace_path.to_owned());
    }

    /// Whether a Host channel exists (standalone runs have none).
    pub fn attached(&self) -> bool {
        self.events.get().is_some()
    }

    fn env(&self, key: &str) -> Option<&str> {
        self.env.get(key)
    }

    /// The current ZCode API origin (Node `resolveRuntimeZCodeEndpointOrigin`).
    pub fn zcode_origin(&self) -> Option<String> {
        zcode_cli_net::headers::endpoint_origin(|key| self.env(key)).ok()
    }

    /// Node `createOfficialMcpTrustedOriginRegistry(...).isTrusted`.
    pub fn is_trusted(&self, origin: &str) -> (bool, &'static str) {
        trusted(
            origin,
            self.env(DEV_ORIGINS_ENV),
            self.zcode_origin().as_deref(),
        )
    }

    fn workspace(&self) -> Value {
        let identity = self
            .env(IDENTITY_ENV)
            .map(str::trim)
            .filter(|i| !i.is_empty());
        let path = self.workspace_path.get().unwrap_or(&self.cwd);
        let mut workspace = json!({"workspaceKey":identity.unwrap_or(path),"workspacePath":path});
        if let Some(identity) = identity {
            workspace["workspaceIdentity"] = identity.into();
        }
        workspace
    }

    /// Node `resolveHeaders`: any transport failure or malformed reply is
    /// `official_auth_unavailable`; the caller dropping the future abandons the wait.
    pub async fn headers(&self, official: &Official, target_origin: &str) -> Headers {
        let Some(events) = self.events.get() else {
            return Err(UNAVAILABLE);
        };
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let params = json!({"mcpKey":official.mcp_key,"pluginId":official.plugin_id,
            "requestId":format!("official-mcp-auth:{sequence}"),"targetOrigin":target_origin,
            "workspace":self.workspace()});
        let (reply, answer) = oneshot::channel();
        let event = RunEvent {
            session_id: OWNER.into(),
            run_id: OWNER.into(),
            event: Event::HostCall {
                method: "interaction/requestOfficialMcpAuthHeaders",
                params,
                reply: Some(reply),
            },
        };
        if events.send(event).await.is_err() {
            return Err(UNAVAILABLE);
        }
        let Ok(result) = answer.await else {
            return Err(UNAVAILABLE);
        };
        parse_reply(&result)
    }
}

/// `zcodeOfficialMcpAuthHeadersResponseSchema` (strict).
fn parse_reply(result: &Value) -> Headers {
    let Some(reply) = result.as_object() else {
        return Err(UNAVAILABLE);
    };
    match reply.get("ok") {
        Some(Value::Bool(true)) if reply.len() == 2 => match reply.get("headers") {
            Some(Value::Object(headers)) if headers.values().all(Value::is_string) => {
                Ok(headers.clone())
            }
            _ => Err(UNAVAILABLE),
        },
        Some(Value::Bool(false)) if reply.len() == 2 => {
            let reason = reply.get("reason").and_then(Value::as_str);
            REASONS
                .iter()
                .find(|r| Some(**r) == reason)
                .map_or(Err(UNAVAILABLE), |r| Err(*r))
        }
        _ => Err(UNAVAILABLE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_and_replies_follow_node() {
        let zcode = Some("https://zcode.example");
        assert_eq!(trusted("https://zcode.example", None, zcode), (true, "ok"));
        assert_eq!(
            trusted("https://other.example", None, zcode),
            (false, "origin_mismatch")
        );
        assert_eq!(
            trusted("https://u:p@zcode.example", None, zcode),
            (false, "origin_mismatch")
        );
        assert_eq!(
            trusted("https://zcode.example", None, None),
            (false, "zcode_origin_unresolved")
        );
        assert_eq!(trusted(" ", None, zcode), (false, "invalid_input"));
        let dev = Some("http://127.0.0.1:3999, http://localhost:1");
        assert_eq!(trusted("http://127.0.0.1:3999", dev, zcode), (true, "ok"));
        assert_eq!(
            trusted("http://127.0.0.1:4000", dev, zcode),
            (false, "origin_mismatch")
        );
        assert!(!trusted("https://evil.example", Some("https://evil.example"), zcode).0);
        assert_eq!(
            parse_reply(&json!({"ok":true,"headers":{"Authorization":"Bearer t"}})).unwrap()["Authorization"],
            "Bearer t"
        );
        assert_eq!(
            parse_reply(&json!({"ok":false,"reason":"official_auth_plan_required"})),
            Err("official_auth_plan_required")
        );
        assert_eq!(
            parse_reply(&json!({"ok":false,"reason":"other"})),
            Err(UNAVAILABLE)
        );
        assert_eq!(
            parse_reply(&json!({"ok":true,"headers":{"a":1}})),
            Err(UNAVAILABLE)
        );
        assert_eq!(
            parse_reply(&json!({"ok":true,"headers":{},"x":1})),
            Err(UNAVAILABLE)
        );
    }
}
