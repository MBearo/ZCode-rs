//! `auth: {type: "zcode_official", provider: "jwt_token"}` of plugin MCP
//! servers (Node `mcp-official-auth.ts`, `official-mcp-auth.ts`
//! `findOfficialMcpReservedHeaders`). Spec rust-m10-plugins §3.11.
use serde_json::{Value, json};

pub const AUTH_TYPE: &str = "zcode_official";
pub const PROVIDER: &str = "jwt_token";

/// Headers a plugin may never set statically on an official server (lower case).
pub const RESERVED_HEADERS: [&str; 8] = [
    "authorization",
    "x-bigmodel-authorization",
    "bigmodel-target-type",
    "bigmodel-organization",
    "bigmodel-project",
    "x-coding-plan-api-key",
    "mcp-session-id",
    "mcp-protocol-version",
];

pub fn reserved(name: &str) -> bool {
    RESERVED_HEADERS.contains(&crate::js::trim(name).to_lowercase().as_str())
}

/// JavaScript `String(value)` for the error messages.
fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(_)) => "[object Object]".into(),
        Some(other) => other.to_string(),
    }
}

/// Node `parseZCodeOfficialAuth`: `None` when undeclared; a malformed
/// declaration is an error, never a silent downgrade to anonymous requests.
pub fn parse(value: Option<&Value>, key: &str) -> Result<Option<Value>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Value::Object(auth) = value else {
        return Err(format!("MCP server {key}: auth must be an object"));
    };
    if auth.get("type").and_then(Value::as_str) != Some(AUTH_TYPE) {
        return Err(format!(
            "MCP server {key}: unsupported auth type: {}",
            js_string(auth.get("type"))
        ));
    }
    if auth.get("provider").and_then(Value::as_str) != Some(PROVIDER) {
        return Err(format!(
            "MCP server {key}: unsupported auth provider: {}",
            js_string(auth.get("provider"))
        ));
    }
    Ok(Some(json!({"type":AUTH_TYPE,"provider":PROVIDER})))
}

/// Node `findOfficialMcpReservedHeaders`: hits, lower-cased, deduplicated and sorted.
pub fn reserved_hits(headers: &Value) -> Vec<String> {
    let mut hits: Vec<String> = headers
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, _)| crate::js::trim(name).to_lowercase())
        .filter(|name| RESERVED_HEADERS.contains(&name.as_str()))
        .collect();
    hits.sort();
    hits.dedup();
    hits
}

/// Node `buildOfficialProvenance`: host-generated, never read from `.mcp.json`.
pub fn provenance(key: &str, plugin_id: &str) -> Value {
    json!({"mcpKey":key,"pluginId":plugin_id,"source":"plugin"})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_auth_declarations_follow_node() {
        assert_eq!(parse(None, "k"), Ok(None));
        let valid = json!({"type":"zcode_official","provider":"jwt_token","extra":1});
        assert_eq!(
            parse(Some(&valid), "k").unwrap(),
            Some(json!({"type":"zcode_official","provider":"jwt_token"}))
        );
        assert_eq!(
            parse(Some(&json!("x")), "k").unwrap_err(),
            "MCP server k: auth must be an object"
        );
        assert_eq!(
            parse(Some(&json!({"type":"zcode-official"})), "k").unwrap_err(),
            "MCP server k: unsupported auth type: zcode-official"
        );
        assert_eq!(
            parse(Some(&json!({"type":"zcode_official"})), "k").unwrap_err(),
            "MCP server k: unsupported auth provider: undefined"
        );
        let headers =
            json!({"X-Ok":"1","Authorization":"a","authorization ":"b","MCP-Session-Id":"s"});
        assert_eq!(
            reserved_hits(&headers),
            vec!["authorization", "mcp-session-id"]
        );
        assert!(reserved("Bigmodel-Project") && !reserved("x-other"));
    }
}
