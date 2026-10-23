// 复制自 rust/crates/kit/src/js.rs（feat/rs 79b6691c55）。
//! 与 JavaScript 语义一致的文本工具（docs/specs/rust-host-cli-boundary.md §3.1）：移植 Node 逻辑时
//! CLI 与 Host 都要用的 `String.prototype.trim` / UTF-16 长度、`JSON.stringify` 与
//! `Date.prototype.toISOString`、`Number.prototype.toString(36)`。

pub mod date;
pub mod json;
mod radix;
pub mod string;

pub use date::iso_timestamp;
pub use radix::base36;

/// Node `process.platform` / `os.platform()` 对应当前编译目标的值（`darwin`、`win32`、`linux` …）。
///
/// F55：以前 platform（`credentials::node_platform`）与 env-local（`host::platform`）各写了一份，函数体相同。
pub fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// Node `os.arch()` 对应当前编译目标的值（`arm64`、`x64`、`ia32` …；工作线 F64，模型请求的 `x-platform`）。
pub fn node_arch() -> &'static str {
    node_arch_of(std::env::consts::ARCH)
}

/// Rust 的架构名换成 Node 的写法；没有对应的原样返回。
pub fn node_arch_of(arch: &str) -> &str {
    match arch {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        "loongarch64" => "loong64",
        "powerpc" => "ppc",
        "powerpc64" => "ppc64",
        other => other,
    }
}

/// JS 定时器的最大毫秒数（`setTimeout` 的 2^31 - 1；更大的值取这个上限）。
///
/// F55：以前 cli（MCP 的超时）与 tools（Bash 的后台上限）各写了一份。
pub const MAX_TIMER_MS: u64 = 2_147_483_647;

#[cfg(test)]
#[path = "js_tests.rs"]
mod tests;
