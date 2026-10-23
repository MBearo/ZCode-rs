//! 表驱动：工具路径参数的规则（
//! docs/specs/rust-cli/tools-files.md §3.2）。期望值是按 spec 的写法用 Node `path.posix` / `path.win32` 与
//! `String.prototype.normalize("NFC")` 实测的结果，再按本模块的约定去掉末尾分隔符（注释里写明 Node 的原样）。

use super::*;

const P: PathStyle = PathStyle::Posix;
const W: PathStyle = PathStyle::Windows;
const POSIX_HOME: &str = "/Users/dev";
const WINDOWS_HOME: &str = r"C:\Users\dev";

fn home(style: PathStyle) -> &'static str {
    match style {
        PathStyle::Posix => POSIX_HOME,
        PathStyle::Windows => WINDOWS_HOME,
    }
}

#[test]
fn user_paths_are_trimmed_like_js() {
    for (style, base, input, expected) in [
        (P, "/w", " /a/b ", "/a/b"),
        (P, "/w", "\t./x\n", "/w/x"),
        // 空串与只有空白：基准目录。
        (P, "/w", "", "/w"),
        (P, "/w", "   ", "/w"),
        // JS `trim` 的字符集：去掉 U+FEFF、U+3000、U+00A0，不去 U+0085（Rust `str::trim` 正好相反）。
        (P, "/w", "\u{feff}/a", "/a"),
        (P, "/w", "\u{3000}/a\u{a0}", "/a"),
        (P, "/w", "\u{85}/a", "/w/\u{85}/a"),
        // 末尾的空白在 `x ` 里被 trim 掉；在分隔符前面的不会（Node：`/a/b /` 原样保留，本模块去尾）。
        (P, "/w", "x ", "/w/x"),
        (P, "/w", "/a/b /", "/a/b "),
        (P, "/w", "/a/b/", "/a/b"),
        (W, r"C:\w", r" C:\a\ ", r"C:\a"),
        (W, r"C:\w", " /c/Users/x ", r"C:\Users\x"),
    ] {
        assert_eq!(
            style.resolve_user_path(base, input, Some(home(style))),
            expected,
            "{style:?} {input:?}"
        );
    }
}

#[test]
fn user_paths_come_out_in_nfc() {
    // 分解形式的韩文（ᄒ ᅡ ᆫ）与带重音的字母（e + U+0301）：macOS 的文件名常是这种写法，统一成合成形式（NFC）。
    let hangul = "\u{1112}\u{1161}\u{11ab}";
    let accent = "cafe\u{301}";
    for (style, base, input, expected) in [
        (P, "/w", &*format!("/w/{hangul}.txt"), "/w/\u{d55c}.txt"),
        (P, "/w", &*format!(" {accent}/x "), "/w/caf\u{e9}/x"),
        (W, r"C:\w", hangul, "C:\\w\\\u{d55c}"),
        // 基准目录与 home 也在结果里，一起做 NFC。
        (P, &*format!("/w/{accent}"), "x", "/w/caf\u{e9}/x"),
        (P, &*format!("/w/{accent}"), "", "/w/caf\u{e9}"),
        // 已经是合成形式的不变。
        (P, "/w", "/w/\u{d55c}/caf\u{e9}", "/w/\u{d55c}/caf\u{e9}"),
    ] {
        assert_eq!(
            style.resolve_user_path(base, input, Some(home(style))),
            expected,
            "{style:?} {input:?}"
        );
    }
    assert_eq!(nfc(&format!("{accent}{hangul}")), "caf\u{e9}\u{d55c}");
}

#[test]
fn only_tilde_slash_reaches_home() {
    for (style, base, input, expected) in [
        (P, "/w", "~", POSIX_HOME),
        (P, "/w", " ~/x ", "/Users/dev/x"),
        (P, "/w", "~x", "/w/~x"),
        (W, r"C:\w", "~/x", r"C:\Users\dev\x"),
        // Windows 上也只认 `~/`：`~\x` 是工作区里名为 `~` 的目录下的 `x`（`resolve` 仍按 home，见 §3.1）。
        (W, r"C:\w", r"~\x", r"C:\w\~\x"),
    ] {
        assert_eq!(
            style.resolve_user_path(base, input, Some(home(style))),
            expected,
            "{style:?} {input:?}"
        );
    }
    // 没有 home：`~` 当普通相对路径（与 `resolve` 相同）。
    assert_eq!(P.resolve_user_path("/w", "~/x", None), "/w/~/x");
    for (style, input, needs) in [
        (P, "~", true),
        (P, " ~/x", true),
        (P, " ~ ", true),
        (P, "~x", false),
        (P, "/a/~/x", false),
        (W, r"~\x", false),
        (W, "~/x", true),
    ] {
        assert_eq!(
            style.user_path_needs_home(input),
            needs,
            "{style:?} {input:?}"
        );
    }
    // 旧规则不变（只加不改）：`resolve` 在 Windows 上仍展开 `~\x`、不 trim。
    assert_eq!(
        W.resolve(r"C:\w", r"~\x", Some(WINDOWS_HOME)),
        r"C:\Users\dev\x"
    );
    assert_eq!(P.resolve("/w", " /a", Some(POSIX_HOME)), "/w/ /a");
}

#[test]
fn stable_user_paths_reject_a_trailing_white_space_result() {
    for (style, base, input, expected) in [
        // 规范化之后以空白结尾：再解析会被 trim 成别的路径。
        (P, "/w", "/tmp/x /.", Err(UnstableUserPath)),
        (P, "/w", "x /.", Err(UnstableUserPath)),
        // 相对路径一支是 Node `resolve`，会去掉末尾的 `/`，所以 `x /` 也不稳定。
        (P, "/w", "x /", Err(UnstableUserPath)),
        (P, "/w", "~/x /.", Err(UnstableUserPath)),
        (W, r"C:\w", r"C:\x \.", Err(UnstableUserPath)),
        (W, r"C:\w", r"x \", Err(UnstableUserPath)),
        // Node `normalize` / `join` 保留末尾分隔符：`/tmp/x /` 稳定（Node 给 `/tmp/x /`）。
        (P, "/w", "/tmp/x /", Ok("/tmp/x ")),
        (P, "/w", "~/x /", Ok("/Users/dev/x ")),
        (W, r"C:\w", r"C:\x \", Ok(r"C:\x ")),
        (P, "/w", "x ", Ok("/w/x")),
        (P, "/w", " /a/b ", Ok("/a/b")),
        (P, "/w", "", Ok("/w")),
    ] {
        assert_eq!(
            style.resolve_user_path_stable(base, input, Some(home(style))),
            expected.map(str::to_owned),
            "{style:?} {input:?}"
        );
    }
}
