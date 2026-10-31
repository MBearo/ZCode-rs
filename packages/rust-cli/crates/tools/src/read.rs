//! Read（tools-files.md §1）。
//!
//! 文本：按范围选行，每行 `<行号>\t<内容>`；重复读取未变的文件返回固定提示；不加 limit
//! 时文件超过 256KB 报错，估算超过 25000 token 报错。notebook 渲染成单元格块。图片原样内联
//! （不缩放）。PDF 与 `pages` 见 [`crate::read_pdf`]（tools-files.md §1.5）。

use async_trait::async_trait;
use rust_cli_base::proto::mime;
use rust_cli_kernel_api::env::{EnvError, EnvPath, ReadFile};
use rust_cli_kernel_api::env_files::{ReadText, TextRead};
use rust_cli_kernel_api::env_path;
use rust_cli_kernel_api::ids::{ArtifactId, SessionId};
use rust_cli_kernel_api::message::{ImageSource, ToolResultContent};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::common::{
    Definition, FileTools, cwd, definition, env_failure, failed, format_bytes, invalid,
    missing_file, parse, resolve_input, stable_path, stat, text, unstable_input,
};
use crate::file_state::{Baseline, FileState};
use crate::read_support::{
    IMAGES, MAX_BYTES, MAX_IMAGE_BYTES, check_tokens, image_type, is_binary, is_device,
    number_lines,
};
use crate::{base64, budget, notebook, read_pdf, texts};

pub const NAME: &str = "Read";

#[derive(Deserialize)]
struct Input {
    file_path: String,
    offset: Option<u64>,
    limit: Option<u64>,
    /// PDF 的页码范围（tools-files.md §1.5）；不是 PDF 时通过检查后不起作用。
    #[serde(default)]
    pages: Option<String>,
}

pub struct ReadTool {
    files: FileTools,
    definition: Definition,
}

impl ReadTool {
    pub fn new(files: FileTools) -> Self {
        Self {
            files,
            definition: definition(NAME),
        }
    }
}

/// 落盘的大结果（`zcode-artifact://<会话>/<artifact>`，见 [`budget`]；tools-files.md §1.4）：按读文件的规则
/// 给结果（256KB 与 25000 token 的上限、行号、offset 越界的提示），不进 readFileState。
///
/// 缺陷修复（F18）：以前在这里自己造引用（`size: 0`、`ToolOutput`、`text/plain`），生产的 NodeStore `get` 核对
/// 大小，报 `has N bytes, expected 0`（多段结果的文件是 `.json`，文件名也算错）；测试替身不核对大小，单测没拦住。
/// 位置里只有会话与 artifact：引用由存储按这两项找回（`ArtifactStore::find`），再照常 `get`。
async fn read_artifact(
    context: &ToolUseContext,
    location: &str,
    input: &Input,
) -> Result<ToolOutput, ToolError> {
    // 地址原样交给存储，不按路径规范化（F14）：第一个 `/` 之后都是 artifact id，存储自己决定认不认。
    let (session, id) = location
        .split_once('/')
        .filter(|(session, id)| !session.is_empty() && !id.is_empty())
        .ok_or_else(|| failed(format!("Invalid artifact location: {}", input.file_path)))?;
    let unreadable =
        |reason: String| failed(format!("Could not read {}: {reason}", input.file_path));
    let artifact = context
        .artifacts
        .find(&SessionId::new(session), &ArtifactId::new(id))
        .await
        .map_err(|error| unreadable(error.to_string()))?
        .ok_or_else(|| unreadable("artifact not found".to_owned()))?;
    if input.limit.is_none() && artifact.size > MAX_BYTES {
        return Err(failed(texts::too_large(
            &format_bytes(artifact.size),
            &format_bytes(MAX_BYTES),
        )));
    }
    let bytes = context
        .artifacts
        .get(&artifact)
        .await
        .map_err(|error| unreadable(error.to_string()))?;
    let body = String::from_utf8_lossy(&bytes);
    let offset = input.offset.unwrap_or(1);
    let skip = usize::try_from(offset.saturating_sub(1)).unwrap_or(usize::MAX);
    let take = input.limit.map_or(usize::MAX, |limit| {
        usize::try_from(limit).unwrap_or(usize::MAX)
    });
    let lines: Vec<&str> = body.split('\n').skip(skip).take(take).collect();
    if lines.is_empty() {
        return Ok(text(texts::shorter_than_offset(
            offset,
            body.split('\n').count() as u64,
        )));
    }
    let content = lines.join("\n");
    // 估算按读落盘文件时的扩展名：多段结果是 `.json`（2 字符 / token），其余是 `.txt`。
    let extension = if artifact.media_type == mime::JSON {
        ".json"
    } else {
        ".txt"
    };
    check_tokens(&content, extension)?;
    Ok(text(number_lines(&content, offset)))
}

impl ReadTool {
    async fn read_notebook(
        &self,
        context: &ToolUseContext,
        path: &str,
    ) -> Result<ToolOutput, ToolError> {
        let request = ReadFile {
            path: EnvPath::new(path),
        };
        let file = match context
            .machine
            .files()
            .read_file(&context.workspace, request)
            .await
        {
            Ok(file) => file,
            Err(EnvError::NotFound { .. }) => {
                return Err(failed(
                    missing_file(context, &cwd(context).await, path).await,
                ));
            }
            Err(error) => return Err(env_failure(error)),
        };
        let parsed = notebook::parse(&String::from_utf8_lossy(&file.bytes)).map_err(|message| {
            failed(if message == texts::NOTEBOOK_BAD_JSON {
                "Notebook file is not valid JSON (it may be truncated, corrupted, or still being written)".to_owned()
            } else {
                message.to_owned()
            })
        })?;
        let cells = notebook::cells(&parsed);
        let content = rust_cli_base::js::json::stringify(&Value::Array(cells.clone()));
        if content.len() as u64 > MAX_BYTES {
            return Err(failed(format!(
                "Notebook content ({}) exceeds maximum allowed size ({}). Use Bash with jq to read specific portions:\n  cat <notebook_path> | jq '.cells[:20]' # First 20 cells\n  cat <notebook_path> | jq '.cells[100:120]' # Cells 100-120\n  cat <notebook_path> | jq '.cells | length' # Count total cells\n  cat <notebook_path> | jq '.cells[] | select(.cell_type==\"code\") | .source' # All code sources",
                format_bytes(content.len() as u64),
                format_bytes(MAX_BYTES)
            )));
        }
        check_tokens(&content, ".ipynb")?;
        self.files.state.set_at(
            &context.session,
            context.machine.name(),
            path,
            FileState {
                content,
                baseline: Baseline::Version(file.version),
                offset: Some(1),
                limit: None,
                partial: false,
                not_in_context: false,
            },
        );
        Ok(ToolOutput {
            content: notebook::render(&cells),
            is_error: false,
            display: json!({"type": "notebook", "file": {"filePath": path}}),
        })
    }

    async fn read_image(
        &self,
        context: &ToolUseContext,
        path: &str,
    ) -> Result<ToolOutput, ToolError> {
        let request = ReadFile {
            path: EnvPath::new(path),
        };
        let file = match context
            .machine
            .files()
            .read_file(&context.workspace, request)
            .await
        {
            Ok(file) => file,
            Err(EnvError::NotFound { .. }) => {
                return Err(failed(
                    missing_file(context, &cwd(context).await, path).await,
                ));
            }
            Err(error) => return Err(env_failure(error)),
        };
        let media_type = image_type(&file.bytes).unwrap_or(mime::PNG);
        if file.bytes.len() > MAX_IMAGE_BYTES {
            return Err(failed(texts::too_large(
                &format_bytes(file.bytes.len() as u64),
                &format_bytes(MAX_IMAGE_BYTES as u64),
            )));
        }
        Ok(ToolOutput {
            content: vec![ToolResultContent::Image {
                source: ImageSource::Base64 {
                    media_type: media_type.to_owned(),
                    data: base64::encode(&file.bytes),
                },
            }],
            is_error: false,
            display: json!({"type": "image", "file": {"type": media_type, "originalSize": file.bytes.len()}}),
        })
    }

    /// 同样的 offset / limit 再读一次、文件版本没变：不再返回内容。
    async fn unchanged(&self, context: &ToolUseContext, path: &str, input: &Input) -> bool {
        let Some(prior) = self
            .files
            .state
            .get_at(&context.session, context.machine.name(), path)
        else {
            return false;
        };
        let offset = input.offset.unwrap_or(1);
        if prior.partial
            || prior.not_in_context
            || prior.offset != Some(offset)
            || prior.limit != input.limit
        {
            return false;
        }
        let Baseline::Version(version) = &prior.baseline else {
            return false;
        };
        matches!(stat(context, path).await, Ok(Some(current)) if &current.version == version)
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        NAME
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    async fn validate_input(&self, input: &Value, context: &ToolUseContext) -> Validation {
        let Ok(input) = parse::<Input>(input) else {
            return Validation::Ok;
        };
        // `pages` 的语法与上限对任何路径都查（在“以空白结尾”之后、二进制检查之前）。
        let pages = || match input.pages.as_deref().map(read_pdf::check) {
            Some(Err(refused)) => refused,
            _ => Validation::Ok,
        };
        // 落盘的大结果的地址不是执行环境的路径（与 `call` 相同，不经路径规范化，tools-files.md §3.2）。
        if input.file_path.starts_with(budget::ARTIFACT_SCHEME) {
            return pages();
        }
        let cwd = cwd(context).await;
        if let Some(refused) =
            unstable_input(context, &cwd, NAME, "file_path", &input.file_path).await
        {
            return refused;
        }
        if let refused @ Validation::Invalid { .. } = pages() {
            return refused;
        }
        let Ok(path) = resolve_input(context, &cwd, &input.file_path).await else {
            return Validation::Ok;
        };
        if is_binary(&path) {
            return invalid(texts::binary_file(&env_path::extension(&path)));
        }
        if is_device(&path) {
            return invalid(texts::device_file(&crate::common::escape_control(
                &input.file_path,
            )));
        }
        Validation::Ok
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let input: Input = parse(&input)?;
        if let Some(location) = input.file_path.strip_prefix(budget::ARTIFACT_SCHEME) {
            return read_artifact(&context, location, &input).await;
        }
        let cwd = cwd(&context).await;
        let path = stable_path(&context, &cwd, NAME, "file_path", &input.file_path).await?;
        let extension = env_path::extension(&path);
        if extension == ".ipynb" {
            // notebook 总是整份读（offset 定为 1、去掉 limit）。
            let whole = Input {
                offset: Some(1),
                limit: None,
                ..input
            };
            if self.unchanged(&context, &path, &whole).await {
                return Ok(text(texts::FILE_UNCHANGED.to_owned()));
            }
            return self.read_notebook(&context, &path).await;
        }
        if IMAGES.contains(&extension.as_str()) {
            return self.read_image(&context, &path).await;
        }
        // PDF 不写 readFileState，所以排在“未变”判断之前。
        if extension == ".pdf" {
            let model = self.files.model.as_ref();
            let pages = input.pages.as_deref();
            return read_pdf::read(&context, (&cwd, &path), pages, model).await;
        }
        if self.unchanged(&context, &path, &input).await {
            return Ok(text(texts::FILE_UNCHANGED.to_owned()));
        }
        let offset = input.offset.unwrap_or(1);
        let request = ReadText {
            path: EnvPath::new(path.clone()),
            start_line: offset.saturating_sub(1),
            max_lines: input.limit,
            max_bytes: input.limit.is_none().then_some(MAX_BYTES),
        };
        let slice = match context
            .machine
            .files()
            .read_text(&context.workspace, request)
            .await
        {
            Ok(TextRead::Text(slice)) => slice,
            Ok(TextRead::Directory) => return Err(failed(texts::is_directory(&path))),
            Ok(TextRead::TooLarge { size }) => {
                return Err(failed(texts::too_large(
                    &format_bytes(size),
                    &format_bytes(MAX_BYTES),
                )));
            }
            Err(EnvError::NotFound { .. }) => {
                return Err(failed(missing_file(&context, &cwd, &path).await));
            }
            Err(error) => return Err(env_failure(error)),
        };
        check_tokens(&slice.content, &extension)?;
        let body = if !slice.content.is_empty() {
            number_lines(&slice.content, offset)
        } else if slice.line_count >= 1 && slice.total_lines > 1 {
            format!("{offset}\t")
        } else if slice.line_count >= 1 || slice.total_lines == 0 {
            texts::EMPTY_FILE.to_owned()
        } else {
            texts::shorter_than_offset(offset, slice.total_lines)
        };
        let display = json!({"type": "text", "file": {
            "filePath": input.file_path, "numLines": slice.line_count,
            "startLine": offset, "totalLines": slice.total_lines,
        }});
        self.files.state.set_at(
            &context.session,
            context.machine.name(),
            &path,
            FileState {
                content: slice.content,
                baseline: Baseline::Version(slice.version),
                offset: Some(offset),
                limit: input.limit,
                partial: false,
                not_in_context: false,
            },
        );
        Ok(ToolOutput {
            display,
            ..text(body)
        })
    }
}

#[cfg(test)]
#[path = "read_artifact_tests.rs"]
mod artifact_tests;

#[cfg(test)]
#[path = "read_pdf_tests.rs"]
mod pdf_tests;
