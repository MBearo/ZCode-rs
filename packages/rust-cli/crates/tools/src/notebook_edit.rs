//! NotebookEdit（tools-files.md §1）。
//!
//! 必须先 Read 过（不看模型与权限，一律要求），读后被改过就拒绝。替换、插入、删除一个单元格，
//! 写回 `JSON.stringify(notebook, null, 1)`，保留原编码与换行。

use std::hash::{DefaultHasher, Hash, Hasher};

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvError, EnvPath, ReadFile, WritePrecondition};
use rust_cli_kernel_api::env_files::PutFile;
use rust_cli_kernel_api::env_path;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use crate::common::{
    Definition, FileTools, cwd, definition, env_failure, failed, invalid, parse, resolve_input,
    stable_path, text, unstable_input,
};
use crate::encoding::{decode, encode};
use crate::file_state::{Baseline, FileState};
use crate::{checkpoint, notebook, texts};

pub const NAME: &str = "NotebookEdit";
/// 能处理的最大 notebook（100MB）。
const MAX_NOTEBOOK_BYTES: usize = 104_857_600;

#[derive(Deserialize)]
struct Input {
    notebook_path: String,
    cell_id: Option<String>,
    new_source: String,
    cell_type: Option<String>,
    edit_mode: Option<String>,
}

pub struct NotebookEditTool {
    files: FileTools,
    definition: Definition,
}

impl NotebookEditTool {
    pub fn new(files: FileTools) -> Self {
        Self {
            files,
            definition: definition(NAME),
        }
    }
}

fn mode(input: &Input) -> &str {
    input.edit_mode.as_deref().unwrap_or("replace")
}

/// 单元格位置：按 ID 找，找不到再按 `cell-N` 解析。
fn position(cells: &[Value], id: &str) -> Option<usize> {
    cells
        .iter()
        .position(|cell| cell.get("id").and_then(Value::as_str) == Some(id))
        .or_else(|| notebook::index_from_id(id))
}

/// 新单元格的 ID（8 位）：由这次操作的 ID 决定，重放时不变。
fn new_cell_id(context: &ToolUseContext) -> String {
    let mut hasher = DefaultHasher::new();
    context.operation.operation.as_str().hash(&mut hasher);
    format!("{:016x}", hasher.finish())[..8].to_owned()
}

#[async_trait]
impl Tool for NotebookEditTool {
    fn name(&self) -> &str {
        NAME
    }

    /// 延迟加载（docs/specs/rust-cli/tools-more.md §3.1）。
    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("edit Jupyter notebook cells (.ipynb)")
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(100_000)
    }

    async fn validate_input(&self, raw: &Value, context: &ToolUseContext) -> Validation {
        let Ok(input) = parse::<Input>(raw) else {
            return Validation::Ok;
        };
        let cwd = cwd(context).await;
        if let Some(refused) =
            unstable_input(context, &cwd, NAME, "notebook_path", &input.notebook_path).await
        {
            return refused;
        }
        let Ok(path) = resolve_input(context, &cwd, &input.notebook_path).await else {
            return Validation::Ok;
        };
        if env_path::extension(&path) != ".ipynb" {
            return invalid(texts::NOTEBOOK_ONLY.to_owned());
        }
        if mode(&input) == "insert" && input.cell_type.is_none() {
            return invalid(texts::NOTEBOOK_INSERT_TYPE.to_owned());
        }
        let Some(prior) = self.files.state.get(&context.session, &path) else {
            return invalid(texts::NOT_READ.to_owned());
        };
        let request = ReadFile {
            path: EnvPath::new(path.clone()),
        };
        let file = match context
            .machine
            .files()
            .read_file(&context.workspace, request)
            .await
        {
            Ok(file) => file,
            Err(EnvError::NotFound { .. }) => return invalid(texts::NOTEBOOK_MISSING.to_owned()),
            Err(_) => return Validation::Ok,
        };
        let decoded = decode(&file.bytes);
        // 只比版本，不比内容：版本变了就算改过。
        let changed = match &prior.baseline {
            Baseline::Version(version) => version != &file.version,
            Baseline::Content => prior.content != decoded.content,
        };
        if changed {
            return invalid(texts::MODIFIED_SINCE_READ.to_owned());
        }
        let parsed = match notebook::parse(&decoded.content) {
            Ok(parsed) => parsed,
            Err(message) => return invalid(message.to_owned()),
        };
        let cells = parsed["cells"].as_array().cloned().unwrap_or_default();
        match &input.cell_id {
            None if mode(&input) != "insert" => invalid(texts::NOTEBOOK_NEED_ID.to_owned()),
            None => Validation::Ok,
            Some(id)
                if cells
                    .iter()
                    .any(|cell| cell.get("id").and_then(Value::as_str) == Some(id)) =>
            {
                Validation::Ok
            }
            Some(id) => match notebook::index_from_id(id) {
                None => invalid(texts::notebook_cell_missing(id)),
                Some(index) if index >= cells.len() => {
                    invalid(texts::notebook_index_missing(index))
                }
                Some(_) => Validation::Ok,
            },
        }
    }

    async fn call(
        &self,
        raw: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let input: Input = parse(&raw)?;
        let cwd = cwd(&context).await;
        let path = stable_path(&context, &cwd, NAME, "notebook_path", &input.notebook_path).await?;
        let request = ReadFile {
            path: EnvPath::new(path.clone()),
        };
        let file = match context
            .machine
            .files()
            .read_file(&context.workspace, request)
            .await
        {
            Ok(file) => file,
            Err(EnvError::NotFound { .. }) => {
                return Err(failed(texts::NOTEBOOK_MISSING.to_owned()));
            }
            Err(error) => return Err(env_failure(error)),
        };
        let decoded = decode(&file.bytes);
        let mut parsed =
            notebook::parse(&decoded.content).map_err(|message| failed(message.to_owned()))?;
        let with_ids = notebook::has_cell_ids(&parsed);
        let mut edit_mode = mode(&input).to_owned();
        let mut cell_type = input.cell_type.clone();
        let cells = parsed["cells"]
            .as_array_mut()
            .ok_or_else(|| failed(texts::NOTEBOOK_INVALID.to_owned()))?;
        let mut index = match &input.cell_id {
            None => 0,
            Some(id) => position(cells, id).unwrap_or(usize::MAX),
        };
        if input.cell_id.is_some() && edit_mode == "insert" {
            index = index.saturating_add(1);
        }
        if edit_mode == "replace" && index == cells.len() {
            edit_mode = "insert".to_owned();
            cell_type.get_or_insert_with(|| "code".to_owned());
        }
        // nbformat ≥ 4.5 才有单元格 ID；更早的版本结果里是 JS 的 "undefined"。
        let new_id = with_ids.then(|| match edit_mode.as_str() {
            "insert" => Some(new_cell_id(&context)),
            _ => input.cell_id.clone(),
        });
        let new_id = new_id.flatten();
        match edit_mode.as_str() {
            "delete" => {
                if index >= cells.len() {
                    return Err(failed(texts::notebook_index_missing(index)));
                }
                cells.remove(index);
            }
            "insert" => {
                let mut cell = Map::new();
                let kind = if cell_type.as_deref() == Some("markdown") {
                    "markdown"
                } else {
                    "code"
                };
                cell.insert("cell_type".to_owned(), json!(kind));
                if let Some(id) = &new_id {
                    cell.insert("id".to_owned(), json!(id));
                }
                cell.insert("source".to_owned(), json!(input.new_source));
                cell.insert("metadata".to_owned(), json!({}));
                if kind == "code" {
                    cell.insert("execution_count".to_owned(), Value::Null);
                    cell.insert("outputs".to_owned(), json!([]));
                }
                cells.insert(index.min(cells.len()), Value::Object(cell));
            }
            _ => {
                let cell = cells
                    .get_mut(index)
                    .ok_or_else(|| failed(texts::notebook_index_missing(index)))?;
                cell["source"] = json!(input.new_source);
                if cell["cell_type"] == "code" {
                    cell["execution_count"] = Value::Null;
                    cell["outputs"] = json!([]);
                }
                if let Some(kind) = &cell_type
                    && cell["cell_type"].as_str() != Some(kind.as_str())
                {
                    cell["cell_type"] = json!(kind);
                }
            }
        }
        let updated = notebook::serialize(&parsed);
        let bytes = encode(&updated, decoded.encoding, decoded.line_endings);
        if bytes.len() > MAX_NOTEBOOK_BYTES {
            return Err(failed("Edit rejected: the updated notebook would exceed the maximum size this tool can read (100MB). The file was left unchanged.".to_owned()));
        }
        let prior = self.files.state.get(&context.session, &path);
        let request = PutFile {
            path: EnvPath::new(path.clone()),
            bytes: bytes.clone(),
            precondition: WritePrecondition::Version(file.version.clone()),
            create_parents: false,
        };
        let written = context
            .machine
            .files()
            .put_file(&context.workspace, context.operation.clone(), request)
            .await
            .map_err(env_failure)?;
        let change = checkpoint::Change {
            tool: NAME,
            path: &path,
            before: Some(&file.bytes),
            after: &bytes,
            mtime_ms: written.mtime_ms,
        };
        let checkpoint = checkpoint::record(&context, change).await;
        let unchanged_since_read = prior
            .as_ref()
            .is_some_and(|prior| prior.baseline == Baseline::Version(file.version.clone()));
        self.files.state.set(
            &context.session,
            &path,
            FileState {
                content: updated,
                baseline: Baseline::Version(written.version),
                offset: None,
                limit: None,
                partial: false,
                not_in_context: !prior.as_ref().is_some_and(FileState::is_in_context)
                    || !unchanged_since_read,
            },
        );
        let shown = new_id.unwrap_or_else(|| "undefined".to_owned());
        let message = match edit_mode.as_str() {
            "insert" => format!("Inserted cell {shown} with {}", input.new_source),
            "delete" => format!("Deleted cell {shown}"),
            _ => format!("Updated cell {shown} with {}", input.new_source),
        };
        let mut display = json!({"notebook_path": path, "edit_mode": edit_mode});
        if let Some(checkpoint) = checkpoint {
            display["checkpoint"] = checkpoint;
        }
        Ok(ToolOutput {
            display,
            ..text(message)
        })
    }
}
