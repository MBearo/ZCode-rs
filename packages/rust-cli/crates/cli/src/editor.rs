//! TUI 的外部编辑器（docs/specs/rust-cli/tui-input.md §4）：临时文件经 fs，编辑器进程经 proc 的 shell（继承终端）。调用时 TUI
//! 已经交出终端。
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_fs::{FileMode, ParentDir, TempName, WriteOptions};
use rust_cli_node_store::NodeIds;
use rust_cli_tui::Editor;

const WRITE: WriteOptions = WriteOptions {
    mode: FileMode::Private,
    temp_name: TempName::Hidden,
    parent: ParentDir::Create,
    rename_retry: &[],
};

/// shell 找不到命令时的退出码（POSIX）：缺省编辑器的候选按顺序试。
const NOT_FOUND: i32 = 127;
/// Windows 的 cmd 找不到命令时的退出码。
const CMD_NOT_FOUND: i32 = 9009;

/// shell 的退出码是不是“找不到命令”；`windows` 是入口所在的平台（测试在别的平台上注入）。
///
/// 缺陷修复（F65 的 T15）：以前只认 127，Windows 上 `$EDITOR` 不存在时提示 `<名字> exited with code 9009`。
fn not_found(code: i32, windows: bool) -> bool {
    code == NOT_FOUND || (windows && code == CMD_NOT_FOUND)
}

pub(crate) struct ProcessEditor {
    /// `$VISUAL` / `$EDITOR`（去掉首尾空白，空的当没有）。
    visual: Option<String>,
    editor: Option<String>,
    /// 临时文件所在的本用户私有目录（`<临时目录>/zcode-<uid>`，proc `private::user_dir`）。
    temp_dir: PathBuf,
    /// 临时文件名里的 uuid（F7：经入口的 `NodeIds`，不直接 `Uuid::new_v4`）。
    ids: Arc<NodeIds>,
}

fn value(env: &[(String, String)], key: &str) -> Option<String> {
    rust_cli_base::secret::env_var(env, key)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

impl ProcessEditor {
    /// `temp` 是入口算好的临时目录（`temp_dir::node_tmpdir`）。
    ///
    /// 修复线 F58：以前自己按 `TMPDIR` → `TEMP` → `TMP` → `/tmp` 找目录（与入口的算法不同），文件直接放在公共的临时
    /// 目录里；现在放在本用户 0700 的私有目录（`zcode-prompt-<uuid>.md`）。
    pub(crate) fn from_env(env: &[(String, String)], temp: &Path, ids: Arc<NodeIds>) -> Self {
        Self {
            visual: value(env, "VISUAL"),
            editor: value(env, "EDITOR"),
            temp_dir: rust_cli_proc::private::user_dir(temp),
            ids,
        }
    }

    /// 要试的编辑器：设了变量就只用它；否则 Windows 用 notepad，其他平台依次试
    /// `code`、`vi`、`nano`。
    pub(crate) fn candidates(&self) -> Vec<String> {
        if let Some(configured) = self.visual.clone().or_else(|| self.editor.clone()) {
            return vec![configured];
        }
        if cfg!(windows) {
            return vec!["start /wait notepad".to_owned()];
        }
        ["code", "vi", "nano"].map(str::to_owned).to_vec()
    }
}

/// 一次调用的命令行（GUI 编辑器加上等文件关闭的参数）。
///
/// 缺陷修复 R2-19：路径原来一律写成 `"<路径>"` 拼进 shell。Unix 上是 bash，双引号里 `$(…)`、反引号与
/// `$变量` 照样展开，路径里的 `"` 会提前结束引号；现在用单引号字面量。Windows 上是 cmd.exe，双引号里不展开
/// 这些，路径也不会含 `"`，保持原样。
pub(crate) fn command_line(editor: &str, path: &Path) -> String {
    let editor = match editor {
        "code" => "code -w",
        "subl" => "subl --wait",
        other => other,
    };
    let path = path.to_string_lossy();
    if cfg!(windows) {
        format!("{editor} \"{path}\"")
    } else {
        format!("{editor} {}", rust_cli_proc::shell::single_quote(&path))
    }
}

/// 提示里的编辑器名：命令的第一个词的文件名。
fn display_name(editor: &str) -> String {
    let first = editor.split_whitespace().next().unwrap_or(editor);
    Path::new(first).file_name().map_or_else(
        || first.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// 跑一次编辑器；找不到命令时返回 Ok(false)，让调用方试下一个。
async fn launch(editor: &str, path: &Path) -> Result<bool, String> {
    let status = rust_cli_proc::shell::script(&command_line(editor, path))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .map_err(|error| error.to_string())?;
    match status.code() {
        Some(0) => Ok(true),
        Some(code) if not_found(code, cfg!(windows)) => Ok(false),
        Some(code) => Err(format!("{} exited with code {code}", display_name(editor))),
        None => Err(format!("{} was terminated", display_name(editor))),
    }
}

async fn open_with(candidates: Vec<String>, path: &Path) -> Result<(), String> {
    for editor in &candidates {
        if launch(editor, path).await? {
            return Ok(());
        }
    }
    Err("No external editor was found. Set $EDITOR or $VISUAL.".to_owned())
}

/// 读回的文字以单个换行结尾时去掉它（TUI 还会按自己的粘贴折回，这里只做文件读写）。
async fn edit_text(
    candidates: Vec<String>,
    dir: PathBuf,
    file: PathBuf,
    text: String,
) -> Result<String, String> {
    // 私有目录：没有时以 0700 建，属主不是自己或是符号链接时拒绝。
    rust_cli_proc::private::ensure(&dir)
        .await
        .map_err(|error| error.to_string())?;
    rust_cli_fs::atomic_write(&file, text.as_bytes(), WRITE)
        .await
        .map_err(|error| error.to_string())?;
    let opened = open_with(candidates, &file).await;
    let read = rust_cli_fs::read_optional(&file).await;
    let _ = rust_cli_fs::remove_optional(&file).await;
    opened?;
    let bytes = read.map_err(|error| error.to_string())?.unwrap_or_default();
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

impl Editor for ProcessEditor {
    fn edit(&self, text: String) -> BoxFuture<'static, Result<String, String>> {
        let dir = self.temp_dir.clone();
        let file = dir.join(format!("zcode-prompt-{}.md", self.ids.uuid()));
        edit_text(self.candidates(), dir, file, text).boxed()
    }

    fn open(&self, path: String) -> BoxFuture<'static, Result<(), String>> {
        let candidates = self.candidates();
        async move {
            let file = PathBuf::from(path);
            // 文件不存在时先建空文件。
            let existing = rust_cli_fs::read_optional(&file)
                .await
                .map_err(|error| error.to_string())?;
            if existing.is_none() {
                rust_cli_fs::atomic_write(&file, b"", WRITE)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            open_with(candidates, &file).await
        }
        .boxed()
    }

    fn source(&self) -> Option<(String, String)> {
        match (&self.visual, &self.editor) {
            (Some(visual), _) => Some(("$VISUAL".to_owned(), visual.clone())),
            (None, Some(editor)) => Some(("$EDITOR".to_owned(), editor.clone())),
            (None, None) => None,
        }
    }
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;
