//! Switching one reviewed project hook in `<cwd>/.zcode/config.json` (Node
//! `shared/workspace-hook-mutation.ts`): the declaration must still have the
//! reviewed digest; the file is rewritten atomically in its own key order.
use crate::contract::HookToggle;
use crate::domain::{
    hooks::digest::{Slot, declaration_digest},
    json_order::Ordered,
};
use std::path::Path;
use tokio::io::AsyncWriteExt;

const UNREADABLE: &str = "workspace_hooks_config_unreadable";
const MISMATCH: &str = "workspace_hooks_snapshot_mismatch";
const WRITE_FAILED: &str = "workspace_hooks_config_write_failed";

pub async fn set_enabled(toggle: &HookToggle) -> Result<(), &'static str> {
    let path = Path::new(&toggle.path);
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|_| UNREADABLE)?;
    let mut tree: Ordered = serde_json::from_str(&text).map_err(|_| UNREADABLE)?;
    // 与 Node 一致：配置已不符合 schema 时视为审查快照失效，不写入。
    let checked = crate::domain::config::parse_file(&toggle.path, Ok(Some(&text)));
    if !checked.loaded {
        return Err(MISMATCH);
    }
    let group = tree
        .get_mut("hooks")
        .and_then(|h| h.get_mut("events"))
        .and_then(|e| e.get_mut(toggle.event.as_str()))
        .and_then(|m| m.at_mut(toggle.matcher_index))
        .ok_or(MISMATCH)?;
    let matcher = group.to_value()["matcher"].clone();
    let declaration = group
        .get_mut("hooks")
        .and_then(|h| h.at_mut(toggle.hook_index))
        .ok_or(MISMATCH)?;
    let raw = declaration.to_value();
    if !raw.is_object() {
        return Err(MISMATCH);
    }
    let slot = Slot {
        relative_path: &toggle.relative_path,
        discovery_order: toggle.discovery_order,
        event: toggle.event,
        matcher: &matcher,
        matcher_index: toggle.matcher_index,
        hook_index: toggle.hook_index,
        default_timeout_ms: toggle.resolved_timeout_ms,
        max_output_bytes: toggle.resolved_max_output_bytes,
    };
    if declaration_digest(&raw, &slot) != toggle.digest {
        return Err(MISMATCH);
    }
    declaration.set("enabled", Ordered::Bool(toggle.enabled));
    write(path, &format!("{}\n", tree.pretty()))
        .await
        .map_err(|_| WRITE_FAILED)
}

/// Node `atomicWriteWorkspaceHookConfig`: same-directory temp file (0600),
/// fsync, rename; the temp file is removed on failure.
async fn write(path: &Path, content: &str) -> std::io::Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    tokio::fs::create_dir_all(directory).await?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = directory.join(format!(
        ".{name}.{}.{}.{:x}.tmp",
        std::process::id(),
        crate::now(),
        uuid::Uuid::new_v4().as_u128() as u64
    ));
    let result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut handle = options.open(&temp).await?;
        handle.write_all(content.as_bytes()).await?;
        handle.sync_all().await?;
        drop(handle);
        tokio::fs::rename(&temp, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}
