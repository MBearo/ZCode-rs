//! Glob（tools-files.md §1）。
//!
//! rg `--files --glob <pattern> --sort=modified --no-ignore --hidden`：按修改时间升序，最多 100 个，
//! 路径相对工作目录。默认配置下不暴露这个工具（tools-files.md §1.1）。

use async_trait::async_trait;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::{FileKind, GlobFiles, PathStyle};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::common::{
    Definition, cwd, definition, display_path, env_failure, escape_control, failed, parse, resolve,
    resolve_input, stat, style, suggestion_under_cwd, text,
};
use crate::texts;

pub const NAME: &str = "Glob";
const LIMIT: usize = 100;

#[derive(Deserialize)]
struct Input {
    pattern: String,
    path: Option<String>,
}

pub struct GlobTool {
    definition: Definition,
}

impl GlobTool {
    pub fn new() -> Self {
        Self {
            definition: definition(NAME),
        }
    }
}

impl Default for GlobTool {
    fn default() -> Self {
        Self::new()
    }
}

/// 绝对路径的模式拆成搜索目录与相对模式（tools-files.md §3.2）。
///
/// 缺陷修复（工作线 F57）：以前 POSIX 上 `\` 也当分隔符（`\` 是 glob 的转义，`/tmp/a\[x].rs` 被拆成 `/tmp/a` 与
/// `[x].rs`）；Windows 上 `C:\*.rs` 的搜索目录是 `C:`（C 盘的当前目录），`C:a\*.rs`（盘符相对）被当成绝对路径。现在
/// 按工作区的风格判断绝对路径与分隔符，`C:` 补成 `C:\`；没有通配符时是 `dirname` / `basename`。
fn split_absolute(pattern: &str, root: &EnvPath) -> Option<(String, String)> {
    let style = PathStyle::infer(root.as_str());
    if !style.is_absolute(pattern) {
        return None;
    }
    let separator = |c: char| style.is_separator(c);
    const GLOB: [char; 4] = ['*', '?', '[', '{'];
    let text = if pattern.contains(GLOB) {
        pattern
    } else {
        // 没有通配符：Node `dirname` / `basename`，末尾的分隔符不算。
        Some(pattern.trim_end_matches(separator))
            .filter(|trimmed| !trimmed.is_empty())
            .unwrap_or(pattern)
    };
    let head = text.find(GLOB).map_or(text, |at| &text[..at]);
    let cut = head.rfind(separator)?;
    let base = match &text[..cut] {
        "" => "/".to_owned(),
        // `C:` 是 C 盘的当前目录，补成 `C:\`。
        drive
            if style == PathStyle::Windows
                && drive.len() == 2
                && drive.as_bytes()[0].is_ascii_alphabetic()
                && drive.as_bytes()[1] == b':' =>
        {
            format!("{drive}\\")
        }
        base => base.to_owned(),
    };
    Some((base, text[cut + 1..].to_owned()))
}

/// 模式开头重复了搜索目录的最后几段时去掉它们，例如在 `src` 里找 `src/*.ts`。
/// 返回去掉后的模式（以 `/` 开头，锚定在搜索目录）与被去掉的那几段。
fn strip_repeated(pattern: &str, dir: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = pattern.split('/').collect();
    let literal = parts
        .iter()
        .take(parts.len().saturating_sub(1))
        .take_while(|part| !part.is_empty() && !part.contains(['*', '?', '[', ']', '{', '}', '!']))
        .count();
    let tail: Vec<&str> = dir
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();
    (1..=literal).rev().find_map(|count| {
        (tail.len() >= count && parts[..count] == tail[tail.len() - count..]).then(|| {
            (
                format!("/{}", parts[count..].join("/")),
                parts[..count].join("/"),
            )
        })
    })
}

/// 截断说明；总数总是准确的。
fn truncation_note(shown: usize, total: usize) -> String {
    format!(
        "(Showing {shown} of {total} matching files; {} more are not listed. Narrow the pattern or path to see the rest.)",
        total - shown
    )
}

#[async_trait]
impl Tool for GlobTool {
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
        Some(100_000)
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
        let (root, mut pattern) = match split_absolute(&input.pattern, &context.workspace.root) {
            // 绝对模式拆出的搜索目录不经路径参数的解析，`path` 经过（tools-files.md §3.2）。
            Some((base, relative)) => (resolve(&context, &cwd, &base).await?, relative),
            None => match &input.path {
                Some(path) => (
                    resolve_input(&context, &cwd, path).await?,
                    input.pattern.clone(),
                ),
                None => (cwd.clone(), input.pattern.clone()),
            },
        };
        if let Some(path) = &input.path {
            match stat(&context, &root).await? {
                None => {
                    let hint = suggestion_under_cwd(&context, &cwd, &root).await;
                    let message = format!("{}{hint}", texts::directory_missing(path, &cwd));
                    return Err(failed(escape_control(&message)));
                }
                Some(found) if found.kind != FileKind::Directory => {
                    return Err(failed(escape_control(&texts::not_a_directory(path))));
                }
                Some(_) => {}
            }
        }
        // 搜索目录下真有这几层子目录时保留原模式。
        if let Some((stripped, removed)) = strip_repeated(&pattern, &root) {
            let implied = style(&context).join(&root, &[&removed]);
            if !matches!(stat(&context, &implied).await, Ok(Some(found)) if found.kind == FileKind::Directory)
            {
                pattern = stripped;
            }
        }
        let request = GlobFiles {
            root: EnvPath::new(root),
            pattern,
            limit: LIMIT,
            hidden: true,
            respect_ignore: false,
        };
        // R5-13：同 Grep，遍历时被取消立即结束，丢下的遍历由执行环境停掉。
        let found = tokio::select! {
            found = context.machine.search().glob(&context.workspace, request) => found.map_err(env_failure)?,
            () = context.cancel.cancelled() => return Err(ToolError::Cancelled),
        };
        let names: Vec<String> = found
            .files
            .iter()
            .map(|file| display_path(style(&context), &cwd, file.as_str()))
            .collect();
        let display = json!({"filenames": names, "numFiles": names.len(), "truncated": found.total > names.len(), "totalMatches": found.total});
        if names.is_empty() {
            return Ok(ToolOutput {
                display,
                ..text("No files found".to_owned())
            });
        }
        let mut lines = names.clone();
        if found.total > names.len() {
            lines.push(truncation_note(names.len(), found.total));
        }
        Ok(ToolOutput {
            display,
            ..text(lines.join("\n"))
        })
    }
}

#[cfg(test)]
#[path = "glob_tests.rs"]
mod tests;
