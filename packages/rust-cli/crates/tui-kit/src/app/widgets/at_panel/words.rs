//! `@` 面板里写死的话（docs/specs/rust-cli/tui-kit.md §4.1 第 4 条）：英文是 Puffin 的（[`AT_WORDS`]），旧界面的中文传一份
//! 译文，版面不变。只有文件与文件夹会画的话；会话与子代理的说法照旧是英文（旧界面的 `@` 不列它们）。

/// 面板与预览的话。
#[derive(Clone, Copy)]
pub struct AtWords {
    /// 文件还在找时组名下的一行。
    pub looking: &'static str,
    /// 什么都没有匹配。
    pub nothing: &'static str,
    /// 预览里文件夹名字后面的（带前面的两个空格）。
    pub folder: &'static str,
    pub not_found: &'static str,
    /// 碰过它的说法：第二栏的短的与预览的长的，各是改过、读过、git 说改了没提交。
    pub short_notes: [&'static str; 3],
    pub long_notes: [&'static str; 3],
    /// 文件的行数（第二个参数：只数到了 8 MB，后面还有）。
    pub lines: fn(u64, bool) -> String,
    /// 二进制文件，参数是大小的说法（`3 KB`）。
    pub binary: fn(&str) -> String,
    /// 文件夹的项数。
    pub entries: fn(u64) -> String,
    /// 一窗的项上面与下面还有多少。
    pub above: fn(usize) -> String,
    pub below: fn(usize) -> String,
    /// 组里多出的项：数目与组给的词（`files`）。
    pub more_of: fn(usize, &str) -> String,
}

/// Puffin 的话。
pub const AT_WORDS: AtWords = AtWords {
    looking: "Looking through the files…",
    nothing: "Nothing matches. It goes out as text.",
    folder: "  folder",
    not_found: "Not found.",
    short_notes: ["edited here", "read here", "changed"],
    long_notes: [
        "edited in this conversation",
        "read in this conversation",
        "changed, not committed",
    ],
    lines: |n, more| {
        format!(
            "{}{n} line{}",
            if more { "over " } else { "" },
            if n == 1 { "" } else { "s" }
        )
    },
    binary: |size| format!("binary · {size}"),
    entries: |n| format!("{n} entr{}", if n == 1 { "y" } else { "ies" }),
    above: |n| format!("↑ {n} more"),
    below: |n| format!("↓ {n} more"),
    more_of: |n, word| format!("↓ {n} more {word}"),
};
