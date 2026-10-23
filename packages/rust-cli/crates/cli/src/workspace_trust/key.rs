//! 信任记录的键（docs/specs/rust-cli/workspace-trust.md §2.2）：工作区规范化后的绝对路径。
//!
//! Posix 不改大小写、不改 `\`；Windows 去掉 `\\?\` 前缀、`\` 换成 `/`、整串转小写（路径不分大小写，盘符的大小写
//! 常随 shell 变）。按段去掉 `.` 与空段、`..` 回退一段；根是 `/`、`c:/` 或 `//server/share`。

use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::workspace_trust::TrustKey;

/// 规范化后的路径：根与各段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Key {
    root: String,
    segments: Vec<String>,
}

impl Key {
    pub(crate) fn parse(path: &str, style: PathStyle) -> Self {
        let text = match style {
            PathStyle::Posix => path.to_owned(),
            PathStyle::Windows => {
                let unprefixed = if let Some(rest) = path.strip_prefix("\\\\?\\UNC\\") {
                    format!("\\\\{rest}")
                } else {
                    path.strip_prefix("\\\\?\\").unwrap_or(path).to_owned()
                };
                unprefixed.replace('\\', "/").to_lowercase()
            }
        };
        let (root, rest) = split_root(&text, style);
        let mut segments: Vec<String> = Vec::new();
        for segment in rest.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    segments.pop();
                }
                other => segments.push(other.to_owned()),
            }
        }
        Self { root, segments }
    }

    /// 上一层；已经是根时为 None。
    pub(crate) fn parent(&self) -> Option<Self> {
        let mut segments = self.segments.clone();
        segments.pop()?;
        Some(Self {
            root: self.root.clone(),
            segments,
        })
    }

    pub(crate) fn trust_key(&self) -> TrustKey {
        TrustKey::new(self.to_string())
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let unc = self.root.starts_with("//");
        match (unc, self.segments.is_empty()) {
            (true, true) => f.write_str(&self.root),
            (true, false) => write!(f, "{}/{}", self.root, self.segments.join("/")),
            (false, _) => write!(f, "{}{}", self.root, self.segments.join("/")),
        }
    }
}

/// 根与其余部分：Posix 的 `/`；Windows 的盘符（`c:/`）或 UNC 的 `//server/share`。
fn split_root(text: &str, style: PathStyle) -> (String, &str) {
    if style == PathStyle::Windows {
        let bytes = text.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return (format!("{}/", &text[..2]), &text[2..]);
        }
        if let Some(rest) = text.strip_prefix("//") {
            let mut parts = rest.splitn(3, '/');
            let server = parts.next().unwrap_or_default();
            let share = parts.next().unwrap_or_default();
            return (
                format!("//{server}/{share}"),
                parts.next().unwrap_or_default(),
            );
        }
    }
    match text.strip_prefix('/') {
        Some(rest) => ("/".to_owned(), rest),
        None => (String::new(), text),
    }
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod tests;
