//! Grep（tools-files.md §1）。
//!
//! 搜索在执行环境里按 rg 语义完成；这里拼参数、把匹配块排成 rg 的行格式、分页并生成结果文本。
//! 默认配置下不暴露这个工具（tools-files.md §1.1）。

use async_trait::async_trait;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::{FileKind, GrepMode, GrepOutcome, GrepSearch};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::common::{
    Definition, cwd, definition, display_path, env_failure, escape_control, failed, invalid, parse,
    resolve_input, stat, style, suggestion_under_cwd, text,
};
use crate::grep_output::{content_lines, count, globs, locale_order, paginate, pagination, plural};
use crate::texts;

pub const NAME: &str = "Grep";

#[derive(Deserialize)]
struct Input {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    output_mode: Option<String>,
    #[serde(rename = "-B")]
    before: Option<f64>,
    #[serde(rename = "-A")]
    after: Option<f64>,
    #[serde(rename = "-C")]
    around: Option<f64>,
    context: Option<f64>,
    #[serde(rename = "-n")]
    line_numbers: Option<bool>,
    #[serde(rename = "-i")]
    ignore_case: Option<bool>,
    #[serde(rename = "-o")]
    only_matching: Option<bool>,
    #[serde(rename = "type")]
    file_type: Option<String>,
    head_limit: Option<f64>,
    offset: Option<f64>,
    multiline: Option<bool>,
}

pub struct GrepTool {
    definition: Definition,
}

impl GrepTool {
    pub fn new() -> Self {
        Self {
            definition: definition(NAME),
        }
    }
}

impl Default for GrepTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GrepTool {
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

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(20_000)
    }

    async fn validate_input(&self, raw: &Value, _context: &ToolUseContext) -> Validation {
        for name in ["head_limit", "offset"] {
            if let Some(value) = raw.get(name).and_then(Value::as_f64)
                && (value.fract() != 0.0 || value < 0.0)
            {
                let hint = if name == "head_limit" {
                    " Pass 0 for unlimited."
                } else {
                    ""
                };
                let shown = rust_cli_base::js::json::js_double(value);
                return invalid(format!(
                    "{name} must be a whole number of 0 or more, got {shown}.{hint}"
                ));
            }
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
        // 会话的当前目录（tools-files.md §3.3）：相对路径、缺省的搜索目录与结果的显示都以它为基准。
        let cwd = cwd(&context).await;
        let root = match &input.path {
            Some(path) => resolve_input(&context, &cwd, path).await?,
            None => cwd.clone(),
        };
        let target = stat(&context, &root).await?;
        if let (Some(path), None) = (&input.path, &target) {
            let hint = suggestion_under_cwd(&context, &cwd, &root).await;
            return Err(failed(escape_control(&format!(
                "{}{hint}",
                texts::path_missing(path, &cwd)
            ))));
        }
        let is_directory = target.is_none_or(|found| found.kind == FileKind::Directory);
        let mode_name = input.output_mode.as_deref().unwrap_or("files_with_matches");
        let mode = match mode_name {
            "content" => match (input.context.or(input.around), input.before, input.after) {
                (Some(around), _, _) => GrepMode::Content {
                    before: count(Some(around)),
                    after: count(Some(around)),
                },
                (None, before, after) => GrepMode::Content {
                    before: count(before),
                    after: count(after),
                },
            },
            "count" => GrepMode::Count,
            _ => GrepMode::FilesWithMatches,
        };
        let request = GrepSearch {
            root: EnvPath::new(root),
            pattern: input.pattern.clone(),
            case_insensitive: input.ignore_case.unwrap_or(false),
            multiline: input.multiline.unwrap_or(false),
            globs: globs(input.glob.as_deref()),
            file_type: input.file_type.clone().filter(|name| !name.is_empty()),
            hidden: true,
            mode,
        };
        // R5-13：遍历时被取消（esc）立即结束；丢下的遍历由执行环境停掉（tools-files.md §3）。原来只等遍历，
        // 很大的目录树要跑完才结束这个回合。
        let outcome = tokio::select! {
            outcome = context.machine.search().grep(&context.workspace, request) => outcome.map_err(env_failure)?,
            () = context.cancel.cancelled() => return Err(ToolError::Cancelled),
        };
        let head_limit = input.head_limit.map(|value| value as usize);
        let offset = input.offset.map_or(0, |value| value as usize);
        let style = style(&context);
        let relative = |path: &str| display_path(style, &cwd, path);
        let body = match outcome {
            GrepOutcome::Rejected { message } => {
                return Err(failed(format!("{}rg: {message}", texts::RG_REJECTED)));
            }
            GrepOutcome::Content { files } => {
                let breaks = [input.context, input.around, input.before, input.after]
                    .iter()
                    .any(|value| value.is_some_and(|value| value > 0.0));
                let all = content_lines(&files, breaks, input.only_matching.unwrap_or(false));
                let (items, limit) = paginate(&all, head_limit, offset);
                let numbers = input.line_numbers.unwrap_or(true);
                let lines: Vec<String> = items
                    .iter()
                    .map(|item| match item.split_once('\0') {
                        None if numbers => item.clone(),
                        None => item
                            .trim_start_matches(|c: char| c.is_ascii_digit())
                            .trim_start_matches([':', '-'])
                            .to_owned(),
                        Some((path, rest)) => {
                            let digits = rest.chars().take_while(char::is_ascii_digit).count();
                            let separator = rest[digits..]
                                .chars()
                                .next()
                                .filter(|c| digits > 0 && (*c == ':' || *c == '-'));
                            let shown = match separator {
                                Some(_) if !numbers => &rest[digits + 1..],
                                _ => rest,
                            };
                            if is_directory {
                                format!("{}{}{shown}", relative(path), separator.unwrap_or(':'))
                            } else {
                                shown.to_owned()
                            }
                        }
                    })
                    .collect();
                let note = pagination(limit, offset);
                let shown = if lines.is_empty() {
                    if offset > 0 && !all.is_empty() {
                        "No entries at this offset".to_owned()
                    } else {
                        "No matches found".to_owned()
                    }
                } else {
                    lines.join("\n")
                };
                if note.is_empty() {
                    shown
                } else {
                    format!("{shown}\n\n[Showing results with pagination = {note}]")
                }
            }
            GrepOutcome::Counts { counts } => {
                let matches: u64 = counts.iter().map(|item| item.count).sum();
                let (items, limit) = paginate(&counts, head_limit, offset);
                let lines: Vec<String> = items
                    .iter()
                    .map(|item| format!("{}:{}", relative(item.path.as_str()), item.count))
                    .collect();
                let note = pagination(limit, offset);
                let shown = if lines.is_empty() {
                    if matches > 0 {
                        "No entries at this offset".to_owned()
                    } else {
                        "No matches found".to_owned()
                    }
                } else {
                    lines.join("\n")
                };
                let occurrences = if matches == 1 {
                    "occurrence"
                } else {
                    "occurrences"
                };
                let files = plural(counts.len(), "file");
                let tail = if note.is_empty() {
                    String::new()
                } else {
                    format!(" with pagination = {note}")
                };
                format!(
                    "{shown}\n\nFound {matches} total {occurrences} across {} {files}.{tail}",
                    counts.len()
                )
            }
            GrepOutcome::Files { mut files } => {
                files.sort_by(|left, right| {
                    right
                        .mtime_ms
                        .cmp(&left.mtime_ms)
                        .then_with(|| locale_order(left.path.as_str(), right.path.as_str()))
                });
                let (items, limit) = paginate(&files, head_limit, offset);
                let note = pagination(limit, offset);
                if items.is_empty() {
                    if offset > 0 && !files.is_empty() {
                        format!(
                            "No entries at this offset. [Showing results with pagination = {note}]"
                        )
                    } else {
                        "No files found".to_owned()
                    }
                } else {
                    let names: Vec<String> = items
                        .iter()
                        .map(|item| relative(item.path.as_str()))
                        .collect();
                    let note = if note.is_empty() {
                        String::new()
                    } else {
                        format!(" {note}")
                    };
                    format!(
                        "Found {} {}{note}\n{}",
                        names.len(),
                        plural(names.len(), "file"),
                        names.join("\n")
                    )
                }
            }
        };
        Ok(ToolOutput {
            display: json!({"mode": mode_name}),
            ..text(body)
        })
    }
}

#[cfg(test)]
#[path = "search_cancel_tests.rs"]
mod cancel_tests;
