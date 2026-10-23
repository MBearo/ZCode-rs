//! 本机临时目录（修复线 F58，README §4.1）：入口从快照算一次，Bash 输出与账本的私有目录、跨会话的 socket、外部编辑器
//! 的临时文件都用这一个值。
//!
//! 照 Node `os.tmpdir()`（与 Node 版 zcode 读同样的变量）。以前有两种算法：入口用 Rust 的
//! `std::env::temp_dir()`（Unix 只看 `TMPDIR`，`TMPDIR=` 时得到空路径，macOS 没设时取 confstr 的目录；Windows 先看
//! `TMP`），外部编辑器自己按 `TMPDIR` → `TEMP` → `TMP` → `/tmp`。

use std::path::PathBuf;

use rust_cli_base::secret::EnvSnapshot;

/// Node `os.tmpdir()`：Unix 是 `TMPDIR` → `TMP` → `TEMP` → `/tmp`（空值跳过），长度大于 1 时去掉一个末尾的 `/`；
/// Windows 是 `TEMP` → `TMP` → `(SystemRoot || windir) + "\temp"`，去掉末尾的 `\`（前一个字符是 `:` 时不去）。
pub(crate) fn node_tmpdir(env: &EnvSnapshot, windows: bool) -> PathBuf {
    let first = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| env.get(name).filter(|value| !value.is_empty()))
            .map(str::to_owned)
    };
    if windows {
        let path = first(&["TEMP", "TMP"]).unwrap_or_else(|| {
            // Node 写的是 `(process.env.SystemRoot || process.env.windir) + '\\temp'`，两个都没有时是 `undefined\temp`。
            let root = first(&["SystemRoot", "windir"]).unwrap_or_else(|| "undefined".to_owned());
            format!(r"{root}\temp")
        });
        let bytes = path.as_bytes();
        if bytes.len() > 1 && bytes.ends_with(b"\\") && bytes[bytes.len() - 2] != b':' {
            return PathBuf::from(&path[..path.len() - 1]);
        }
        return PathBuf::from(path);
    }
    let path = first(&["TMPDIR", "TMP", "TEMP"]).unwrap_or_else(|| "/tmp".to_owned());
    match path.strip_suffix('/') {
        Some(trimmed) if path.len() > 1 => PathBuf::from(trimmed),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
#[path = "temp_dir_tests.rs"]
mod tests;
