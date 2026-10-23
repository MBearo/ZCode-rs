//! Glob 绝对模式的拆分（docs/specs/rust-cli/tools-files.md §3.2，工作线 F57）。

use super::*;

fn split(pattern: &str, root: &str) -> Option<(String, String)> {
    split_absolute(pattern, &EnvPath::new(root))
}

fn pair(base: &str, relative: &str) -> Option<(String, String)> {
    Some((base.to_owned(), relative.to_owned()))
}

/// POSIX 只有 `/` 是分隔符：`\` 是 glob 的转义（以前也当分隔符，`/tmp/a\[x].rs` 拆成 `/tmp/a` 与 `[x].rs`）。
#[test]
fn posix_patterns_split_at_slashes_only() {
    for (pattern, expected) in [
        ("/tmp/*.rs", pair("/tmp", "*.rs")),
        (r"/tmp/a\[x].rs", pair("/tmp", r"a\[x].rs")),
        ("/tmp/src/**/x?.{ts,js}", pair("/tmp/src", "**/x?.{ts,js}")),
        ("/*.rs", pair("/", "*.rs")),
        // 没有通配符：`dirname` / `basename`，末尾的分隔符不算。
        ("/tmp/x.rs", pair("/tmp", "x.rs")),
        ("/tmp/dir/", pair("/tmp", "dir")),
        ("src/*.rs", None),
        (r"C:\x\*.rs", None),
    ] {
        assert_eq!(split(pattern, "/w"), expected, "{pattern}");
    }
}

/// Windows：Node `path.win32.isAbsolute`（`C:x` 不算）；`/` 与 `\` 都是分隔符；搜索目录是 `C:` 时补成 `C:\`
/// （以前是 `C:`，即 C 盘的当前目录）。
#[test]
fn windows_patterns_split_at_the_drive() {
    for (pattern, expected) in [
        (r"C:\x\*.rs", pair(r"C:\x", "*.rs")),
        ("C:/x/*.rs", pair("C:/x", "*.rs")),
        (r"C:\*.rs", pair(r"C:\", "*.rs")),
        (r"C:\x.rs", pair(r"C:\", "x.rs")),
        (r"\x\*.rs", pair(r"\x", "*.rs")),
        ("/x/*.rs", pair("/x", "*.rs")),
        (r"\\server\share\*.rs", pair(r"\\server\share", "*.rs")),
        (r"C:a\*.rs", None),
        (r"x\*.rs", None),
    ] {
        assert_eq!(split(pattern, r"C:\w"), expected, "{pattern}");
    }
}
