//! Grep 的参数拼装与结果排版（rg 参数、行格式、分页与分页说明）。

use rust_cli_kernel_api::env_files::{GrepFileLines, GrepLineKind, VCS_DIRS};

use crate::common::utf16_len;

const DEFAULT_HEAD_LIMIT: usize = 250;

pub(crate) fn count(value: Option<f64>) -> u64 {
    value
        .filter(|value| *value > 0.0)
        .map_or(0, |value| value as u64)
}

/// `--glob` 列表：空白分隔；不含花括号的再按逗号切开。
pub(crate) fn globs(glob: Option<&str>) -> Vec<String> {
    // 版本控制目录总是排除（表只定义在 kernel-api，F58）。
    let mut list: Vec<String> = VCS_DIRS.iter().map(|dir| format!("!{dir}")).collect();
    for part in glob.unwrap_or("").split_whitespace() {
        if part.contains('{') && part.contains('}') {
            list.push(part.to_owned());
        } else {
            list.extend(
                part.split(',')
                    .filter(|item| !item.is_empty())
                    .map(str::to_owned),
            );
        }
    }
    list
}

/// 超过 500 个字符的行不展示。
fn clip(line: &str, kind: GrepLineKind) -> String {
    if utf16_len(line) <= 500 {
        return line.to_owned();
    }
    match kind {
        GrepLineKind::Match => "[Omitted long matching line]".to_owned(),
        GrepLineKind::Context => "[Omitted long context line]".to_owned(),
    }
}

/// 把执行环境给的匹配块排成 `路径\0行号:内容` / `路径\0行号-内容` 与 `--`。
pub(crate) fn content_lines(files: &[GrepFileLines], breaks: bool, only: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut last: Option<(String, u64)> = None;
    for file in files {
        let path = file.path.as_str();
        let mut pending = Vec::new();
        for line in &file.lines {
            let body = line
                .text
                .strip_suffix('\n')
                .map_or(line.text.as_str(), |text| {
                    text.strip_suffix('\r').unwrap_or(text)
                });
            if breaks
                && last
                    .as_ref()
                    .is_some_and(|(prev, end)| prev != path || line.line_number > end + 1)
            {
                pending.push("--".to_owned());
            }
            if line.kind == GrepLineKind::Match && only {
                let mut end = line.line_number;
                for sub in &line.submatches {
                    let before = usize::try_from(sub.start)
                        .unwrap_or(usize::MAX)
                        .min(body.len());
                    let first = line.line_number
                        + body.as_bytes()[..before]
                            .iter()
                            .filter(|b| **b == b'\n')
                            .count() as u64;
                    for (index, part) in sub.text.split('\n').enumerate() {
                        let part = part.strip_suffix('\r').unwrap_or(part);
                        pending.push(format!(
                            "{path}\0{}:{}",
                            first + index as u64,
                            clip(part, GrepLineKind::Match)
                        ));
                        end = first + index as u64;
                    }
                }
                last = Some((path.to_owned(), end));
                continue;
            }
            let marker = if line.kind == GrepLineKind::Match {
                ':'
            } else {
                '-'
            };
            let parts: Vec<&str> = body
                .split('\n')
                .map(|part| part.strip_suffix('\r').unwrap_or(part))
                .collect();
            for (index, part) in parts.iter().enumerate() {
                pending.push(format!(
                    "{path}\0{}{marker}{}",
                    line.line_number + index as u64,
                    clip(part, line.kind)
                ));
            }
            last = Some((path.to_owned(), line.line_number + parts.len() as u64 - 1));
        }
        if let Some(offset) = file.binary_offset {
            let had = !pending.is_empty();
            pending.retain(|item| {
                !item
                    .split_once('\0')
                    .is_some_and(|(_, rest)| rest.contains('\0'))
            });
            if had {
                pending.push(format!(
                    "{path}\0binary file matches (found \"\\0\" byte around offset {offset})"
                ));
            }
        }
        out.extend(pending);
    }
    out
}

/// 分页：head_limit 为 0 表示不限。
pub(crate) fn paginate<T: Clone>(
    items: &[T],
    head_limit: Option<usize>,
    offset: usize,
) -> (Vec<T>, Option<usize>) {
    let rest = items.get(offset..).unwrap_or(&[]);
    match head_limit {
        Some(0) => (rest.to_vec(), None),
        limit => {
            let limit = limit.unwrap_or(DEFAULT_HEAD_LIMIT);
            let shown = rest.iter().take(limit).cloned().collect();
            (shown, (rest.len() > limit).then_some(limit))
        }
    }
}

pub(crate) fn pagination(limit: Option<usize>, offset: usize) -> String {
    let mut parts = Vec::new();
    if let Some(limit) = limit {
        parts.push(format!("limit: {limit}"));
    }
    if offset > 0 {
        parts.push(format!("offset: {offset}"));
    }
    parts.join(", ")
}

pub(crate) fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

/// 不区分大小写的近似 `localeCompare`。
pub(crate) fn locale_order(left: &str, right: &str) -> std::cmp::Ordering {
    left.to_lowercase()
        .cmp(&right.to_lowercase())
        .then_with(|| left.cmp(right))
}
