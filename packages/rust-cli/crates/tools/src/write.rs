//! Write（tools-files.md §1）。
//!
//! 覆盖已有文件前必须读过它，且读后没被改过；较新的模型在写入会被权限规则直接放行时，未读过的
//! 已有文件也可以写（tools-files.md §1.2）。写入保留原编码，换行按给出的内容（LF）。

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvError, EnvPath, FileVersion, ReadFile, WritePrecondition};
use rust_cli_kernel_api::env_files::{FileKind, PutFile};
use rust_cli_kernel_api::env_path;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::common::{
    Definition, FileTools, cwd, definition, env_failure, escape_control, failed, invalid, parse,
    resolve_input, stable_path, stat, style, text, unstable_input,
};
use crate::encoding::{Decoded, Encoding, LineEndings, decode, encode, normalize};
use crate::file_state::{Baseline, FileState};
use crate::{checkpoint, texts};

pub const NAME: &str = "Write";

#[derive(Deserialize)]
struct Input {
    file_path: String,
    content: String,
}

pub struct WriteTool {
    files: FileTools,
    definition: Definition,
}

impl WriteTool {
    pub fn new(files: FileTools) -> Self {
        Self {
            files,
            definition: definition(NAME),
        }
    }

    /// 已有文件的读取规则（validateInput 与写入共用）；`None` 表示可以写。
    async fn guard(
        &self,
        input: &Value,
        context: &ToolUseContext,
        path: &str,
        current: &FileVersion,
        content: impl FnOnce() -> Option<String>,
    ) -> Option<String> {
        let prior = self
            .files
            .state
            .get_at(&context.session, context.machine.name(), path);
        match prior {
            Some(prior) if !prior.partial => prior
                .changed(current, content)
                .then(|| texts::MODIFIED_SINCE_READ.to_owned()),
            prior => {
                let skipped = prior.is_none()
                    && env_path::extension(path) != ".ipynb"
                    && self.files.auto_allowed(self, input, context).await;
                (!skipped).then(|| texts::NOT_READ.to_owned())
            }
        }
    }

    async fn existing(
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
            Ok(file) => Ok(Some((decode(&file.bytes), file.version, file.bytes))),
            Err(EnvError::NotFound { .. }) => Ok(None),
            Err(error) => Err(env_failure(error)),
        }
    }
}

/// 子代理不写报告文件（`/^(REPORT|SUMMARY|FINDINGS|ANALYSIS).*\.md$/i`）。
fn is_report(name: &str) -> bool {
    let upper = name.to_uppercase();
    ["REPORT", "SUMMARY", "FINDINGS", "ANALYSIS"]
        .iter()
        .any(|prefix| upper.starts_with(prefix))
        && upper.ends_with(".MD")
}

#[async_trait]
impl Tool for WriteTool {
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
        let cwd = cwd(context).await;
        if let Some(refused) =
            unstable_input(context, &cwd, NAME, "file_path", &input.file_path).await
        {
            return refused;
        }
        let Ok(path) = resolve_input(context, &cwd, &input.file_path).await else {
            return Validation::Ok;
        };
        if context.agent.is_some() && is_report(&style(context).file_name(&path)) {
            return invalid(texts::SUBAGENT_REPORT.to_owned());
        }
        let current = match stat(context, &path).await {
            Ok(Some(current)) => current,
            Ok(None) | Err(_) => return Validation::Ok,
        };
        match current.kind {
            FileKind::Directory => {
                return invalid(texts::write_directory(&escape_control(&input.file_path)));
            }
            FileKind::Other => {
                return invalid(texts::write_not_regular(&escape_control(&input.file_path)));
            }
            FileKind::File => {}
        }
        let existing = self.existing(context, &path).await.ok().flatten();
        let text = existing
            .as_ref()
            .map(|(decoded, _, _)| decoded.content.clone());
        match self
            .guard(raw, context, &path, &current.version, || text)
            .await
        {
            Some(message) => invalid(message),
            None => Validation::Ok,
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
        let path = stable_path(&context, &cwd, NAME, "file_path", &input.file_path).await?;
        let existing = self.existing(&context, &path).await?;
        if let Some((decoded, version, _)) = &existing {
            let text = decoded.content.clone();
            if let Some(message) = self
                .guard(&raw, &context, &path, version, || Some(text))
                .await
            {
                return Err(failed(message));
            }
        }
        let encoding = existing
            .as_ref()
            .map_or(Encoding::Utf8, |(decoded, _, _)| decoded.encoding);
        let bytes = encode(&input.content, encoding, LineEndings::Lf);
        let request = PutFile {
            path: EnvPath::new(path.clone()),
            bytes: bytes.clone(),
            precondition: existing
                .as_ref()
                .map_or(WritePrecondition::Absent, |(_, version, _)| {
                    WritePrecondition::Version(version.clone())
                }),
            create_parents: true,
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
            before: existing.as_ref().map(|(_, _, before)| before.as_slice()),
            after: &bytes,
            mtime_ms: written.mtime_ms,
        };
        let checkpoint = checkpoint::record(&context, change).await;
        self.files.state.set_at(
            &context.session,
            context.machine.name(),
            &path,
            FileState {
                content: normalize(&input.content),
                baseline: Baseline::Version(written.version),
                offset: None,
                limit: None,
                partial: false,
                not_in_context: false,
            },
        );
        let created = existing.is_none();
        let message = if created {
            format!(
                "File created successfully at: {}{}",
                input.file_path,
                texts::STATE_IS_CURRENT
            )
        } else {
            format!(
                "The file {} has been updated successfully.{}",
                input.file_path,
                texts::STATE_IS_CURRENT
            )
        };
        let mut display =
            json!({"type": if created { "create" } else { "update" }, "filePath": input.file_path});
        if let Some(checkpoint) = checkpoint {
            display["checkpoint"] = checkpoint;
        }
        Ok(ToolOutput {
            display,
            ..text(message)
        })
    }
}
