//! 要读文件、看目录的参数（docs/specs/rust-cli/cli-args.md §4、§6、§7）：入口读完环境之后、建内核之前处理，
//! 文件与目录都经执行环境端口（与 `--mcp-config` 相同）。报错的文字固定，写 stderr、退出码 1。

use rust_cli_kernel_api::env::{EnvError, EnvPath, Environment, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_files::{FileKind, PathStyle, StatFile};

use serde_json::{Map, Value};

use crate::args::PromptFlags;

/// `--settings` 文件的上限。
const SETTINGS_LIMIT: usize = 2 * 1024 * 1024;

/// 以 `Error: …` 退出的参数错误：原样写 stderr，退出码 1。
#[derive(Debug)]
pub(crate) struct Fatal(pub(crate) String);

impl std::fmt::Display for Fatal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Fatal {}

/// 读文件要的上下文：执行环境、工作区、cwd 与 home（解析相对路径与 `~`）。
pub(crate) struct Place<'a> {
    pub(crate) env: &'a dyn Environment,
    pub(crate) workspace: &'a WorkspaceRef,
    pub(crate) cwd: &'a str,
    pub(crate) home: Option<&'a str>,
    pub(crate) style: PathStyle,
}

/// 哪一个 system prompt 参数（报错文字不同）。
#[derive(Debug, Clone, Copy)]
pub(crate) enum PromptKind {
    System,
    Append,
}

impl PromptKind {
    fn not_found(self) -> &'static str {
        match self {
            Self::System => "Error: System prompt file not found",
            Self::Append => "Error: Append system prompt file not found",
        }
    }

    fn unreadable(self) -> &'static str {
        match self {
            Self::System => "Error reading system prompt file",
            Self::Append => "Error reading append system prompt file",
        }
    }
}

/// 文件内容（去掉一个结尾换行）与命令行的文字用空行连接；有一边是空的就取另一边（原样）。
pub(crate) fn join_prompt(file: Option<String>, inline: Option<String>) -> Option<String> {
    let inline = inline.filter(|text| !text.is_empty());
    let Some(file) = file.filter(|text| !text.is_empty()) else {
        return inline;
    };
    let Some(inline) = inline else {
        return Some(file);
    };
    let file = file
        .strip_suffix("\r\n")
        .or_else(|| file.strip_suffix('\n'))
        .unwrap_or(&file);
    Some(format!("{file}\n\n{inline}"))
}

/// `--system-prompt[-file]` / `--append-system-prompt[-file]` 的最终文字；文件按 cwd 解析（`path.resolve`，不展开
/// `~`），读不到时报错退出。
pub(crate) async fn prompt_text(
    place: &Place<'_>,
    flags: &PromptFlags,
    kind: PromptKind,
) -> Result<Option<String>, Fatal> {
    let Some(file) = &flags.file else {
        return Ok(flags.inline.clone());
    };
    let path = place.style.resolve(place.cwd, file, None);
    let request = ReadFile {
        path: EnvPath::new(&path),
    };
    let content = match place.env.read_file(place.workspace, request).await {
        Ok(content) => String::from_utf8_lossy(&content.bytes).into_owned(),
        Err(EnvError::NotFound { .. }) => {
            return Err(Fatal(format!("{}: {path}", kind.not_found())));
        }
        Err(error) => return Err(Fatal(format!("{}: {error}", kind.unreadable()))),
    };
    Ok(join_prompt(Some(content), flags.inline.clone()))
}

/// `--settings`（cli-args.md §8）：去掉首尾空白后以 `{` 开头、
/// `}` 结尾的当 JSON 文本，否则是文件（按 cwd 解析）。文件里不是 JSON 对象时 `-p` 里静默忽略：这里记警告、
/// 当作空配置。
pub(crate) async fn settings(place: &Place<'_>, value: &str) -> Result<Map<String, Value>, Fatal> {
    let trimmed = value.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return match serde_json::from_str::<Value>(trimmed) {
            Ok(Value::Object(object)) => Ok(object),
            _ => Err(Fatal(
                "Error: Invalid JSON provided to --settings".to_owned(),
            )),
        };
    }
    let path = place.style.resolve(place.cwd, value, None);
    let request = ReadFile {
        path: EnvPath::new(&path),
    };
    let bytes = match place.env.read_file(place.workspace, request).await {
        Ok(content) => content.bytes,
        Err(EnvError::NotFound { .. }) => {
            return Err(Fatal(format!("Error: Settings file not found: {path}")));
        }
        Err(error) => {
            return Err(Fatal(format!(
                "Error: Cannot use settings file ({error}): {path}"
            )));
        }
    };
    if bytes.len() > SETTINGS_LIMIT {
        return Err(Fatal(format!(
            "Error: Settings file exceeds the {}MiB limit: {path}",
            SETTINGS_LIMIT / 1024 / 1024
        )));
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(object)) => Ok(object),
        _ => {
            tracing::warn!(
                path,
                "the --settings file is not a JSON object and is ignored"
            );
            Ok(Map::new())
        }
    }
}

/// `--add-dir` 的结果：加入的目录与要给用户的警告。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AddedDirs {
    pub(crate) dirs: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

/// 逐个检查 `--add-dir`（cli-args.md §6）：`~` 与相对路径按 cwd 解析；不存在的、已在工作目录里的静默丢掉；不是目录
/// 的给警告（text 输出时写 stderr）。
pub(crate) async fn add_dirs(place: &Place<'_>, raw: &[String]) -> AddedDirs {
    let mut added = AddedDirs::default();
    for input in raw {
        if input.is_empty() {
            added
                .warnings
                .push("Please provide a directory path.".to_owned());
            continue;
        }
        let path = place.style.resolve(place.cwd, input, place.home);
        let request = StatFile {
            path: EnvPath::new(&path),
        };
        let stat = place.env.stat(place.workspace, request).await;
        match stat {
            Ok(Some(stat)) if stat.kind == FileKind::Directory => {}
            Ok(Some(_)) => {
                added.warnings.push(format!(
                    "{input} is not a directory. Did you mean to add the parent directory {}?",
                    place.style.dirname(&path)
                ));
                continue;
            }
            // 不存在（或读不到）：静默丢掉（`pathNotFound` 不进警告）。
            Ok(None) | Err(_) => continue,
        }
        let within = std::iter::once(place.cwd)
            .chain(added.dirs.iter().map(String::as_str))
            .any(|root| place.style.is_within(&path, root));
        if !within {
            added.dirs.push(path);
        }
    }
    added
}

/// bypass 模式在 root 下的限制（cli-args.md §5.2）：初始模式
/// 是 bypassPermissions（来自 `--permission-mode`、持久模式或配置）、进程是 root（Unix 的 uid 0）、`IS_SANDBOX` 不是
/// `1`、`ZCODE_BUBBLEWRAP`（config.md §2.5）不为真时拒绝启动。
/// 工作线 F63：本工程没有 `--dangerously-skip-permissions`，文字写模式名（`bypassPermissions` 与本工程的 `yolo`），不写参数名。
pub(crate) fn refuse_root_bypass(
    bypass: bool,
    uid: Option<u32>,
    env: &[(String, String)],
) -> Result<(), Fatal> {
    let var = |name: &str| rust_cli_base::secret::env_var(env, name);
    let sandbox = crate::options::is_sandbox_one(env);
    let bubblewrap = var("ZCODE_BUBBLEWRAP").is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    });
    if bypass && uid == Some(0) && !sandbox && !bubblewrap {
        return Err(Fatal(
            "Permission mode bypassPermissions (yolo) cannot be used with root/sudo privileges for security reasons"
                .to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "flags_tests.rs"]
mod tests;
