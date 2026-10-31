//! cli 里几个模块共用的命名常量（docs/specs/rust-cli/constants.md §2）：数字的进制、短选项的长度、JS 风格的整数读法。

// ── 进制（`from_str_radix` 的参数）──

/// 十六进制。
pub(crate) const HEX_RADIX: u32 = rust_cli_base::units::HEX_RADIX;
/// 八进制。
pub(crate) const OCTAL_RADIX: u32 = 8;
/// 二进制。
pub(crate) const BINARY_RADIX: u32 = 2;

// ── 命令行 ──

/// 短选项 `-x` 的长度（字节）。
pub(crate) const SHORT_OPTION_LEN: usize = 2;

// ── JS 风格的整数读法（`limits::js_int`、`mcp` 的 `env_int`）──

/// 科学计数法与千分位写法只认这么长以内的值（limits 数 UTF-16 码元，mcp 数字节）。
pub(crate) const SHORT_NUMBER_LEN: usize = 32;
/// 千分位写法一组的数字个数（第一组 1 到这么多个）。
pub(crate) const DIGIT_GROUP_LEN: usize = 3;
