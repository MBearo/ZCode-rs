//! policy 里跨模块的常量（docs/specs/rust-cli/constants.md §2）：按含义分段。

// ---- shell 记号的宽度（字符数；这些记号都是 ASCII，字节数与字符数相同） ----

/// 反斜杠转义：`\` 与它后面的一个字符（续行 `\` + 换行也是）。
pub(crate) const ESCAPE_LEN: usize = 2;
/// 前缀符号与它后面的一个字符：`$(`、`${`、`$'`、`$"`、`$name` 的头两个字符、`<(`、`>(`、extglob 的 `?(`。
pub(crate) const SIGIL_PAIR_LEN: usize = 2;
/// 双字符的运算符：`;;`、`&&`、`||`、`&>`、`((`、`))`、`<<`。
pub(crate) const DOUBLE_OPERATOR_LEN: usize = 2;

// ---- 命令行的词 ----

/// 短选项 `-x` 的长度：`-` 与一个字母（更长的是选项簇或带值的写法 `-n3`）。
pub(crate) const SHORT_OPTION_LEN: usize = 2;
/// 带值的选项与它的值分成两个词（`-k 5`、`-u NAME`）：越过时一次走两个。
pub(crate) const OPTION_WITH_VALUE_WORDS: usize = 2;
/// `nice -n 5` 占的词：命令名、`-n` 与优先级。
pub(crate) const NICE_DASH_N_WORDS: usize = 3;
/// `-newerXY` 后面的两个时间戳字母（find、bfs）。
pub(crate) const NEWER_XY_LETTERS: usize = 2;
/// 复制与移动至少有源与目标两个操作数（cp、mv、robocopy）。
pub(crate) const MIN_COPY_OPERANDS: usize = 2;

// ---- Windows 盘符 ----

/// 盘符 `C:` 的字节数。
pub(crate) const DRIVE_LEN: usize = rust_cli_base::path::DRIVE_PREFIX_LEN;
/// 盘符根 `C:/`（或 `C:\`）的字节数。
pub(crate) const DRIVE_ROOT_LEN: usize = DRIVE_LEN + 1;
