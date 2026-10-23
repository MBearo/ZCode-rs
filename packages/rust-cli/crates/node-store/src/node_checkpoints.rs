//! Node 写的 checkpoint 的产物（修复线 F12，docs/specs/rust-cli/node-store.md §4.4、§5 第 7 步）。
//!
//! Node `NodeToolArtifactStore` 的文件名是 `<sanitize(toolCallId)>-<artifactId><扩展名>`，读的时候在会话目录里找
//! 文件名含 artifact id 的文件（`readArtifactFile`）。本工程自己的文件名由引用完全决定（`artifacts::path`）；
//! Node 写的 checkpoint 没有本工程的引用，冷读取在这里按 Node 的读法补出（大小、类型、调用 id），`get` 找不到
//! 本工程命名的文件时也按同样的读法找。
use std::collections::HashMap;

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef, StoreError, WorkspaceCheckpoint};
use rust_cli_paths::StoragePath;

use crate::artifacts::{media_type, segment};
use crate::rewind::{Active, NodeCheckpoint};
use crate::storage;

/// 会话目录里含 artifact id 的文件名（Node `entries.find((entry) => entry.includes(artifactId))`）。
fn find<'a>(names: &'a [String], artifact: &str) -> Option<&'a String> {
    names.iter().find(|name| name.contains(artifact))
}

/// 冷读取的 checkpoint 清单：本工程写的原样；Node 写的按产物文件补出引用，文件不在时不给出（不能回退）。
pub async fn resolve(root: &StoragePath, listed: Vec<Active>) -> Vec<WorkspaceCheckpoint> {
    let mut directories: HashMap<String, Vec<String>> = HashMap::new();
    let mut out = Vec::with_capacity(listed.len());
    for entry in listed {
        match entry {
            Active::Rust(checkpoint) => out.push(checkpoint),
            Active::Node(node) => {
                if let Some(checkpoint) = node_checkpoint(root, &node, &mut directories).await {
                    out.push(checkpoint);
                }
            }
        }
    }
    out
}

async fn node_checkpoint(
    root: &StoragePath,
    node: &NodeCheckpoint,
    directories: &mut HashMap<String, Vec<String>>,
) -> Option<WorkspaceCheckpoint> {
    let directory = segment(node.session.as_str());
    let dir = root.join(&directory);
    if !directories.contains_key(&directory) {
        let names = rust_cli_fs::file_names(dir.as_path())
            .await
            .unwrap_or_default();
        directories.insert(directory.clone(), names);
    }
    let name = find(directories.get(&directory)?, node.artifact.as_str())?.clone();
    let size = rust_cli_fs::file_len(dir.join(&name).as_path())
        .await
        .ok()??;
    // 文件名里 `-<artifactId>` 之前是 Node 处理过的 toolCallId（本工程用它作这个 checkpoint 的调用 id）。
    let tool = name
        .split_once(&format!("-{}", node.artifact.as_str()))
        .map_or("", |(tool, _)| tool);
    Some(WorkspaceCheckpoint {
        id: node.id.clone(),
        prompt: node.prompt.clone(),
        assistant: node.assistant.clone(),
        tool_use: ToolUseId::new(tool),
        snapshot: ArtifactRef {
            session: node.session.clone(),
            id: node.artifact.clone(),
            purpose: ArtifactPurpose::Checkpoint,
            media_type: media_type(&name).to_owned(),
            size,
        },
    })
}

/// `get` 的退路：本工程命名的文件不在时，按 Node 的读法在会话目录里找；也没有时为 `None`。
pub async fn read_node_named(
    root: &StoragePath,
    artifact: &ArtifactRef,
) -> Result<Option<Vec<u8>>, StoreError> {
    let dir = root.join(segment(artifact.session.as_str()));
    let names = rust_cli_fs::file_names(dir.as_path())
        .await
        .map_err(storage)?;
    let Some(name) = find(&names, artifact.id.as_str()) else {
        return Ok(None);
    };
    rust_cli_fs::read_optional(dir.join(name).as_path())
        .await
        .map_err(storage)
}
