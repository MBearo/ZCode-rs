//! 信任记录的键（docs/specs/rust-cli/workspace-trust.md §2.2）。
use rust_cli_kernel_api::env_files::PathStyle;

use super::*;

fn posix(path: &str) -> String {
    Key::parse(path, PathStyle::Posix).to_string()
}

fn windows(path: &str) -> String {
    Key::parse(path, PathStyle::Windows).to_string()
}

#[test]
fn posix_keys_keep_case_and_backslashes() {
    assert_eq!(posix("/Users/dev/Work"), "/Users/dev/Work");
    assert_eq!(posix("/Users/dev/Work/"), "/Users/dev/Work");
    assert_eq!(posix("/Users//dev/./a/../Work"), "/Users/dev/Work");
    assert_eq!(posix("/"), "/");
    assert_eq!(posix("/.."), "/");
    // `\` 是 Posix 文件名里的合法字符。
    assert_eq!(posix("/tmp/a\\b"), "/tmp/a\\b");
}

#[test]
fn windows_keys_fold_case_separators_and_prefixes() {
    assert_eq!(windows("C:\\Users\\Dev\\Work"), "c:/users/dev/work");
    assert_eq!(windows("c:/users/dev/work\\"), "c:/users/dev/work");
    assert_eq!(windows("C:\\"), "c:/");
    assert_eq!(windows("\\\\?\\C:\\Users\\Dev"), "c:/users/dev");
    assert_eq!(
        windows("\\\\?\\UNC\\Server\\Share\\Dev"),
        "//server/share/dev"
    );
    assert_eq!(windows("\\\\Server\\Share"), "//server/share");
    assert_eq!(windows("C:\\Users\\..\\Dev\\.\\x"), "c:/dev/x");
}

#[test]
fn parents_stop_at_the_root() {
    let chain = |path: &str, style| {
        let mut key = Key::parse(path, style);
        let mut out = vec![key.to_string()];
        while let Some(up) = key.parent() {
            out.push(up.to_string());
            key = up;
        }
        out
    };
    assert_eq!(chain("/a/b", PathStyle::Posix), ["/a/b", "/a", "/"]);
    assert_eq!(chain("C:\\a", PathStyle::Windows), ["c:/a", "c:/"]);
    assert_eq!(
        chain("\\\\srv\\share\\a", PathStyle::Windows),
        ["//srv/share/a", "//srv/share"]
    );
}
