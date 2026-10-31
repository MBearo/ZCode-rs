//! Edit（tools-files.md §1）。
//!
//! 先读规则与“读后被改过”同 Write，但多一条恢复：写入会被直接放行、且编辑仍能干净地套上时照常
//! 编辑，结果里注明文件另有改动。匹配允许弯引号与 `\uXXXX` 两种写法（[`crate::edit_match`]）。

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvError, EnvPath, FileVersion, ReadFile, WritePrecondition};
use rust_cli_kernel_api::env_files::PutFile;
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
use crate::edit_match::{adapt_new, applies, apply, find_actual, wants_escape_hint};
use crate::encoding::{Decoded, encode};
use crate::file_state::{Baseline, FileState};
use crate::{checkpoint, notebook_edit, texts};

pub const NAME: &str = "Edit";
/// 能编辑的最大文件（1GB）。
const MAX_EDIT_BYTES: u64 = 1_073_741_824;

#[derive(Deserialize)]
struct Input {
    file_path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

pub struct EditTool {
    files: FileTools,
    definition: Definition,
}

/// 读取规则的结论。
enum Freshness {
    Fresh,
    /// 读后被改过，但编辑仍能套上、写入会被直接放行。
    Recovered,
    Rejected(String),
}

impl EditTool {
    pub fn new(files: FileTools) -> Self {
        Self {
            files,
            definition: definition(NAME),
        }
    }

    async fn current(
        &self,
        context: &ToolUseContext,
        path: &str,
    ) -> Result<Option<(Decoded, FileVersion, Vec<u8>)>, ToolError> {
        let request = ReadFile {
            path: EnvPath::new(path),
        };
        match context
            .machine
            .files()
            .read_file(&context.workspace, request)
            .await
        {
            Ok(file) => Ok(Some((
                crate::encoding::decode(&file.bytes),
                file.version,
                file.bytes,
            ))),
            Err(EnvError::NotFound { .. }) => Ok(None),
            Err(error) => Err(env_failure(error)),
        }
    }

    async fn freshness(
        &self,
        raw: &Value,
        input: &Input,
        context: &ToolUseContext,
        path: &str,
        content: &str,
        version: &FileVersion,
    ) -> Freshness {
        let prior = self
            .files
            .state
            .get_at(&context.session, context.machine.name(), path);
        let recoverable = || async {
            applies(content, &input.old_string, input.replace_all)
                && self.files.auto_allowed(self, raw, context).await
        };
        match prior {
            Some(prior) if !prior.partial => {
                if !prior.changed(version, || Some(content.to_owned())) {
                    Freshness::Fresh
                } else if recoverable().await {
                    Freshness::Recovered
                } else {
                    Freshness::Rejected(texts::MODIFIED_SINCE_READ.to_owned())
                }
            }
            prior => {
                if !self.files.auto_allowed(self, raw, context).await {
                    return Freshness::Rejected(texts::NOT_READ.to_owned());
                }
                // 只看过一部分时，仍按版本判断是否被改过。
                match prior {
                    Some(prior) if prior.changed(version, || None) && !recoverable().await => {
                        Freshness::Rejected(texts::MODIFIED_SINCE_READ.to_owned())
                    }
                    _ => Freshness::Fresh,
                }
            }
        }
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        NAME
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(rust_cli_kernel_api::tool::DEFAULT_MAX_RESULT_CHARS)
    }

    async fn validate_input(&self, raw: &Value, context: &ToolUseContext) -> Validation {
        let Ok(input) = parse::<Input>(raw) else {
            return Validation::Ok;
        };
        // 先查路径，再查 old_string 与 new_string 相同。
        let cwd = cwd(context).await;
        if let Some(refused) =
            unstable_input(context, &cwd, NAME, "file_path", &input.file_path).await
        {
            return refused;
        }
        if input.old_string == input.new_string {
            return invalid(texts::EDIT_NO_CHANGES.to_owned());
        }
        let Ok(path) = resolve_input(context, &cwd, &input.file_path).await else {
            return Validation::Ok;
        };
        if let Ok(Some(current)) = stat(context, &path).await
            && current.size > MAX_EDIT_BYTES
        {
            return invalid(texts::edit_too_large(
                &format_bytes(current.size),
                &format_bytes(MAX_EDIT_BYTES),
            ));
        }
        let current = match self.current(context, &path).await {
            Ok(current) => current,
            Err(_) => return Validation::Ok,
        };
        let Some((decoded, version, _)) = current else {
            if input.old_string.is_empty() {
                return Validation::Ok;
            }
            return invalid(missing_file(context, &cwd, &path).await);
        };
        let content = decoded.content;
        if input.old_string.is_empty() {
            if !content.trim().is_empty() {
                return invalid(texts::EDIT_FILE_EXISTS.to_owned());
            }
            return Validation::Ok;
        }
        if path.ends_with(".ipynb") {
            return invalid(texts::edit_notebook(notebook_edit::NAME));
        }
        if let Freshness::Rejected(message) = self
            .freshness(raw, &input, context, &path, &content, &version)
            .await
        {
            return invalid(message);
        }
        let Some(actual) = find_actual(&content, &input.old_string) else {
            let hint = if wants_escape_hint(&input.old_string) {
                texts::EDIT_ESCAPE_HINT
            } else {
                ""
            };
            return invalid(format!(
                "{}{hint}",
                texts::edit_not_found(&input.old_string)
            ));
        };
        let matches = content.matches(actual.as_str()).count();
        if matches > 1 && !input.replace_all {
            return invalid(texts::edit_ambiguous(matches, &input.old_string));
        }
        Validation::Ok
    }

    async fn call(
        &self,
        raw: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let input: Input = parse(&raw)?;
        let cwd = cwd(&context).await;
        let path = stable_path(&context, &cwd, NAME, "file_path", &input.file_path).await?;
        let current = self.current(&context, &path).await?;
        let lastly = self
            .files
            .state
            .get_at(&context.session, context.machine.name(), &path);
        let (content, encoding, endings, precondition, exists) = match &current {
            Some((decoded, version, _)) => (
                decoded.content.clone(),
                decoded.encoding,
                decoded.line_endings,
                WritePrecondition::Version(version.clone()),
                true,
            ),
            None => (
                String::new(),
                crate::encoding::Encoding::Utf8,
                crate::encoding::LineEndings::Lf,
                WritePrecondition::Absent,
                false,
            ),
        };
        let recovered = match &current {
            Some((_, version, _)) if !input.old_string.is_empty() => {
                match self
                    .freshness(&raw, &input, &context, &path, &content, version)
                    .await
                {
                    Freshness::Fresh => false,
                    Freshness::Recovered => true,
                    Freshness::Rejected(message) => return Err(failed(message)),
                }
            }
            _ => false,
        };
        let actual =
            find_actual(&content, &input.old_string).unwrap_or_else(|| input.old_string.clone());
        let new_string = adapt_new(&input.old_string, &actual, &input.new_string);
        let updated = apply(&content, &actual, &new_string, input.replace_all)
            .map_err(|message| failed(message.to_owned()))?;
        let not_in_context =
            exists && (!lastly.as_ref().is_some_and(FileState::is_in_context) || recovered);
        let bytes = encode(&updated, encoding, endings);
        let request = PutFile {
            path: EnvPath::new(path.clone()),
            bytes: bytes.clone(),
            precondition,
            create_parents: input.old_string.is_empty(),
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
            before: current.as_ref().map(|(_, _, before)| before.as_slice()),
            after: &bytes,
            mtime_ms: written.mtime_ms,
        };
        let checkpoint = checkpoint::record(&context, change).await;
        self.files.state.set_at(
            &context.session,
            context.machine.name(),
            &path,
            FileState {
                content: updated
                    .strip_prefix('\u{feff}')
                    .unwrap_or(&updated)
                    .to_owned(),
                baseline: Baseline::Version(written.version),
                offset: None,
                limit: None,
                partial: exists && lastly.is_some_and(|state| state.partial),
                not_in_context,
            },
        );
        let notice = if recovered {
            texts::STALE_RECOVERED
        } else if not_in_context {
            ""
        } else {
            texts::STATE_IS_CURRENT
        };
        let message = if input.replace_all {
            format!(
                "The file {} has been updated. All occurrences were successfully replaced.{notice}",
                input.file_path
            )
        } else {
            format!(
                "The file {} has been updated successfully.{notice}",
                input.file_path
            )
        };
        let mut display = json!({"filePath": input.file_path, "replaceAll": input.replace_all});
        if let Some(checkpoint) = checkpoint {
            display["checkpoint"] = checkpoint;
        }
        Ok(ToolOutput {
            display,
            ..text(message)
        })
    }
}
