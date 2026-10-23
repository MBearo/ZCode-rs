//! 表驱动：除注明的行外，每一行是 Node `path.posix` / `path.win32` 的结果（`node -e` 实测），去尾约定见模块文档。
//! 旧实现之间不一致的行在注释里写明取哪一种、为什么（docs/specs/rust-cli/tools-files.md §3.1）。

use super::*;

const P: PathStyle = PathStyle::Posix;
const W: PathStyle = PathStyle::Windows;

#[test]
fn style_comes_from_platform_or_from_the_path() {
    assert_eq!(PathStyle::of_platform("win32"), W);
    assert_eq!(PathStyle::of_platform("darwin"), P);
    assert_eq!(PathStyle::of_platform("linux"), P);
    for (path, style) in [
        ("/Users/dev/w", P),
        (r"C:\work", W),
        ("c:/work", W),
        (r"\\server\share", W),
        (r"\work", W),
        // 旧拷贝（agents、ext、memory）看“含不含 `\`”：POSIX 目录名可以含 `\`，按开头判断才对。
        (r"/Users/dev/a\b", P),
        ("relative", P),
    ] {
        assert_eq!(PathStyle::infer(path), style, "{path}");
    }
}

#[test]
fn normalize_follows_node() {
    for (style, input, expected) in [
        (P, "/", "/"),
        (P, "/a/./b//c/", "/a/b/c"),
        (P, "//a", "/a"),
        // 根之上的 `..`：绝对路径停在根。
        (P, "/..", "/"),
        (P, "/a/../..", "/"),
        // 相对路径保留开头的 `..`（Node）；以前丢掉，只影响相对路径。
        (P, "a/../../b", "../b"),
        (P, "", "."),
        (P, r"/a\b", r"/a\b"),
        (W, r"C:\", r"C:\"),
        (W, "c:/a/b/", r"c:\a\b"),
        // 盘符大小写保留（Node）；以前 policy 改成大写。
        (W, r"c:\Users\dev\..\x", r"c:\Users\x"),
        (W, r"C:\a\..\..", r"C:\"),
        (W, r"C:\..", r"C:\"),
        // `C:`、`C:a` 是盘符相对路径；以前 policy 把它们当成 `C:\`。
        (W, "C:", "C:."),
        (W, "C:a", "C:a"),
        (W, r"C:a\..\..", "C:.."),
        // UNC 的根是 `\\server\share\`，`..` 不越过共享名；以前 tools、context 的根是 `\\`。
        (W, r"\\server\share", r"\\server\share\"),
        (W, r"\\server\share\a\..\..", r"\\server\share\"),
        (W, "//server/share/x", r"\\server\share\x"),
        (W, r"\/server/share/a", r"\\server\share\a"),
        (W, r"\\server", r"\server"),
        (W, r"\\\a", r"\a"),
        (W, r"\\?\C:\x", r"\\?\C:\x"),
        (W, r"\foo", r"\foo"),
        (W, "/foo", r"\foo"),
        (W, r"a\..\..\b", r"..\b"),
        (W, "", "."),
    ] {
        assert_eq!(style.normalize(input), expected, "{style:?} {input}");
    }
}

#[test]
fn absolute_paths_follow_node_is_absolute() {
    for (style, input, expected) in [
        (P, "/a", true),
        (P, "a", false),
        (P, r"C:\a", false),
        (W, r"C:\a", true),
        (W, "C:/a", true),
        (W, "C:", false),
        (W, "C:a", false),
        (W, r"\a", true),
        (W, "/a", true),
        (W, r"\\server\share", true),
        (W, "a", false),
    ] {
        assert_eq!(style.is_absolute(input), expected, "{style:?} {input}");
    }
}

#[test]
fn resolve_follows_expand_path() {
    let home = Some("/home/dev");
    let win_home = Some(r"C:\Users\dev");
    for (style, base, input, home, expected) in [
        (P, "/w", "a.txt", None, "/w/a.txt"),
        (P, "/w", "/w/./x/../a.txt", None, "/w/a.txt"),
        (P, "/w/", "dir/", None, "/w/dir"),
        (P, "/w", "", None, "/w"),
        (P, "/w", "~", home, "/home/dev"),
        (P, "/w", "~/notes", home, "/home/dev/notes"),
        (P, "/w", "~/notes", None, "/w/~/notes"),
        (P, "/w", "~x", home, "/w/~x"),
        // POSIX 不认 `~\`。
        (P, "/w", r"~\x", home, r"/w/~\x"),
        (P, "/w/docs", "../../../b.md", None, "/b.md"),
        (W, r"C:\work", "src/a.ts", None, r"C:\work\src\a.ts"),
        (W, r"C:\work", "D:/x/../y", None, r"D:\y"),
        (W, r"C:\work", r"..\..\..", None, r"C:\"),
        (W, r"C:\w", "~/x.md", win_home, r"C:\Users\dev\x.md"),
        // `~\` 按 home 展开（旧实现一致；工具路径参数只认 `~/`，见 tools-files.md §3.1）。
        (W, r"C:\w", r"~\x.md", win_home, r"C:\Users\dev\x.md"),
        // `\x`、`/x` 是绝对路径，原样规范化；以前 tools 接在工作区后面。
        (W, r"C:\work", r"\foo", None, r"\foo"),
        // `/c/...` 换成盘符路径；以前三套实现三种结果。
        (W, r"D:\work", "/c/Users/x", None, r"C:\Users\x"),
        (W, r"D:\work", "/C/", None, r"C:\"),
        (W, r"D:\work", "/cc/x", None, r"\cc\x"),
        // 盘符相对：同盘接在基准后面，别的盘接在那个盘的根后面（Node `path.win32.resolve`）。
        (W, r"C:\work", "C:foo", None, r"C:\work\foo"),
        (W, r"C:\work", "c:foo", None, r"C:\work\foo"),
        (W, r"D:\work", "C:foo", None, r"C:\foo"),
        // UNC 基准：`..` 停在共享名。
        (W, r"\\s\sh\w", r"..\..\x", None, r"\\s\sh\x"),
    ] {
        assert_eq!(
            style.resolve(base, input, home),
            expected,
            "{style:?} {base} {input}"
        );
    }
}

#[test]
fn needs_home_only_for_tilde_forms_of_the_style() {
    assert!(P.needs_home("~"));
    assert!(P.needs_home("~/x"));
    assert!(!P.needs_home(r"~\x"));
    assert!(!P.needs_home("~x"));
    assert!(W.needs_home(r"~\x"));
    assert!(W.needs_home("~/x"));
}

#[test]
fn parents_stop_at_the_root() {
    for (style, input, parent, dirname) in [
        (P, "/Users/dev/w", Some("/Users/dev"), "/Users/dev"),
        (P, "/Users", Some("/"), "/"),
        (P, "/", None, "/"),
        (P, "/a/b/", Some("/a"), "/a"),
        (P, "a", Some("."), "."),
        (P, ".", None, "."),
        // 盘符根：`C:\a` 的上一层是 `C:\`；以前 agents、ext 的拷贝给 `C:`。
        (W, r"C:\work\proj", Some(r"C:\work"), r"C:\work"),
        (W, r"C:\w", Some(r"C:\"), r"C:\"),
        (W, r"C:\", None, r"C:\"),
        (W, "C:/", None, r"C:\"),
        (
            W,
            r"\\server\share\a",
            Some(r"\\server\share\"),
            r"\\server\share\",
        ),
        (W, r"\\server\share", None, r"\\server\share\"),
        (W, "C:a", Some("C:"), "C:"),
    ] {
        assert_eq!(style.parent(input).as_deref(), parent, "{style:?} {input}");
        assert_eq!(style.dirname(input), dirname, "{style:?} {input}");
    }
}

#[test]
fn ancestors_run_from_below_the_root_down_to_the_dir() {
    assert_eq!(
        P.ancestors("/Users/dev/w"),
        ["/Users", "/Users/dev", "/Users/dev/w"]
    );
    assert!(P.ancestors("/").is_empty());
    assert_eq!(W.ancestors(r"C:\Users\dev"), [r"C:\Users", r"C:\Users\dev"]);
    assert_eq!(W.ancestors(r"\\s\sh\a"), [r"\\s\sh\a"]);
}

#[test]
fn names_joins_and_extensions() {
    for (style, input, name) in [
        (P, "/w/AGENTS.md", "AGENTS.md"),
        (P, "/w/a.TXT", "a.TXT"),
        (P, "/", ""),
        (W, r"C:\w\a.md", "a.md"),
        (W, "C:/w/a.md", "a.md"),
        // 根没有名字：Node `basename` 对 UNC 根给共享名，这里与 `C:\` 一致给空串。
        (W, r"C:\", ""),
        (W, r"\\server\share", ""),
    ] {
        assert_eq!(style.file_name(input), name, "{style:?} {input}");
    }
    assert_eq!(
        P.join("/Users/dev/w", &[".zcode", "rules"]),
        "/Users/dev/w/.zcode/rules"
    );
    assert_eq!(P.join("/w/", &["./skills/x"]), "/w/skills/x");
    assert_eq!(
        W.join(r"C:\ProgramData\ZCode", &["AGENTS.md"]),
        r"C:\ProgramData\ZCode\AGENTS.md"
    );
    assert_eq!(W.join(r"C:\", &["a"]), r"C:\a");
    assert_eq!(
        W.join(r"C:\p", &["hooks/hooks.json"]),
        r"C:\p\hooks\hooks.json"
    );
    assert_eq!(W.join(r"\\s\sh", &["a"]), r"\\s\sh\a");
    for (name, extension) in [
        ("AGENTS.md", ".md"),
        ("notes.TXT", ".txt"),
        (".env", ""),
        ("Makefile", ""),
        ("a.b.png", ".png"),
        ("/w/archive.tar.gz", ".gz"),
        ("/w/.bashrc", ""),
        (r"C:\w\a.TS", ".ts"),
    ] {
        assert_eq!(super::extension(name), extension, "{name}");
    }
}

#[test]
fn comparison_ignores_case_and_separators_on_windows_only() {
    assert_eq!(W.key(r"C:\W\A.md"), W.key("c:/w/a.md"));
    assert!(W.same(r"C:\Users\dev\", r"c:\users\DEV"));
    assert!(!P.same("/Users/Dev", "/Users/dev"));
    assert!(P.same("/w/", "/w"));
    for (style, path, dir, within) in [
        (P, "/w/a/b", "/w", true),
        (P, "/w", "/w", true),
        (P, "/w/", "/w", true),
        (P, "/wx", "/w", false),
        (P, "/etc/hosts", "/", true),
        (W, r"c:\users\dev\w\a.md", r"C:\Users\dev", true),
        (W, r"C:\Work\a", r"c:\work", true),
        (W, r"C:\a", r"C:\", true),
        (W, r"D:\a", r"C:\", false),
        (W, r"\\S\SH\a", r"\\s\sh", true),
        (W, r"C:\workx", r"C:\work", false),
    ] {
        assert_eq!(style.is_within(path, dir), within, "{style:?} {path} {dir}");
    }
}

#[test]
fn relative_is_the_rest_inside_the_dir() {
    for (style, dir, path, expected) in [
        (P, "/w", "/w/src/a.ts", Some("src/a.ts")),
        (P, "/w", "/w", Some("")),
        (P, "/w/", "/w/a", Some("a")),
        (P, "/w", "/x/a.ts", None),
        (P, "/w", "/w2/a.ts", None),
        (P, "/", "/a/b", Some("a/b")),
        (W, r"C:\w", r"C:\w\src\a.ts", Some(r"src\a.ts")),
        (W, r"C:\work", r"c:\WORK\src\a.ts", Some(r"src\a.ts")),
        (W, r"C:\", r"C:\a\b", Some(r"a\b")),
        (W, r"C:\w", r"D:\w\a", None),
    ] {
        assert_eq!(
            style.relative(dir, path).as_deref(),
            expected,
            "{style:?} {dir} {path}"
        );
    }
}

#[test]
fn unc_paths_are_recognized_in_both_styles() {
    for (path, unc) in [
        (r"\\server\share\AGENTS.md", true),
        ("//server/share", true),
        ("///a", false),
        ("/w", false),
        (r"C:\w", false),
    ] {
        assert_eq!(is_unc(path), unc, "{path}");
    }
}

/// 工作线 F57：Windows 路径的 Git Bash 写法。期望值按三条规则手算。
#[test]
fn windows_paths_have_a_git_bash_spelling() {
    for (path, expected) in [
        (r"C:\Users\dev\a.txt", "/c/Users/dev/a.txt"),
        ("D:/tools/zcode.exe", "/d/tools/zcode.exe"),
        (r"c:\", "/c/"),
        (r"\\server\share\x", "//server/share/x"),
        (r"\\?\C:\x", "//?/C:/x"),
        // 没有分隔符的盘符不换（规则要求 `C:` 后面是 `/` 或 `\`）。
        ("C:", "C:"),
        ("C:x", "C:x"),
        (r"rel\dir", "rel/dir"),
        ("/already/posix", "/already/posix"),
    ] {
        assert_eq!(git_bash_spelling(path), expected, "{path}");
    }
}
