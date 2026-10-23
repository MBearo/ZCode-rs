//! 可以写在命令前、不影响判定的环境变量（Node
//! `bash-readonly-policy-argv-io.ts` 的同一张表）。只读判定（`readonly/argv.rs`）与规则匹配前去掉前缀
//! （`shell_rules::strip_safe_wrappers`）共用这一张（修复线 F58，README §4.1）。以前规则匹配用另一张
//! `SAFE_ENV_VARS`（32 个），少 `COLUMNS`、`LINES`、`CLICOLOR`、`CLICOLOR_FORCE`、`CI`、`DEBIAN_FRONTEND`、
//! `GIT_TERMINAL_PROMPT`。顺序固定。

pub(crate) const SAFE_ENV_VARS: [&str; 39] = [
    "GOEXPERIMENT",
    "GOOS",
    "GOARCH",
    "CGO_ENABLED",
    "GO111MODULE",
    "RUST_BACKTRACE",
    "RUST_LOG",
    "NODE_ENV",
    "PYTHONUNBUFFERED",
    "PYTHONDONTWRITEBYTECODE",
    "PYTEST_DISABLE_PLUGIN_AUTOLOAD",
    "PYTEST_DEBUG",
    "ANTHROPIC_API_KEY",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_TIME",
    "CHARSET",
    "TERM",
    "COLORTERM",
    "NO_COLOR",
    "FORCE_COLOR",
    "TZ",
    "LS_COLORS",
    "LSCOLORS",
    "GREP_COLOR",
    "GREP_COLORS",
    "GCC_COLORS",
    "TIME_STYLE",
    "BLOCK_SIZE",
    "BLOCKSIZE",
    "COLUMNS",
    "LINES",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "CI",
    "DEBIAN_FRONTEND",
    "GIT_TERMINAL_PROMPT",
];

/// 名字在表里。
pub(crate) fn is_safe(name: &str) -> bool {
    SAFE_ENV_VARS.contains(&name)
}
