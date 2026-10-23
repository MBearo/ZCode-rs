//! 编入二进制的文本只有 LF（修复线 F56，docs/specs/rust-cli/prompts.md「换行」、node-store.md §2.1）。
//!
//! 修的 bug：Windows 上 git 的 `core.autocrlf=true` 以前把仓库里的文本检出成 CRLF，`include_str!` 按字节编入，
//! `\r` 就进了二进制。迁移 SQL 的校验和因此与 Node 不同（Node 的 SQL 在 JS 模板字符串里，CRLF 被规范成 LF），
//! 用户的库打不开（0.1.52 的 `checksum_mismatch`）；发给模型的文本也与其他平台不同。
//!
//! 修法有两层：根本的一层是 `packages/rust-cli/.gitattributes` 固定 LF；这里是第二层，产品代码编入文本时用
//! [`include_lf!`](crate::include_lf)，文本含 `\r` 就编译失败，带 `\r` 的版本发不出去。

/// 文本里有没有 `\r`（CRLF 或单独的 CR）。是 `const fn`，供编译期断言用。
pub const fn has_cr(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' {
            return true;
        }
        index += 1;
    }
    false
}

/// 与 `include_str!` 相同（路径相对调用处的文件），另在编译期断言文本不含 `\r`：含有时编译失败，
/// 错误里有文件路径。可以用在 `const` 的初始化里。
#[macro_export]
macro_rules! include_lf {
    ($path:expr $(,)?) => {{
        const TEXT: &str = ::core::include_str!($path);
        const _: () = ::core::assert!(
            !$crate::embed::has_cr(TEXT),
            ::core::concat!(
                "embedded text contains \\r (checked out with CRLF? see packages/rust-cli/.gitattributes): ",
                $path
            )
        );
        TEXT
    }};
}

#[cfg(test)]
#[path = "embed_tests.rs"]
mod tests;
