//! 内嵌的搜索工具按 argv0 分派（docs/specs/rust-cli/embedded-search.md §3，工作线 F53）。
//!
//! 入口的第一件事：argv[0] 的文件名，或者环境变量 `ARGV0`，是内置工具的名字时，直接跑那个工具，以它的退出码结束
//! 进程；不读配置、不打日志、不建 runtime。bash 的 `(exec -a rg …)` 与 zsh 的 `ARGV0=rg …` 改的是 argv[0]；msys /
//! cygwin 的 bash 下 `ARGV0=rg …` 只把变量放进环境，所以两者都认。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// 内置的工具。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    /// ripgrep 15.2.0（`rust-cli-vendor-rg`，embedded-search.md §5）。
    Rg,
    /// bfs 4.1.4 + Oniguruma 6.9.10（`rust-cli-vendor-bfs`，§6）。Windows 没有（§6.4）。
    #[cfg(unix)]
    Bfs,
    /// ugrep 7.8.4（`rust-cli-vendor-ugrep`，§7）。
    Ugrep,
}

/// 名字对应的工具：完全相同才算；Windows 上去掉 `.exe`、不分大小写。
fn named(name: &OsStr) -> Option<Tool> {
    let name = name.to_str()?;
    let name = if cfg!(windows) {
        let lower = name.to_ascii_lowercase();
        lower
            .strip_suffix(".exe")
            .map_or(lower.clone(), str::to_owned)
    } else {
        name.to_owned()
    };
    match name.as_str() {
        "rg" => Some(Tool::Rg),
        #[cfg(unix)]
        "bfs" => Some(Tool::Bfs),
        "ugrep" => Some(Tool::Ugrep),
        _ => None,
    }
}

/// argv[0] 的文件名优先，其次环境变量 `ARGV0` 的值。
pub(crate) fn choose(argv0: Option<&OsStr>, env: Option<&OsStr>) -> Option<Tool> {
    argv0
        .and_then(|argv0| Path::new(argv0).file_name())
        .and_then(named)
        .or_else(|| env.and_then(named))
}

/// 入口最先调用：是内置工具时跑它并返回退出码，否则返回 None（继续当 zcode）。
#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一读取进程参数与环境的位置（README §4）；分派发生在入口的任何初始化之前"
)]
pub(crate) fn dispatch() -> Option<ExitCode> {
    let argv0 = std::env::args_os().next();
    let env = std::env::var_os("ARGV0");
    let tool = choose(argv0.as_deref(), env.as_deref())?;
    if env.is_some() {
        clear_argv0();
    }
    Some(match tool {
        // rg 自己读 `std::env::args_os()`（跳过 argv[0]）。
        Tool::Rg => rust_cli_vendor_rg::main(),
        #[cfg(unix)]
        Tool::Bfs => exit_code(rust_cli_vendor_bfs::main(std::env::args_os().skip(1))),
        Tool::Ugrep => exit_code(rust_cli_vendor_ugrep::main(std::env::args_os().skip(1))),
    })
}

/// 本进程二进制的绝对路径（embedded-search.md §4）：Bash 的 prelude 用它（`ZCODE_EXECPATH` 与函数里的回退路径）。
/// 符号链接解析成真实文件（npm 的启动器、`~/.local/bin` 的链接）；取不到时 None（Bash 里用系统的命令）。
#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一读取进程事实（current_exe）与判断本机文件的位置（README §4）"
)]
pub(crate) fn executable() -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?;
    let real = std::fs::canonicalize(&path).unwrap_or(path);
    Some(without_verbatim_prefix(real))
}

/// 带盘符的 verbatim 路径最短的写法 `\\?\C:\` 的长度。
const MIN_VERBATIM_DRIVE_LEN: usize = 7;

/// 去掉 Windows 的 `\\?\` 前缀：`\\?\UNC\server\x` → `\\server\x`；
/// `\\?\C:\x` → `C:\x`（长度至少 7、第 6 个字符是 `:`）；其余原样。
///
/// 缺陷修复（工作线 F57）：Windows 上 `canonicalize` 给出 `\\?\C:\…`，prelude 按 Git Bash 写法换成 `//?/C:/…`，Git Bash
/// 不认，`[[ -x … ]]` 不成立，内嵌的 rg / ugrep 退回系统命令。只改写法，指向同一个文件。
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if text.starts_with(r"\\?\")
        && text.len() >= MIN_VERBATIM_DRIVE_LEN
        && text.as_bytes()[5] == b':'
    {
        return PathBuf::from(&text[4..]);
    }
    path
}

/// C 工具的返回值 → 进程退出码（取低 8 位，同 C 的 `exit`）。
fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(code.to_le_bytes()[0])
}

/// 运行工具前从环境删掉 `ARGV0`：工具再起的子进程（rg 的 `--pre`、bfs 的 `-exec`）不继承它，再起 zcode 时也不会被
/// 当成工具。
#[expect(
    unsafe_code,
    reason = "edition 2024 的 remove_var 是 unsafe：只能在没有别的线程读写环境时调用"
)]
#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一碰进程环境的位置（README §4）；这一步在入口的任何初始化之前，快照还没捕获"
)]
fn clear_argv0() {
    // SAFETY: 这是 main 的第一步，进程里还没有别的线程，没有并发读写环境。
    unsafe { std::env::remove_var("ARGV0") };
}

#[cfg(test)]
#[path = "embedded_tests.rs"]
mod tests;
