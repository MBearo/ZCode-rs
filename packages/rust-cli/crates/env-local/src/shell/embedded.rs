//! 内嵌搜索工具的 prelude（docs/specs/rust-cli/embedded-search.md §8，工作线 F53）：每条 Bash 工具命令前定义
//! `find` / `grep` / `rg` 三个 shell 函数，转给本进程的二进制（按 argv0 分派到内嵌的 bfs / ugrep / rg）。
//!
//! 函数的原文见 embedded-search.md §8.2：本进程路径的变量是 `ZCODE_EXECPATH`，局部变量名用 _zc_ 前缀，回退路径
//! 是本进程的路径。本工程不加载会话开始时生成的 shell
//! 快照（shell-permissions.md D1），每条命令前直接加这一段。

use std::path::PathBuf;

// Windows 路径的 Git Bash 写法：F57 起只在 kernel-api 一处（以前这里有一份 `git_bash_path`）。
use rust_cli_kernel_api::env_path::git_bash_spelling;

/// 内嵌的搜索工具（入口给出，cli 的 `shell.rs`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedSearch {
    /// 本进程的二进制（绝对路径）：每条命令的 `ZCODE_EXECPATH`，也是函数里的回退路径。
    pub executable: PathBuf,
    /// 启动参数没有点名 Glob / Grep 时为真，定义 `find` / `grep`；为假时只有 `rg`。
    pub find_and_grep: bool,
}

/// 每条命令的环境里本进程二进制的变量名（本工程的 `ZCODE_*` 名字，F42）。
pub(crate) const EXECPATH: &str = rust_cli_base::names::env::ZCODE_EXECPATH;

/// grep 的缺省参数（`-G --ignore-files --hidden -I` 加版本控制目录）。
const GREP_DEFAULTS: [&str; 10] = [
    "-G",
    "--ignore-files",
    "--hidden",
    "-I",
    "--exclude-dir=.git",
    "--exclude-dir=.svn",
    "--exclude-dir=.hg",
    "--exclude-dir=.bzr",
    "--exclude-dir=.jj",
    "--exclude-dir=.sl",
];

/// 交给系统 grep 的写法：ugrep 会执行命令、读写配置的选项，以及与 GNU 含义不同的 `-z` / `-Z` / `--null`。
const GREP_FALLBACK: [&str; 12] = [
    "-*-filter*",
    "-*-pager*",
    "-*-view*",
    "-*-format-open*",
    "-*-config*",
    "---*",
    "-@*",
    "-*-save-config*",
    "-[Zz]*",
    "-[!-]*[Zz]*",
    "--null",
    "--null-data",
];

/// find 的缺省参数。
const FIND_DEFAULTS: [&str; 4] = ["-S", "dfs", "-regextype", "findutils-default"];

/// shell 引用：空串写成 `''`；只含 `[A-Za-z0-9_./:=@+,-]` 的原样；其余用单引号，`'` 写成 `'"'"'`。
pub(crate) fn quote(text: &str) -> String {
    if text.is_empty() {
        return "''".to_owned();
    }
    let plain = text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_./:=@+,-".contains(&b));
    if plain {
        return text.to_owned();
    }
    format!("'{}'", text.replace('\'', r#"'"'"'"#))
}

/// 一个函数的原文（名字、argv0、缺省参数、回退）。
fn function(
    name: &str,
    argv0: &str,
    defaults: &[&str],
    fallback: &[&str],
    fallback_path: &str,
) -> String {
    let args = if defaults.is_empty() {
        r#"${1+"$@"}"#.to_owned()
    } else {
        format!(r#"{} ${{1+"$@"}}"#, defaults.join(" "))
    };
    let mut lines = vec![format!("function {name} {{")];
    if !fallback.is_empty() {
        lines.push("  local _zc_a".to_owned());
        lines.push(r#"  for _zc_a in ${1+"$@"}; do"#.to_owned());
        lines.push(format!(
            r#"    case "$_zc_a" in {}) command {name} ${{1+"$@"}}; return ;; esac"#,
            fallback.join("|")
        ));
        lines.push("  done".to_owned());
    }
    lines.push(format!(r#"  local _zc_bin="${{{EXECPATH}:-}}""#));
    lines.push(format!(
        "  [[ -x $_zc_bin ]] || _zc_bin={}",
        quote(fallback_path)
    ));
    lines.push(format!(
        r#"  if [[ ! -x $_zc_bin ]]; then command {name} ${{1+"$@"}}; return; fi"#
    ));
    lines.push("  if [[ -n ${ZSH_VERSION:-} ]]; then".to_owned());
    lines.push(format!(r#"    ARGV0={argv0} "$_zc_bin" {args}"#));
    lines.push(
        r#"  elif [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]] || [[ "$OSTYPE" == "win32" ]]; then"#
            .to_owned(),
    );
    lines.push(format!(r#"    ARGV0={argv0} "$_zc_bin" {args}"#));
    lines.push("  else".to_owned());
    lines.push(format!(r#"    (exec -a {argv0} "$_zc_bin" {args})"#));
    lines.push("  fi".to_owned());
    lines.push("}".to_owned());
    lines.join("\n")
}

/// 整段 prelude：先是快照里 rg 的一段（系统没有 rg 时才定义），再是 find / grep（`find_and_grep` 为真时；Windows 没有
/// bfs，不定义 find，embedded-search.md §6.4）。每行与快照里的写法相同（rg 函数的第一行前有两个空格，照
/// heredoc 的写法）。
pub(crate) fn prelude(search: &EmbeddedSearch, windows: bool) -> String {
    let native = search.executable.to_string_lossy();
    let fallback = if windows {
        git_bash_spelling(&native)
    } else {
        native.into_owned()
    };
    let mut lines = vec![
        "# Check for rg availability".to_owned(),
        "if ! hash rg 2>/dev/null && ! typeset -f rg >/dev/null 2>&1; then".to_owned(),
        format!("  {}", function("rg", "rg", &[], &[], &fallback)),
        "fi".to_owned(),
    ];
    if search.find_and_grep {
        lines.push("# Shadow find/grep with embedded bfs/ugrep".to_owned());
        if !windows {
            lines.push("unalias find 2>/dev/null || true".to_owned());
        }
        lines.push("unalias grep 2>/dev/null || true".to_owned());
        if !windows {
            lines.push(function("find", "bfs", &FIND_DEFAULTS, &[], &fallback));
        }
        lines.push(function(
            "grep",
            "ugrep",
            &GREP_DEFAULTS,
            &GREP_FALLBACK,
            &fallback,
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
#[path = "embedded_tests.rs"]
mod tests;
