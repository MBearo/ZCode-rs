//! Workspace hook trust records and the store file format (Node
//! `workspace-hook-trust-store-file.ts`): strict parsing, trimmed strings,
//! `JSON.stringify(store, null, 2)` output.
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// One persistent grant, in the store file's key order.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub workspace_identity: String,
    pub hook_declaration_digest: String,
    pub digest_algorithm: String,
    pub decision: String,
    pub granted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_digest_at_grant: Option<String>,
    pub event_at_grant: String,
    pub display_command_at_grant: String,
    pub source_path_at_grant: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_discovery_order_at_grant: Option<u64>,
    /// `Some(None)` is a stored `null` (no matcher).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher_at_grant: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher_index_at_grant: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_index_at_grant: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version_at_grant: Option<String>,
}

impl Record {
    pub fn key(&self) -> (String, String) {
        (
            self.workspace_identity.clone(),
            self.hook_declaration_digest.clone(),
        )
    }
}

fn digest(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    (text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .then(|| text.to_owned())
}

/// zod `z.string().trim().min(1)`.
fn text(value: &Value) -> Option<String> {
    let text = value.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// zod `z.number().int().nonnegative()`.
fn index(value: &Value) -> Option<u64> {
    let n = value.as_f64()?;
    (n >= 0.0 && n.fract() == 0.0 && n <= 9_007_199_254_740_991.0).then_some(n as u64)
}

fn leap(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

/// zod `z.string().datetime()`: UTC `YYYY-MM-DDTHH:MM:SS[.fraction]Z`.
fn datetime(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = text.get(range)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let fixed = bytes.len() >= 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes.last() == Some(&b'Z');
    if !fixed {
        return None;
    }
    let fraction = &text[19..text.len() - 1];
    if !(fraction.is_empty()
        || fraction.len() > 1
            && fraction.starts_with('.')
            && fraction[1..].bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let (year, month, day) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap(year) => 29,
        2 => 28,
        _ => return None,
    };
    let valid = (1..=days).contains(&day)
        && digits(11..13)? < 24
        && digits(14..16)? < 60
        && digits(17..19)? < 60;
    valid.then(|| text.to_owned())
}

const EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
];
const RECORD_KEYS: [&str; 15] = [
    "workspaceIdentity",
    "hookDeclarationDigest",
    "digestAlgorithm",
    "decision",
    "grantedAt",
    "lastUsedAt",
    "bundleDigestAtGrant",
    "eventAtGrant",
    "displayCommandAtGrant",
    "sourcePathAtGrant",
    "sourceDiscoveryOrderAtGrant",
    "matcherAtGrant",
    "matcherIndexAtGrant",
    "hookIndexAtGrant",
    "appVersionAtGrant",
];

/// `Ok(None)` for an absent key, `Err` for a present but invalid one.
fn optional<T>(
    o: &Map<String, Value>,
    key: &str,
    check: impl Fn(&Value) -> Option<T>,
) -> Result<Option<T>, ()> {
    match o.get(key) {
        None => Ok(None),
        Some(value) => check(value).map(Some).ok_or(()),
    }
}

/// Node `workspaceHookTrustRecordSchema` (strict, strings trimmed).
pub fn parse_record(value: &Value) -> Option<Record> {
    let o = value.as_object()?;
    if o.keys().any(|k| !RECORD_KEYS.contains(&k.as_str())) {
        return None;
    }
    let event = text(o.get("eventAtGrant")?).filter(|e| EVENTS.contains(&e.as_str()));
    let matcher = optional(o, "matcherAtGrant", |v| match v {
        Value::Null => Some(None),
        Value::String(s) => Some(Some(s.clone())),
        _ => None,
    });
    Some(Record {
        workspace_identity: text(o.get("workspaceIdentity")?)?,
        hook_declaration_digest: digest(o.get("hookDeclarationDigest")?)?,
        digest_algorithm: (o.get("digestAlgorithm")? == "sha256").then(|| "sha256".to_owned())?,
        decision: (o.get("decision")? == "trusted").then(|| "trusted".to_owned())?,
        granted_at: datetime(o.get("grantedAt")?)?,
        last_used_at: optional(o, "lastUsedAt", datetime).ok()?,
        bundle_digest_at_grant: optional(o, "bundleDigestAtGrant", digest).ok()?,
        event_at_grant: event?,
        display_command_at_grant: text(o.get("displayCommandAtGrant")?)?,
        source_path_at_grant: text(o.get("sourcePathAtGrant")?)?,
        source_discovery_order_at_grant: optional(o, "sourceDiscoveryOrderAtGrant", index).ok()?,
        matcher_at_grant: matcher.ok()?,
        matcher_index_at_grant: optional(o, "matcherIndexAtGrant", index).ok()?,
        hook_index_at_grant: optional(o, "hookIndexAtGrant", index).ok()?,
        app_version_at_grant: optional(o, "appVersionAtGrant", text).ok()?,
    })
}

/// Node `parseWorkspaceHookTrustStoreContent`: `None` means corrupt.
pub fn parse_store(content: &str) -> Option<Vec<Record>> {
    let value: Value = serde_json::from_str(content).ok()?;
    let o = value.as_object()?;
    if o.len() != 2 || o.get("schemaVersion")?.as_f64()? != 1.0 {
        return None;
    }
    let records = o
        .get("records")?
        .as_array()?
        .iter()
        .map(parse_record)
        .collect::<Option<Vec<_>>>()?;
    let keys: BTreeSet<_> = records.iter().map(Record::key).collect();
    (keys.len() == records.len()).then_some(records)
}

/// The store file as Node writes it (`JSON.stringify(store, null, 2)`).
pub fn store_content(records: &[Record]) -> String {
    // 直接序列化结构体以保留记录字段顺序；顶层两个键按 Node 的顺序字面拼接。
    let body = serde_json::to_string_pretty(records).unwrap_or_else(|_| "[]".into());
    let records = body.replace('\n', "\n  ");
    format!("{{\n  \"schemaVersion\": 1,\n  \"records\": {records}\n}}\n")
}
