//! 本机 `Path` 的纯词法运算（不查文件系统；README §4：只用 `components()`，不算文件类 IO）。
//!
//! F58：以前 cli 的信任判定（`workspace_trust/git.rs`）、cli 的插件目录（`config/plugins.rs`）与 platform 的已安装
//! 插件目录（`plugin_inventory.rs`）各写一份。执行环境的字符串路径用 kernel-api 的 `PathStyle`，不用这里。

use std::path::{Component, Path, PathBuf};

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
mod tests {
    use super::*;

    #[test]
    fn dots_fold_lexically() {
        let fold = |text: &str| normalize(Path::new(text));
        assert_eq!(fold("/a/./b/../c"), PathBuf::from("/a/c"));
        assert_eq!(fold("/a/b/.."), PathBuf::from("/a"));
        assert_eq!(fold("/.."), PathBuf::from("/"));
        assert_eq!(fold("a/../../b"), PathBuf::from("b"));
        assert_eq!(
            fold("/w/repo/.git/worktrees/x/../.."),
            PathBuf::from("/w/repo/.git")
        );
    }
}
