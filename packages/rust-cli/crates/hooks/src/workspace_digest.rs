//! 项目级 hooks 的信任 digest（docs/specs/rust-cli/config.md §4.1），与 Node 逐字节一致。
//!
//! 算法照搬 `packages/shared/src/workspace-hook-digest.ts`（`resolveWorkspaceHookEntries`、
//! `buildWorkspaceHookBundleSnapshot`、`createWorkspaceHookDeclarationDigest`）与
//! `workspace-hook-config.ts`（`resolveWorkspaceHookRuntimeRoot`、门的计算）。digest 是
//! `sha256(JSON.stringify(数组))`，JSON 文本按 JS 的规则生成。只遍历 Node 的 7 个事件。

use rust_cli_base::js::json::stringify;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::node_config::{
    DEFAULT_MAX_OUTPUT_BYTES, DEFAULT_TIMEOUT_MS, NODE_EVENTS, NodeHook, NodeHookKind, NodeHooks,
    NodeShell,
};
use rust_cli_kernel_api::hooks::HookEventName;

/// Node `WORKSPACE_HOOK_DIGEST_SCHEMA_VERSION`。
const SCHEMA_VERSION: u64 = 1;

/// `sha256(JSON.stringify(value))`，JSON 文本按 JS 的规则生成（`rust_cli_base::js::json`：数字是 JS 的
/// 最短形式，整数不带小数点）。
fn sha256(value: &Value) -> String {
    let digest = Sha256::digest(stringify(value).as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 运行根（Node `WorkspaceHookRuntimeRoot`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuntimeRoot {
    pub enabled: bool,
    pub timeout_ms: f64,
    pub max_output_bytes: f64,
}

/// Node `resolveWorkspaceHookRuntimeRoot`：按给出的顺序（用户级、各项目来源），任一 `enabled === true`
/// 则启用；`timeoutMs`、`maxOutputBytes` 取最后写的值；四舍五入，至少 1。
pub fn runtime_root<'a>(roots: impl IntoIterator<Item = &'a NodeHooks>) -> RuntimeRoot {
    let mut root = RuntimeRoot {
        enabled: false,
        timeout_ms: DEFAULT_TIMEOUT_MS,
        max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
    };
    for hooks in roots {
        root.enabled |= hooks.enabled == Some(true);
        if let Some(timeout) = hooks.timeout_ms {
            root.timeout_ms = timeout;
        }
        if let Some(bytes) = hooks.max_output_bytes {
            root.max_output_bytes = bytes;
        }
    }
    root.timeout_ms = root.timeout_ms.round().max(1.0);
    root.max_output_bytes = root.max_output_bytes.round().max(1.0);
    root
}

/// 一个含 `hooks` 的项目文件（Node `WorkspaceHookSourceInput`）。
#[derive(Debug, Clone)]
pub struct DigestSource<'a> {
    /// 相对工作区根的路径（`/` 分隔，为空时取文件名）。
    pub relative_path: String,
    pub discovery_order: usize,
    /// `zcode.json`、`.zcode/config.json` 或 `explicit`。
    pub kind: &'static str,
    pub explicit: bool,
    pub editable: bool,
    pub hooks: &'a NodeHooks,
}

/// 一个声明（Node `CanonicalWorkspaceHookEntryData` 里 digest 与准入要用的部分）。
#[derive(Debug, Clone, PartialEq)]
pub struct DigestEntry {
    pub review_item_id: String,
    pub event: HookEventName,
    pub matcher_index: usize,
    pub hook_index: usize,
    pub source_index: usize,
    pub source_relative_path: String,
    pub source_discovery_order: usize,
    pub matcher: Option<String>,
    pub hook: NodeHook,
    pub resolved_timeout_ms: f64,
    pub source_root_enabled: bool,
    pub declaration_enabled: bool,
    pub runtime_hooks_enabled: bool,
    pub configured_enabled: bool,
    pub editable: bool,
    pub digest: String,
}

/// 一组项目来源的 digest（Node `WorkspaceHookBundleSnapshotData` 的 `hooks` 与 `bundleDigest`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Bundle {
    pub entries: Vec<DigestEntry>,
    pub bundle_digest: String,
}

fn optional(value: Option<Value>) -> Value {
    match value {
        None => json!(["unset"]),
        Some(value) => json!(["set", value]),
    }
}

fn execution(hook: &NodeHook) -> Value {
    match &hook.kind {
        NodeHookKind::Process { args } => json!(["process", hook.command, args]),
        NodeHookKind::Command {
            shell, background, ..
        } => {
            let shell = match shell {
                NodeShell::Unset => json!(["unset"]),
                NodeShell::True => json!(["true"]),
                NodeShell::Named(name) => json!(["string", name]),
            };
            json!(["command", hook.command, background, shell])
        }
    }
}

/// Node `canonicalDeclarationPayload` 的 sha256。
fn declaration_digest(
    source: &DigestSource<'_>,
    event: HookEventName,
    (matcher, matcher_index, hook_index): (Option<&str>, usize, usize),
    hook: &NodeHook,
    (timeout_ms, max_output_bytes): (f64, f64),
) -> String {
    let mut payload = vec![
        json!("workspace-hook-declaration"),
        json!(SCHEMA_VERSION),
        json!(source.relative_path),
        json!(source.discovery_order),
        json!(event.as_str()),
        json!(matcher),
        json!(matcher_index),
        json!(hook_index),
        execution(hook),
        json!(timeout_ms),
        json!(max_output_bytes),
    ];
    // `onError: "deny"` 只在写成 deny 时追加：没写或写 allow 时 digest 与以前逐字节相同，已有信任不失效
    // （Node `canonicalDeclarationPayload`，修复线 F68）。
    if hook.deny_on_error {
        payload.push(json!(["onError", "deny"]));
    }
    sha256(&Value::Array(payload))
}

/// Node `resolveWorkspaceHookEntries`：来源 → 7 个事件的固定顺序 → matcher → hook。
pub fn entries(sources: &[DigestSource<'_>], root: &RuntimeRoot) -> Vec<DigestEntry> {
    let mut entries = Vec::new();
    for (source_index, source) in sources.iter().enumerate() {
        for event in NODE_EVENTS {
            for (matcher_index, matcher) in source.hooks.matchers(event).iter().enumerate() {
                for (hook_index, hook) in matcher.hooks.iter().enumerate() {
                    let source_root_enabled = source.hooks.enabled != Some(false);
                    let declaration_enabled = hook.enabled != Some(false);
                    let timeout = hook.resolved_timeout_ms(root.timeout_ms);
                    let digest = declaration_digest(
                        source,
                        event,
                        (matcher.matcher.as_deref(), matcher_index, hook_index),
                        hook,
                        (timeout, root.max_output_bytes),
                    );
                    entries.push(DigestEntry {
                        review_item_id: format!(
                            "workspace-hook-{source_index}-{}-{matcher_index}-{hook_index}",
                            event.as_str()
                        ),
                        event,
                        matcher_index,
                        hook_index,
                        source_index,
                        source_relative_path: source.relative_path.clone(),
                        source_discovery_order: source.discovery_order,
                        matcher: matcher.matcher.clone(),
                        hook: hook.clone(),
                        resolved_timeout_ms: timeout,
                        source_root_enabled,
                        declaration_enabled,
                        runtime_hooks_enabled: root.enabled,
                        configured_enabled: source_root_enabled
                            && declaration_enabled
                            && root.enabled,
                        editable: source.editable,
                        digest,
                    });
                }
            }
        }
    }
    entries
}

/// Node `buildWorkspaceHookBundleSnapshot` 的 digest 部分；没有声明时为 None。
pub fn bundle(sources: &[DigestSource<'_>], root: &RuntimeRoot) -> Option<Bundle> {
    let entries = entries(sources, root);
    if entries.is_empty() {
        return None;
    }
    let sources: Vec<Value> = sources
        .iter()
        .map(|source| {
            json!([
                source.relative_path,
                source.discovery_order,
                source.kind,
                source.explicit,
                optional(source.hooks.enabled.map(Value::from)),
                optional(source.hooks.timeout_ms.map(Value::from)),
                optional(source.hooks.max_output_bytes.map(Value::from)),
            ])
        })
        .collect();
    let hooks: Vec<Value> = entries
        .iter()
        .map(|entry| {
            json!([
                entry.digest,
                entry.source_root_enabled,
                entry.declaration_enabled,
                entry.runtime_hooks_enabled,
                entry.configured_enabled,
            ])
        })
        .collect();
    let payload = json!(["workspace-hook-bundle", SCHEMA_VERSION, sources, hooks]);
    Some(Bundle {
        bundle_digest: sha256(&payload),
        entries,
    })
}

#[cfg(test)]
#[path = "workspace_digest_tests.rs"]
mod tests;
