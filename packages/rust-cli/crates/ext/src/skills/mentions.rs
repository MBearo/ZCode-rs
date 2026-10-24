//! 人类键入的输入中间的 `/<skill 名>`（docs/specs/rust-cli/ext.md §2.8、context.md §4.4）：找出对得上可调用 skill
//! 的名字，context 据此加 `skill_mention` 提醒。

use std::collections::HashSet;

use rust_cli_kernel_api::env::WorkspaceRef;

use super::SkillCatalog;

/// 名字前面可以是的字符：空白，或中文的 `。`、`、`、`？`、`！`。
fn opens(previous: Option<char>) -> bool {
    previous.is_none_or(|c| c.is_whitespace() || matches!(c, '。' | '、' | '？' | '！'))
}

fn is_name(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_')
}

/// 输入里的 `/<名字>`：`(start, name_start, end)` 字节下标（`start` 是 `/`）。名字末尾的 `.` 不算；`/` 前面与名字后面
/// 都要是空白或边缘；开头的命令跳过（context.md §4.4）。
pub(super) fn tokens(input: &str) -> Vec<(usize, usize, usize)> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = input[at..].find('/') {
        let start = at + offset;
        let previous = input[..start].chars().next_back();
        let rest = &input[start + 1..];
        let run = rest.find(|c: char| !is_name(c)).unwrap_or(rest.len());
        // 正则吃掉整段（含末尾的 `.`），下一次从它后面找。
        at = start + 1 + run;
        if !opens(previous) || !rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
            at = start + 1;
            continue;
        }
        let name = rest[..run].trim_end_matches('.');
        let end = start + 1 + name.len();
        let next = input[end..].chars().next();
        let spaced = |c: Option<char>| c.is_none_or(char::is_whitespace);
        found.push((start, start + 1, end, spaced(previous) && spaced(next)));
    }
    // 输入以 `/` 开头时，开头那串只隔空白的 `/x` 是命令本身，跳过。
    let mut leading = input.starts_with('/');
    let mut from = 0;
    let mut out = Vec::new();
    for (start, name_start, end, spaced) in found {
        if leading && input[from..start].trim().is_empty() {
            from = end;
            continue;
        }
        leading = false;
        if spaced {
            out.push((start, name_start, end));
        }
    }
    out
}

impl SkillCatalog {
    /// 输入里对得上可调用 skill 的名字，同一个 skill 一次、按出现顺序。禁止模型调用的也算（用户键入的）；print
    /// 会话返回空（`-p` 与 stream-json 的输入不算人类键入）。
    pub async fn mentions(&self, workspace: &WorkspaceRef, input: &str) -> Vec<String> {
        if self.print_session() {
            return Vec::new();
        }
        let tokens = tokens(input);
        if tokens.is_empty() {
            return Vec::new();
        }
        let skills = self.skills(workspace).await;
        let mut seen = HashSet::new();
        tokens
            .into_iter()
            .filter_map(|(_, name_start, end)| {
                let name = &input[name_start..end];
                let skill = skills.iter().find(|skill| skill.name == name)?;
                seen.insert(skill.name.clone()).then(|| name.to_owned())
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "mentions_tests.rs"]
mod tests;
