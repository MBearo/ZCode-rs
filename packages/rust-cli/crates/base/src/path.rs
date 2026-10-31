//! 本机 `Path` 的纯词法运算（不查文件系统；README §4：只用 `components()`，不算文件类 IO）与空设备的路径。
//!
//! F58：以前 cli 的信任判定（`workspace_trust/git.rs`）、cli 的插件目录（`config/plugins.rs`）与 platform 的已安装
//! 插件目录（`plugin_inventory.rs`）各写一份。执行环境的字符串路径用 kernel-api 的 `PathStyle`，不用这里。

use std::path::{Component, Path, PathBuf};

/// Unix 的空设备（重定向的丢弃目标、沙箱里遮住文件用的空文件）。只定义在这里（工作线 F122，`SINGLE_DEFINITIONS`）。
pub const DEV_NULL: &str = "/dev/null";
/// Windows 盘符前缀 `C:` 的字节数（执行环境的字符串路径与本机路径都用它判断盘符）。
pub const DRIVE_PREFIX_LEN: usize = 2;

/// 按段去掉 `.`、`..` 回退一段（Node `path.resolve` 的规范化部分）。根与盘符前缀保留，`..` 越过根时停在根。
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
#[path = "path_tests.rs"]
mod tests;
