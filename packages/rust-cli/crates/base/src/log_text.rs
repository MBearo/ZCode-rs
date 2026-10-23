//! 写进运行日志的文字片段（docs/specs/rust-cli/logging.md §4）。
//!
//! F58：以前 ext 的 MCP 失败原因（`logged_reason`）与 query 的 `tool.failed` 各写一份“前 300 个字符”。

/// 错误文字进日志时最多保留的字符数（按 char）。
pub const HEAD_CHARS: usize = 300;

/// 错误文字的开头 [`HEAD_CHARS`] 个字符：日志只记系统文字的开头，不带长的响应体或用户内容。
pub fn head(text: &str) -> String {
    text.chars().take(HEAD_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_300_characters_are_kept() {
        assert_eq!(head("short"), "short");
        let long = "字".repeat(400);
        assert_eq!(head(&long).chars().count(), 300);
    }
}
