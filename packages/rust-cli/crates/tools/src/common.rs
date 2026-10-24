//! 文件工具共用的零件：定义、路径解析、结果构造、错误映射与 JS 语义的格式化。

use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvError, EnvPath};
use rust_cli_kernel_api::env_files::{FileStat, ListDir, PathStyle, StatFile};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::PermissionGate;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{Tool, ToolError, ToolOutput, ToolUseContext, Validation};
use rust_cli_kernel_api::{env_path, env_shell};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::file_state::ReadFileState;
use crate::texts;

/// 文件工具共享的状态与能力：readFileState，以及判断“这次写入会不会被权限规则直接放行”的判决链。
#[derive(Clone)]
pub struct FileTools {
    pub(crate) state: Arc<ReadFileState>,
    pub(crate) gate: Arc<dyn PermissionGate>,
    /// Read 整读 PDF 前查模型能力用（tools-files.md §1.5，修复线 F24）；没有时按能读处理。
    pub(crate) model: Option<crate::web_fetch::SideModel>,
}

impl FileTools {
    pub fn new(state: Arc<ReadFileState>, gate: Arc<dyn PermissionGate>) -> Self {
        Self {
            state,
            gate,
            model: None,
        }
    }

    /// 模型端口与没有会话 owner 时的回退选择：Read 按会话当前模型的生效配置判断能否整读 PDF。
    #[must_use]
    pub fn with_model(mut self, model: crate::web_fetch::SideModel) -> Self {
        self.model = Some(model);
        self
    }

    pub fn state(&self) -> &Arc<ReadFileState> {
        &self.state
    }

    /// 这次写入是否会被权限规则直接放行（路径层判定为 allow）。
    /// 放行时，较新的模型改未读过、或读后被改过的文件不再报错（见 tools-files.md §1.2）。
    pub(crate) async fn auto_allowed(
        &self,
        tool: &dyn Tool,
        input: &Value,
        context: &ToolUseContext,
    ) -> bool {
        matches!(
            self.gate.decide(tool, input, context).await,
            PermissionDecision::Allow { .. }
        )
    }
}

/// 工具名、描述与 schema（来自 `rust-cli-prompts`；数据文件缺失时为空，由单测兜住）。
pub(crate) struct Definition {
    pub description: String,
    pub schema: Value,
}

pub(crate) fn definition(name: &str) -> Definition {
    rust_cli_prompts::tool(name).map_or_else(
        || Definition {
            description: String::new(),
            schema: json!({"type": "object"}),
        },
        |prompt| Definition {
            description: prompt.description.clone(),
            schema: prompt.input_schema.clone(),
        },
    )
}

pub(crate) fn parse<T: DeserializeOwned>(input: &Value) -> Result<T, ToolError> {
    serde_json::from_value(input.clone()).map_err(|error| ToolError::InvalidInput {
        message: error.to_string(),
    })
}

pub(crate) fn text(text: String) -> ToolOutput {
    ToolOutput {
        content: vec![ToolResultContent::Text { text }],
        is_error: false,
        display: Value::Null,
    }
}

pub(crate) fn failed(message: String) -> ToolError {
    ToolError::Failed { message }
}

pub(crate) fn invalid(message: String) -> Validation {
    Validation::Invalid {
        message,
        error_code: None,
    }
}

/// 执行环境错误给模型看的文本：不带端口内部的前缀。
pub(crate) fn env_failure(error: EnvError) -> ToolError {
    let message = match error {
        EnvError::NotFound { path } => {
            format!("ENOENT: no such file or directory, open '{path}'")
        }
        EnvError::PreconditionFailed { .. } => texts::MODIFIED_SINCE_READ.to_owned(),
        EnvError::PermissionDenied { message }
        | EnvError::Stale { message }
        | EnvError::Io { message } => message,
        EnvError::OperationConflict { operation } => {
            format!("operation {operation} reused with different arguments")
        }
        EnvError::Unsupported { what } => format!("not supported by this environment: {what}"),
    };
    ToolError::Failed { message }
}

/// 会话的当前目录（tools-files.md §3.3）：执行环境里这个会话的 shell cwd——Bash `cd` 改它
/// （越出工作目录时执行环境改回工作区根），EnterWorktree / ExitWorktree / `/cd` 经会话 actor 设置；执行环境答不出时是
/// 工作区根。文件工具的相对路径、显示路径、报错里的工作目录都以它为基准，与权限判定（policy `path_context`）、nested
/// memory 向同一个来源要，不另存一份（readFileState 的键就是这里解析出的绝对路径）。
///
/// 缺陷修复（F19）：以前这里是工作区根，判决链用的是 shell cwd——Bash `cd sub` 之后 `Write x.txt` 判的是
/// `sub/x.txt`、写的是 `x.txt`（可能正是 deny 规则保护的文件）。
pub(crate) async fn cwd(context: &ToolUseContext) -> String {
    env_shell::current_dir(
        context.machine.shell().as_ref(),
        &context.workspace,
        &context.session,
    )
    .await
    .as_str()
    .to_owned()
}

/// 工作区的路径风格：按工作区根的写法推断（kernel-api `env_path`，tools-files.md §3.1）。当前目录与根同一种写法。
pub(crate) fn style(context: &ToolUseContext) -> PathStyle {
    PathStyle::infer(context.workspace.root.as_str())
}

/// 相对当前目录的显示路径（相对会话的当前目录）：在它外面时给规范化后的绝对路径。
pub(crate) fn display_path(style: PathStyle, cwd: &str, path: &str) -> String {
    style
        .relative(cwd, path)
        .unwrap_or_else(|| style.normalize(path))
}

/// 解析用户给的路径（`base` 是相对路径的基准）；`~` 开头时向执行环境要 home。
pub(crate) async fn resolve(
    context: &ToolUseContext,
    base: &str,
    raw: &str,
) -> Result<String, ToolError> {
    let style = style(context);
    let home = if style.needs_home(raw) {
        context
            .machine
            .info()
            .path_facts(&context.workspace)
            .await
            .map_err(env_failure)?
            .home
    } else {
        None
    };
    Ok(style.resolve(base, raw, home.as_ref().map(EnvPath::as_str)))
}

/// 工具路径参数的 home：去掉首尾空白后是 `~` 或 `~/` 开头时才向执行环境要。
async fn input_home(context: &ToolUseContext, raw: &str) -> Result<Option<EnvPath>, ToolError> {
    if !style(context).user_path_needs_home(raw) {
        return Ok(None);
    }
    Ok(context
        .machine
        .info()
        .path_facts(&context.workspace)
        .await
        .map_err(env_failure)?
        .home)
}

/// 工具的路径参数（Glob / Grep 的 `path`）：去首尾空白、NFC、只认 `~/`
/// （kernel-api `env_path`，tools-files.md §3.2）。
///
/// 缺陷修复（F14）：以前用 [`resolve`]，路径原样使用——带空白的路径被当成工作区里的相对路径，分解形式的
/// 文件名与 readFileState 里记的合成形式对不上，Windows 上 `~\x` 也按 home 展开。`cwd` 是相对路径的基准（[`cwd`]，
/// 调用方取一次，同一次调用里的解析与报错用同一个值）。
pub(crate) async fn resolve_input(
    context: &ToolUseContext,
    cwd: &str,
    raw: &str,
) -> Result<String, ToolError> {
    let home = input_home(context, raw).await?;
    Ok(style(context).resolve_user_path(cwd, raw, home.as_ref().map(EnvPath::as_str)))
}

/// Read / Write / Edit / NotebookEdit 的路径参数（解析结果固定）：解析后以空白结尾时为
/// `Ok(None)`，由 [`unstable_input`] 在 validateInput 里报错。
pub(crate) async fn resolve_input_stable(
    context: &ToolUseContext,
    cwd: &str,
    raw: &str,
) -> Result<Option<String>, ToolError> {
    let home = input_home(context, raw).await?;
    Ok(style(context)
        .resolve_user_path_stable(cwd, raw, home.as_ref().map(EnvPath::as_str))
        .ok())
}

/// validateInput 最先做的检查（errorCode 20）：路径解析后以空白结尾时拒绝。
pub(crate) async fn unstable_input(
    context: &ToolUseContext,
    cwd: &str,
    tool: &str,
    field: &str,
    raw: &str,
) -> Option<Validation> {
    matches!(resolve_input_stable(context, cwd, raw).await, Ok(None)).then(|| Validation::Invalid {
        message: texts::unstable_path(tool, field),
        error_code: Some(20),
    })
}

/// `call` 里取路径：validateInput 已经拒绝过不稳定的路径，这里再遇到时给同样的报错。
pub(crate) async fn stable_path(
    context: &ToolUseContext,
    cwd: &str,
    tool: &str,
    field: &str,
    raw: &str,
) -> Result<String, ToolError> {
    resolve_input_stable(context, cwd, raw)
        .await?
        .ok_or_else(|| ToolError::Failed {
            message: texts::unstable_path(tool, field),
        })
}

pub(crate) async fn stat(
    context: &ToolUseContext,
    path: &str,
) -> Result<Option<FileStat>, ToolError> {
    context
        .machine
        .files()
        .stat(
            &context.workspace,
            StatFile {
                path: EnvPath::new(path),
            },
        )
        .await
        .map_err(env_failure)
}

/// 同目录下同名、不同扩展名的文件名。
async fn similar_file(context: &ToolUseContext, path: &str) -> Option<String> {
    let style = style(context);
    let dir = style.dirname(path);
    let name = style.file_name(path);
    let stem = &name[..name.len() - env_path::extension(&name).len()];
    let entries = context
        .machine
        .files()
        .list_dir(
            &context.workspace,
            ListDir {
                path: EnvPath::new(dir.clone()),
            },
        )
        .await
        .ok()?;
    entries
        .into_iter()
        .map(|entry| entry.name)
        .find(|candidate| {
            let extension = &candidate[candidate.len() - env_path::extension(candidate).len()..];
            let plausible = extension.is_empty()
                || (extension.chars().count() <= 33
                    && extension
                        .chars()
                        .skip(1)
                        .all(|c| c.is_alphanumeric() || "_+~#-".contains(c)));
            plausible
                && &candidate[..candidate.len() - extension.len()] == stem
                && style.join(&dir, &[candidate]) != path
        })
}

/// 路径在 cwd 的父目录下、却不在 cwd 里时，试试 cwd 下的同名路径。
async fn under_cwd(context: &ToolUseContext, root: &str, path: &str) -> Option<String> {
    let style = style(context);
    let parent = style.dirname(root);
    let separator = style.separator();
    let prefix = if parent.ends_with(separator) {
        parent.clone()
    } else {
        format!("{parent}{separator}")
    };
    if !path.starts_with(&prefix) || path.starts_with(&format!("{root}{separator}")) || path == root
    {
        return None;
    }
    let candidate = style.join(root, &[&path[prefix.len()..]]);
    stat(context, &candidate)
        .await
        .ok()
        .flatten()
        .map(|_| candidate)
}

/// 文件不存在时的报错：工作目录说明加上可能的路径建议。
pub(crate) async fn missing_file(context: &ToolUseContext, cwd: &str, path: &str) -> String {
    let mut message = format!("File does not exist. {} {cwd}.", texts::CWD_NOTE);
    if let Some(candidate) = under_cwd(context, cwd, path).await {
        message.push_str(&format!(" Did you mean {candidate}?"));
    } else if let Some(name) = similar_file(context, path).await {
        message.push_str(&format!(" Did you mean {name}?"));
    }
    escape_control(&message)
}

/// `cwd` 是这次调用取到的当前目录（与报错里写的是同一个值）。
pub(crate) async fn suggestion_under_cwd(
    context: &ToolUseContext,
    cwd: &str,
    path: &str,
) -> String {
    under_cwd(context, cwd, path)
        .await
        .map(|candidate| format!(" Did you mean {candidate}?"))
        .unwrap_or_default()
}

/// 控制字符换成 `&#N;`。
pub(crate) fn escape_control(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        let code = u32::from(c);
        if code <= 0x1f || (0x7f..=0x9f).contains(&code) || code == 0x2028 || code == 0x2029 {
            escaped.push_str(&format!("&#{code};"));
        } else {
            escaped.push(c);
        }
    }
    escaped
}

/// JS 字符串长度（UTF-16 码元数）：唯一定义在 base（F58：以前这里、kernel-api 与 policy 各写一份）。
pub(crate) use rust_cli_base::js::string::utf16_len;

/// `formatBytes`（唯一实现在 kernel-api 的 `persisted_output`，与 `<persisted-output>` 共用）。
pub(crate) use rust_cli_kernel_api::persisted_output::format_bytes;

#[cfg(test)]
#[path = "common_tests.rs"]
mod tests;
