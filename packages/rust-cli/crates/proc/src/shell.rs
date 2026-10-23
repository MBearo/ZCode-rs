// 复制自 rust/crates/proc/src/shell.rs（feat/rs 79b6691c55）。
//! 跑一段 shell 文本：Unix `/bin/bash -c <text>`；Windows `cmd.exe /d /s /c "<text>"`。

/// 创建跑 `text` 的命令。
///
/// Windows 上整段参数经 `raw_arg` 原样传给 cmd.exe，不经过 CRT 引号转义（见 [`cmd_exe_arguments`]）。
pub fn script(text: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut command = crate::command("cmd.exe");
        command.raw_arg(cmd_exe_arguments(text));
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = crate::command("/bin/bash");
        command.args(["-c", text]);
        command
    }
}

/// bash 的单引号字面量：`'` 写成 `'\''`。Bash 工具的脚本（任何平台都是 bash）与 Unix 上拼进 [`script`]
/// 的路径都用它。
pub fn single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// cmd.exe 跑 `text` 时的完整参数：`/d /s /c "<text>"`，交给 `raw_arg` 使用。
///
/// 修复 B2：Bash 工具原来用 `args(["/D", "/S", "/C", text])`，Rust 按 CRT 规则给参数加引号，
/// 把命令中的 `"` 改写成 `\"`；cmd.exe 不认识这种转义，带引号的路径和参数都会出错。
/// `/s` 让 cmd.exe 只去掉最外层的一对引号，其余字符原样执行，所以这里只需要包一对引号。
/// `/d` 跳过注册表中的 AutoRun。hooks 用同一个函数构造 cmd.exe 的参数。
pub fn cmd_exe_arguments(text: &str) -> String {
    format!("/d /s /c \"{text}\"")
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
