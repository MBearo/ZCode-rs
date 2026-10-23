//! 项目 hooks 的信任记录 `workspace-hook-trust-v1.json`（docs/specs/rust-cli/config.md §4.2），与 Node
//! CLI、App 共用。
//!
//! 格式与校验同 Node `packages/shared/src/workspace-hook-trust-store-file.ts`（strict：多余的键、缺字段、
//! 格式不对或重复的键都让整份记录作废，fail closed）；写入遵守 Node 的单文件锁与原子写
//! （`adapters/src/storage/workspace-hook-trust-store.ts`）。读取不加锁、不改名损坏文件（D3）。

use std::time::Duration;

use rust_cli_kernel_api::hooks::NODE_EVENTS;
use rust_cli_paths::StoragePath;
use serde_json::{Map, Value, json};

/// `eventAtGrant` 只允许 Node 的 7 个事件：表只定义在 kernel-api，这里按名字比较（F58：以前另写了一份字符串表）。
fn node_event(name: &str) -> bool {
    NODE_EVENTS.iter().any(|event| event.as_str() == name)
}

/// Node `DEFAULT_RENAME_RETRY_DELAYS_MS`（定义在 fs，F55）。
const RENAME_RETRY: &[Duration] = rust_cli_fs::NODE_RENAME_RETRY;

/// 一条信任记录（Node `WorkspaceHookTrustRecord`，字段顺序即 schema 顺序）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustRecord {
    pub workspace_identity: String,
    pub hook_declaration_digest: String,
    pub granted_at: String,
    pub last_used_at: Option<String>,
    pub bundle_digest_at_grant: Option<String>,
    pub event_at_grant: String,
    pub display_command_at_grant: String,
    pub source_path_at_grant: String,
    pub source_discovery_order_at_grant: Option<u64>,
    /// 缺省、`null` 或字符串。
    pub matcher_at_grant: Option<Option<String>>,
    pub matcher_index_at_grant: Option<u64>,
    pub hook_index_at_grant: Option<u64>,
    pub app_version_at_grant: Option<String>,
}

/// 读取的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustLoad {
    Missing,
    Records(Vec<TrustRecord>),
    /// 读不出或不合法：全部视为未信任。
    Corrupt,
}

impl TrustLoad {
    /// 某个工作区已信任的 digest。
    pub fn trusted(&self, identity: &str) -> Vec<String> {
        match self {
            Self::Records(records) => records
                .iter()
                .filter(|record| record.workspace_identity == identity)
                .map(|record| record.hook_declaration_digest.clone())
                .collect(),
            Self::Missing | Self::Corrupt => Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// Node 的 reason code：grant / revoke 前记录已损坏，不覆盖。
    #[error("workspace_hooks_trust_store_corrupt")]
    Corrupt,
    #[error("invalid trust record: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn non_empty(text: &str) -> bool {
    !text.trim().is_empty()
}

fn sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// zod `z.string().datetime()`：`YYYY-MM-DDTHH:MM:SS[.小数]Z`。
fn datetime(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
    };
    let fixed = bytes.len() >= 20
        && digits(0..4)
        && bytes[4] == b'-'
        && digits(5..7)
        && bytes[7] == b'-'
        && digits(8..10)
        && bytes[10] == b'T'
        && digits(11..13)
        && bytes[13] == b':'
        && digits(14..16)
        && bytes[16] == b':'
        && digits(17..19);
    let rest = &text.get(19..).unwrap_or("");
    fixed
        && match rest.strip_suffix('Z') {
            Some("") => true,
            Some(fraction) => fraction
                .strip_prefix('.')
                .is_some_and(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit())),
            None => false,
        }
}

const KEYS: [&str; 15] = [
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

impl TrustRecord {
    /// Node `workspaceHookTrustRecordSchema`（strict）。
    fn parse(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.keys().any(|key| !KEYS.contains(&key.as_str())) {
            return None;
        }
        let text = |key: &str| object.get(key).and_then(Value::as_str).map(str::to_owned);
        let optional_text = |key: &str, valid: &dyn Fn(&str) -> bool| match object.get(key) {
            None => Some(None),
            Some(Value::String(text)) if valid(text) => Some(Some(text.clone())),
            Some(_) => None,
        };
        let optional_index = |key: &str| match object.get(key) {
            None => Some(None),
            Some(value) => value.as_u64().map(Some),
        };
        let record = Self {
            workspace_identity: text("workspaceIdentity").filter(|t| non_empty(t))?,
            hook_declaration_digest: text("hookDeclarationDigest").filter(|t| sha256_hex(t))?,
            granted_at: text("grantedAt").filter(|t| datetime(t))?,
            last_used_at: optional_text("lastUsedAt", &datetime)?,
            bundle_digest_at_grant: optional_text("bundleDigestAtGrant", &sha256_hex)?,
            event_at_grant: text("eventAtGrant").filter(|t| node_event(t))?,
            display_command_at_grant: text("displayCommandAtGrant").filter(|t| non_empty(t))?,
            source_path_at_grant: text("sourcePathAtGrant").filter(|t| non_empty(t))?,
            source_discovery_order_at_grant: optional_index("sourceDiscoveryOrderAtGrant")?,
            matcher_at_grant: match object.get("matcherAtGrant") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(Value::String(text)) => Some(Some(text.clone())),
                Some(_) => return None,
            },
            matcher_index_at_grant: optional_index("matcherIndexAtGrant")?,
            hook_index_at_grant: optional_index("hookIndexAtGrant")?,
            app_version_at_grant: optional_text("appVersionAtGrant", &non_empty)?,
        };
        let fixed = text("digestAlgorithm").as_deref() == Some("sha256")
            && text("decision").as_deref() == Some("trusted");
        fixed.then_some(record)
    }

    /// 按 schema 顺序写出；缺省的可选字段不出现（与 Node 的 zod 解析结果相同）。
    fn to_value(&self) -> Value {
        let mut object = Map::new();
        let mut put = |key: &str, value: Value| {
            object.insert(key.to_owned(), value);
        };
        put("workspaceIdentity", json!(self.workspace_identity));
        put("hookDeclarationDigest", json!(self.hook_declaration_digest));
        put("digestAlgorithm", json!("sha256"));
        put("decision", json!("trusted"));
        put("grantedAt", json!(self.granted_at));
        let mut maybe = |key: &str, value: Option<Value>| {
            if let Some(value) = value {
                object.insert(key.to_owned(), value);
            }
        };
        maybe("lastUsedAt", self.last_used_at.as_ref().map(|v| json!(v)));
        maybe(
            "bundleDigestAtGrant",
            self.bundle_digest_at_grant.as_ref().map(|v| json!(v)),
        );
        maybe("eventAtGrant", Some(json!(self.event_at_grant)));
        maybe(
            "displayCommandAtGrant",
            Some(json!(self.display_command_at_grant)),
        );
        maybe("sourcePathAtGrant", Some(json!(self.source_path_at_grant)));
        maybe(
            "sourceDiscoveryOrderAtGrant",
            self.source_discovery_order_at_grant.map(|v| json!(v)),
        );
        maybe(
            "matcherAtGrant",
            self.matcher_at_grant.as_ref().map(|v| json!(v)),
        );
        maybe(
            "matcherIndexAtGrant",
            self.matcher_index_at_grant.map(|v| json!(v)),
        );
        maybe(
            "hookIndexAtGrant",
            self.hook_index_at_grant.map(|v| json!(v)),
        );
        maybe(
            "appVersionAtGrant",
            self.app_version_at_grant.as_ref().map(|v| json!(v)),
        );
        Value::Object(object)
    }

    fn key(&self) -> (&str, &str) {
        (&self.workspace_identity, &self.hook_declaration_digest)
    }
}

/// Node `workspaceHookTrustStoreFileSchema`：解析整份文件；任何不合法都返回 None。
fn parse_store(bytes: &[u8]) -> Option<Vec<TrustRecord>> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    if object.len() != 2 || object.get("schemaVersion") != Some(&json!(1)) {
        return None;
    }
    let records: Vec<TrustRecord> = object
        .get("records")?
        .as_array()?
        .iter()
        .map(TrustRecord::parse)
        .collect::<Option<_>>()?;
    let mut keys: Vec<_> = records.iter().map(TrustRecord::key).collect();
    keys.sort_unstable();
    let unique = keys.windows(2).all(|pair| pair[0] != pair[1]);
    unique.then_some(records)
}

/// 信任记录文件。
#[derive(Debug, Clone)]
pub struct HookTrustStore {
    file: StoragePath,
}

impl HookTrustStore {
    /// `file` 来自 `StoragePaths::hook_trust_store`。
    pub fn new(file: StoragePath) -> Self {
        Self { file }
    }

    pub fn path(&self) -> &StoragePath {
        &self.file
    }

    /// 读当前记录（不加锁：写入是原子 rename，读到的总是完整的一版）。
    pub async fn load(&self) -> TrustLoad {
        match rust_cli_fs::read_optional(self.file.as_path()).await {
            Ok(None) => TrustLoad::Missing,
            Ok(Some(bytes)) => parse_store(&bytes).map_or(TrustLoad::Corrupt, TrustLoad::Records),
            Err(_) => TrustLoad::Corrupt,
        }
    }

    /// 加入记录：同一 `(workspaceIdentity, digest)` 原位替换，新的追加在后（Node `grant`）。
    pub async fn grant(&self, records: Vec<TrustRecord>) -> Result<(), TrustError> {
        for record in &records {
            if TrustRecord::parse(&record.to_value()).is_none() {
                return Err(TrustError::Invalid(record.hook_declaration_digest.clone()));
            }
        }
        self.mutate(move |mut current| {
            for record in records {
                match current.iter_mut().find(|known| known.key() == record.key()) {
                    Some(known) => *known = record,
                    None => current.push(record),
                }
            }
            current
        })
        .await
    }

    /// 撤销某个工作区的记录：`digests` 为 None 时撤销全部（Node `revoke`）。
    pub async fn revoke(
        &self,
        identity: &str,
        digests: Option<&[String]>,
    ) -> Result<(), TrustError> {
        self.mutate(|current| {
            current
                .into_iter()
                .filter(|record| {
                    record.workspace_identity != identity
                        || digests.is_some_and(|digests| {
                            !digests.contains(&record.hook_declaration_digest)
                        })
                })
                .collect()
        })
        .await
    }

    /// 建 0700 目录 → 持锁 → 重读（损坏时拒绝）→ 修改 → 原子写（0600，`JSON.stringify(store, null, 2)`）。
    async fn mutate(
        &self,
        update: impl FnOnce(Vec<TrustRecord>) -> Vec<TrustRecord>,
    ) -> Result<(), TrustError> {
        let file = self.file.as_path();
        if let Some(parent) = file.parent() {
            rust_cli_fs::create_private_dir_all(parent).await?;
        }
        let options = rust_cli_fs::SingleLockOptions::default();
        let mut outcome = Ok(());
        rust_cli_fs::with_single_file_lock(file, &options, async {
            let current = match rust_cli_fs::read_optional(file).await? {
                None => Vec::new(),
                Some(bytes) => match parse_store(&bytes) {
                    Some(records) => records,
                    None => {
                        outcome = Err(TrustError::Corrupt);
                        return Ok(());
                    }
                },
            };
            let records: Vec<Value> = update(current).iter().map(TrustRecord::to_value).collect();
            let store = json!({"schemaVersion": 1, "records": records});
            let text = format!(
                "{}\n",
                serde_json::to_string_pretty(&store).map_err(std::io::Error::other)?
            );
            let write = rust_cli_fs::WriteOptions {
                mode: rust_cli_fs::FileMode::Private,
                temp_name: rust_cli_fs::TempName::Hidden,
                parent: rust_cli_fs::ParentDir::Existing,
                rename_retry: RENAME_RETRY,
            };
            rust_cli_fs::atomic_write(file, text.as_bytes(), write).await
        })
        .await?;
        outcome
    }
}

#[cfg(test)]
#[path = "hook_trust_tests.rs"]
mod tests;
