//! The Write / Edit result (Node `writeEditResult` and Write's result): the
//! adapter data, the diff display, the model text and the telemetry detail
//! (Node `ToolExecutionTelemetry` `filesystem` / `patch`).
use super::file_write::patch;
use super::tool_edit as edit;
use super::tool_files::FileTools;
use super::tools::string;
use crate::contract::ToolOutput;
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;

/// What one Write / Edit call wrote.
pub(super) struct Written {
    pub path: PathBuf,
    /// The file existed.
    pub original: bool,
    pub old: String,
    pub new: String,
    pub search: String,
    pub replacement: String,
    pub replace_all: bool,
    /// Edit's match `(strategy, candidates)`.
    pub planned: Option<(&'static str, usize)>,
}

/// Node `fsReadMs`, `fsWriteMs` and `patchMatchMs` (`None` when no match ran).
pub(super) struct Timings {
    pub read_ms: u64,
    pub write_ms: u64,
    pub match_ms: Option<u64>,
}

/// Node's `filesystem` detail, with `patch` for Edit; the workspace kind is
/// `local` (the engine marks subagent calls `unknown`).
fn perf(edit: bool, bytes: usize, hunks: usize, t: &Timings) -> Value {
    let filesystem = json!({"readMs": t.read_ms, "writeMs": t.write_ms, "fileCount": 1,
        "totalBytes": bytes, "maxFileBytes": bytes, "workspaceKind": "local"});
    if !edit {
        return json!({"kind": "filesystem", "filesystem": filesystem});
    }
    json!({"kind": "patch", "filesystem": filesystem, "patch": {"matchMs": t.match_ms.unwrap_or(0),
        "hunkCount": hunks, "matchAttempts": u8::from(t.match_ms.is_some())}})
}

pub(super) async fn result(
    tools: &FileTools<'_>,
    name: &str,
    args: &Value,
    w: Written,
    timings: Timings,
) -> Result<ToolOutput> {
    let edit = name == "Edit";
    let Written { path, old, new, .. } = &w;
    let (patch, additions, deletions) = patch(old, new);
    let hunks = patch.as_array().map_or(0, Vec::len);
    let mut data = if name == "Write" {
        json!({"type":if w.original{"update"}else{"create"},"filePath":path,"content":new,"originalFile":w.original.then_some(old),"structuredPatch":patch,"userModified":false})
    } else {
        json!({"filePath":path,"oldString":w.search,"newString":w.replacement,"originalFile":old,"structuredPatch":patch,"userModified":false,"replaceAll":w.replace_all,"matchStrategy":w.planned.map(|p|p.0),"matchCandidateCount":w.planned.map(|p|p.1)})
    };
    let mut display = json!({"kind":"file_diff","filePath":path,"additions":additions,"deletions":deletions,"structuredPatch":data["structuredPatch"]});
    if serde_json::to_vec(&display)?.len() > 32 * 1024 {
        display["structuredPatch"] = json!([]);
        display["truncated"] = true.into();
    }
    let mut content = if edit {
        // Node formatEditModelContent：使用模型给出的 file_path。
        edit::model_content(string(args, "file_path")?, w.replace_all)
    } else {
        // 修复：原先是旧文案 "The file <绝对路径> has been written successfully."；Node
        // formatWriteModelContent 区分新建与覆盖，用模型给出的 file_path，并提示文件状态已在上下文中。
        let path = string(args, "file_path")?;
        if w.original {
            format!(
                "The file {path} has been updated successfully.{}",
                edit::FRESHNESS_SUFFIX
            )
        } else {
            format!(
                "File created successfully at: {path}{}",
                edit::FRESHNESS_SUFFIX
            )
        }
    };
    if !edit && serde_json::to_vec(&data)?.len() > 64 * 1024 {
        tokio::fs::create_dir_all(tools.artifacts).await?;
        let artifact = tools.artifacts.join(format!("{}.json", super::id()));
        tokio::fs::write(&artifact, serde_json::to_vec(&data)?).await?;
        content.push_str(&format!(" Full change result: {}", artifact.display()));
    }
    // data is adapter-local validation output; only bounded display/model text crosses stdio.
    let mut result = ToolOutput::new(content, std::mem::take(&mut data));
    result.display = Some(display);
    result.perf = Some(perf(edit, new.len(), hunks, &timings));
    Ok(result)
}
