//! 起 `zcode-rs` 的真实进程测试在 `env_clear()` 之后用（工作线 F66）：Windows 上把系统必需的变量从测试进程带过去——
//! 没有 `SystemRoot` 时子进程的 Winsock 起不来（连不上本地的模型夹具），没有 `TEMP` / `TMP` 时 Node 写法的临时目录是
//! 相对路径 `undefined\temp`。Unix 上什么都不做（测试照旧只给自己设的变量）。

use std::process::Command;

/// Windows 上带过去的变量（只有系统本身要的，不带用户与 zcode 的配置）。
#[cfg(windows)]
pub const SYSTEM: &[&str] = &[
    "SystemRoot",
    "windir",
    "SystemDrive",
    "TEMP",
    "TMP",
    "ComSpec",
    "PATHEXT",
];

pub trait KeepSystem {
    /// 在 `env_clear()` 之后调用：Windows 上补回 [`SYSTEM`] 里测试进程有的变量。
    fn keep_system(&mut self) -> &mut Self;
}

impl KeepSystem for Command {
    #[allow(clippy::disallowed_methods, reason = "测试：从测试进程取系统变量")]
    fn keep_system(&mut self) -> &mut Self {
        #[cfg(windows)]
        for name in SYSTEM {
            if let Some(value) = std::env::var_os(name) {
                self.env(name, value);
            }
        }
        self
    }
}
