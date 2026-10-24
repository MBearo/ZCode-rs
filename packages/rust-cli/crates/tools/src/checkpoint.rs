//! 文件写入的 checkpoint（docs/specs/rust-cli/session-lifecycle.md §4）。
//!
//! Write / Edit / NotebookEdit 写入成功后，把写入前后的磁盘内容写成 Node 格式的 artifact
//! （`workspaceCheckpointArtifactSchema`，Node `stringifyWorkspaceCheckpointArtifact` 的键序与缩进），
//! 引用放进 `ToolOutput.display.checkpoint`；会话 actor 把它与工具结果在一个事务里落库。
//! checkpoint 失败只记日志，不影响工具结果（Node `emitFileMutationCheckpoint` 相同）。
//!
//! 远端机器上的写入（R3，remote-exec.md §20.7.5、§20.9）：artifact 与 `display.checkpoint` 多一个 `machine`；会话 actor 把它
//! 存成本工程的记录（Node / App 看不到，不会把远端文件的内容写到本机的同名路径）。

use rust_cli_base::js::iso_timestamp;
use rust_cli_kernel_api::store::ArtifactPurpose;
use rust_cli_kernel_api::tool::ToolUseContext;
use serde_json::{Value, json};

/// 上下文行数（jsdiff `structuredPatch` 的默认值）。
const CONTEXT: usize = 3;

/// 一次写入：路径、写入前的字节（文件不存在时 None）、写入后的字节、写入后的 mtime。
pub(crate) struct Change<'a> {
    pub tool: &'a str,
    pub path: &'a str,
    pub before: Option<&'a [u8]>,
    pub after: &'a [u8],
    pub mtime_ms: u64,
}

/// 写 artifact，返回 `display.checkpoint`；内容不是 UTF-8 或写 artifact 失败时为 None。
pub(crate) async fn record(context: &ToolUseContext, change: Change<'_>) -> Option<Value> {
    let before = match change.before {
        Some(bytes) => Some(std::str::from_utf8(bytes).ok()?),
        None => None,
    };
    let after = std::str::from_utf8(change.after).ok()?;
    let (hunks, additions, deletions) = structured_patch(before.unwrap_or(""), after);
    // 键序同 Node 的对象字面量（JSON.stringify(artifact, null, 2)）。
    let mut artifact = json!({
        "createdAt": iso_timestamp(change.mtime_ms),
        "files": [{
            "afterContent": after,
            "afterContentLength": after.encode_utf16().count(),
            "beforeContent": before,
            "existedBefore": before.is_some(),
            "path": change.path,
            "structuredPatch": hunks,
        }],
        "kind": "workspace_file_before_change",
        "toolCallId": context.tool_use.as_str(),
        "toolName": change.tool,
        "version": 1,
    });
    let machine = (!context.machine.is_local()).then(|| context.machine.name().to_owned());
    if let Some(name) = &machine {
        artifact["machine"] = Value::from(name.as_str());
    }
    let bytes = serde_json::to_string_pretty(&artifact).ok()?.into_bytes();
    match context
        .artifacts
        .put(
            &context.session,
            ArtifactPurpose::Checkpoint,
            "application/json".to_owned(),
            bytes,
        )
        .await
    {
        Ok(reference) => {
            let mut display = json!({
                "artifact": reference,
                "path": change.path,
                "additions": additions,
                "deletions": deletions,
            });
            if let Some(name) = machine {
                display["machine"] = Value::from(name);
            }
            Some(display)
        }
        Err(error) => {
            tracing::warn!(%error, path = change.path, "workspace checkpoint creation failed");
            None
        }
    }
}

/// 去掉公共前后缀后的一个 hunk（带 3 行上下文）；返回 hunk 列表与增删行数。
/// 不是最小 diff：Node / App 有 `afterContent` 时按前后内容重算统计（spec §4.1）。
pub(crate) fn structured_patch(before: &str, after: &str) -> (Vec<Value>, usize, usize) {
    let old: Vec<&str> = lines(before);
    let new: Vec<&str> = lines(after);
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let removed = &old[prefix..old.len() - suffix];
    let added = &new[prefix..new.len() - suffix];
    if removed.is_empty() && added.is_empty() {
        return (Vec::new(), 0, 0);
    }
    let lead = prefix.min(CONTEXT);
    let trail = suffix.min(CONTEXT);
    let mut body: Vec<String> = old[prefix - lead..prefix]
        .iter()
        .map(|line| format!(" {line}"))
        .collect();
    body.extend(removed.iter().map(|line| format!("-{line}")));
    body.extend(added.iter().map(|line| format!("+{line}")));
    let tail = old.len() - suffix;
    body.extend(
        old[tail..tail + trail]
            .iter()
            .map(|line| format!(" {line}")),
    );
    let old_lines = lead + removed.len() + trail;
    let new_lines = lead + added.len() + trail;
    // jsdiff：某一侧没有行时起始行号为 0。
    let start = |count: usize| if count == 0 { 0 } else { prefix - lead + 1 };
    let hunk = json!({
        "oldStart": start(old_lines),
        "oldLines": old_lines,
        "newStart": start(new_lines),
        "newLines": new_lines,
        "lines": body,
    });
    (vec![hunk], added.len(), removed.len())
}

/// 按 `\n` 分行，行内不含换行符；末尾的换行不产生空行。
fn lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .collect()
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
