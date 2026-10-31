//! 行级 diff 与改动片段（docs/specs/rust-cli/tools-files.md §4.1）。
//!
//! 算法照 jsdiff 9（`diff@9.0.0` 的 `base.js`、`line.js`、`patch/create.js`）移植：按行切分（行尾换行算进行里），
//! Myers 的对角线逐步推进，同一步里能删能增时先看哪条路径走得更远；hunk 按上下文行数合并。片段只要新文件里的行：
//! 去掉删除行，从 hunk 在旧文件里的起始行开始编号。

use std::rc::Rc;

use rust_cli_base::text;

use crate::common::utf16_len;
use crate::read_support::number_lines;

/// 片段里每个 hunk 前后的上下文行数。
const CONTEXT: usize = 8;
/// 一个文件的片段最多这么多 UTF-16 单元，超过时在这之前的最后一个换行处截断。
const MAX_SNIPPET: usize = 8192;
/// 编辑距离的上限：超过时不算 diff（相当于算 diff 超时；只定义在 base 的 `text`）。
const MAX_EDITS: usize = text::MAX_DIFF_EDITS;
/// Myers 每一步只走同奇偶的对角线。
const DIAGONAL_STEP: isize = 2;
/// 最后这么多个 change 之外的公共段，短时并进同一个 hunk。
const TAIL_CHANGES: usize = 2;

/// 一段连续的相同 / 新增 / 删除的行（jsdiff 的 component）。
struct Component {
    count: usize,
    added: bool,
    removed: bool,
    previous: Option<Rc<Component>>,
}

#[derive(Clone)]
struct Path {
    old_pos: isize,
    last: Option<Rc<Component>>,
}

/// 按行切分：每个元素是一行连同它的换行（`\n` 或 `\r\n`），最后一行可以没有换行；空行之外的空串去掉。
fn tokenize(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = 0;
    for (index, _) in text.match_indices('\n') {
        tokens.push(&text[start..=index]);
        start = index + 1;
    }
    if start < text.len() {
        tokens.push(&text[start..]);
    }
    tokens
}

fn add_to_path(path: &Path, added: bool, removed: bool, old_step: isize) -> Path {
    let last = match &path.last {
        Some(last) if last.added == added && last.removed == removed => Component {
            count: last.count + 1,
            added,
            removed,
            previous: last.previous.clone(),
        },
        last => Component {
            count: 1,
            added,
            removed,
            previous: last.clone(),
        },
    };
    Path {
        old_pos: path.old_pos + old_step,
        last: Some(Rc::new(last)),
    }
}

/// 沿对角线走过相同的行；返回新文件里的位置。
fn extract_common(path: &mut Path, new: &[&str], old: &[&str], diagonal: isize) -> isize {
    let (new_len, old_len) = (new.len() as isize, old.len() as isize);
    let mut old_pos = path.old_pos;
    let mut new_pos = old_pos - diagonal;
    let mut common = 0;
    while new_pos + 1 < new_len
        && old_pos + 1 < old_len
        && old[(old_pos + 1) as usize] == new[(new_pos + 1) as usize]
    {
        new_pos += 1;
        old_pos += 1;
        common += 1;
    }
    if common > 0 {
        path.last = Some(Rc::new(Component {
            count: common,
            added: false,
            removed: false,
            previous: path.last.take(),
        }));
    }
    path.old_pos = old_pos;
    new_pos
}

/// 一段改动：行（连同换行）与种类。
struct Change<'a> {
    lines: Vec<&'a str>,
    added: bool,
    removed: bool,
}

fn values<'a>(last: Option<Rc<Component>>, new: &[&'a str], old: &[&'a str]) -> Vec<Change<'a>> {
    let mut chain = Vec::new();
    let mut cursor = last;
    while let Some(component) = cursor {
        cursor = component.previous.clone();
        chain.push(component);
    }
    chain.reverse();
    let (mut new_pos, mut old_pos) = (0, 0);
    chain
        .into_iter()
        .map(|component| {
            let lines = if component.removed {
                let lines = old[old_pos..old_pos + component.count].to_vec();
                old_pos += component.count;
                lines
            } else {
                let lines = new[new_pos..new_pos + component.count].to_vec();
                new_pos += component.count;
                if !component.added {
                    old_pos += component.count;
                }
                lines
            };
            Change {
                lines,
                added: component.added,
                removed: component.removed,
            }
        })
        .collect()
}

/// 两段文字的逐行改动；编辑距离超过上限时为 None。
fn diff_lines<'a>(old_text: &'a str, new_text: &'a str) -> Option<Vec<Change<'a>>> {
    let old = tokenize(old_text);
    let new = tokenize(new_text);
    let (new_len, old_len) = (new.len() as isize, old.len() as isize);
    let max_edits = (new.len() + old.len()).min(MAX_EDITS) as isize;
    let offset = max_edits + 1;
    let slot = |diagonal: isize| (diagonal + offset) as usize;
    let mut best: Vec<Option<Path>> = vec![None; (2 * offset + 1) as usize];
    let mut first = Path {
        old_pos: -1,
        last: None,
    };
    let new_pos = extract_common(&mut first, &new, &old, 0);
    if first.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
        return Some(values(first.last, &new, &old));
    }
    best[slot(0)] = Some(first);
    let (mut min_diagonal, mut max_diagonal) = (isize::MIN, isize::MAX);
    for edits in 1..=max_edits {
        let mut diagonal = min_diagonal.max(-edits);
        while diagonal <= max_diagonal.min(edits) {
            let remove = best[slot(diagonal - 1)].take();
            let add = best[slot(diagonal + 1)].clone();
            let can_add = add
                .as_ref()
                .is_some_and(|path| (0..new_len).contains(&(path.old_pos - diagonal)));
            let can_remove = remove
                .as_ref()
                .is_some_and(|path| path.old_pos + 1 < old_len);
            if !can_add && !can_remove {
                best[slot(diagonal)] = None;
                diagonal += DIAGONAL_STEP;
                continue;
            }
            // 删不了，或者能增且增的那条路在旧文件里走得更远：增；否则删。
            let mut path = match (&remove, &add) {
                (Some(remove), Some(add)) if can_add && remove.old_pos < add.old_pos => {
                    add_to_path(add, true, false, 0)
                }
                (Some(remove), _) if can_remove => add_to_path(remove, false, true, 1),
                (_, Some(add)) => add_to_path(add, true, false, 0),
                _ => {
                    best[slot(diagonal)] = None;
                    diagonal += DIAGONAL_STEP;
                    continue;
                }
            };
            let new_pos = extract_common(&mut path, &new, &old, diagonal);
            if path.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                return Some(values(path.last, &new, &old));
            }
            if path.old_pos + 1 >= old_len {
                max_diagonal = max_diagonal.min(diagonal - 1);
            }
            if new_pos + 1 >= new_len {
                min_diagonal = min_diagonal.max(diagonal + 1);
            }
            best[slot(diagonal)] = Some(path);
            diagonal += DIAGONAL_STEP;
        }
    }
    None
}

/// 一个 hunk：在旧文件里的起始行与 jsdiff 的行（带 ` ` / `+` / `-` 前缀，不带换行）。
struct Hunk {
    old_start: usize,
    lines: Vec<String>,
}

/// `structuredPatch(old, new, {context})` 的 hunk。
fn hunks(changes: &[Change<'_>], context: usize) -> Vec<Hunk> {
    let mut out = Vec::new();
    let (mut range_start, mut current) = (0_usize, Vec::<String>::new());
    let mut old_line = 1_usize;
    // 末尾补一段空的相同行，最后一个 hunk 由它收尾。
    let tail = Change {
        lines: Vec::new(),
        added: false,
        removed: false,
    };
    let all: Vec<&Change<'_>> = changes.iter().chain(std::iter::once(&tail)).collect();
    let prefixed = |sign: char, lines: &[&str]| -> Vec<String> {
        lines.iter().map(|line| format!("{sign}{line}")).collect()
    };
    for (index, change) in all.iter().enumerate() {
        if change.added || change.removed {
            if range_start == 0 {
                range_start = old_line;
                current = match index.checked_sub(1).map(|previous| all[previous]) {
                    Some(previous) if context > 0 => {
                        let skip = previous.lines.len().saturating_sub(context);
                        prefixed(' ', &previous.lines[skip..])
                    }
                    _ => Vec::new(),
                };
                range_start -= current.len();
            }
            let sign = if change.added { '+' } else { '-' };
            current.extend(prefixed(sign, &change.lines));
            if !change.added {
                old_line += change.lines.len();
            }
        } else {
            if range_start != 0 {
                if change.lines.len() <= context * 2 && index + TAIL_CHANGES < all.len() {
                    current.extend(prefixed(' ', &change.lines));
                } else {
                    let take = change.lines.len().min(context);
                    current.extend(prefixed(' ', &change.lines[..take]));
                    out.push(Hunk {
                        old_start: range_start,
                        lines: std::mem::take(&mut current),
                    });
                    range_start = 0;
                }
            }
            old_line += change.lines.len();
        }
    }
    for hunk in &mut out {
        let mut lines = Vec::with_capacity(hunk.lines.len());
        for line in hunk.lines.drain(..) {
            match line.strip_suffix('\n') {
                Some(stripped) => lines.push(stripped.to_owned()),
                None => {
                    lines.push(line);
                    lines.push("\\ No newline at end of file".to_owned());
                }
            }
        }
        hunk.lines = lines;
    }
    out
}

/// 改动片段：每个 hunk 只留新文件里的行，从 hunk 在旧文件里的起始行编号，hunk 之间是 `\n...\n`；太长时截断。
/// 算不出 diff（超过编辑距离上限）或没有可显示的行时为空。
pub(crate) fn snippet(old: &str, new: &str) -> String {
    let Some(changes) = diff_lines(old, new) else {
        return String::new();
    };
    let text = hunks(&changes, CONTEXT)
        .iter()
        .map(|hunk| {
            let content = hunk
                .lines
                .iter()
                .filter(|line| !line.starts_with('-') && !line.starts_with('\\'))
                .map(|line| &line[1..])
                .collect::<Vec<_>>()
                .join("\n");
            if content.is_empty() {
                String::new()
            } else {
                number_lines(&content, hunk.old_start as u64)
            }
        })
        .collect::<Vec<_>>()
        .join("\n...\n");
    truncate(text)
}

/// 超过 [`MAX_SNIPPET`] 个 UTF-16 单元时：在这之前（含）的最后一个换行处截断（没有换行时按长度截），接上没显示的行数。
fn truncate(text: String) -> String {
    if utf16_len(&text) <= MAX_SNIPPET {
        return text;
    }
    let limit = byte_at_utf16(&text, MAX_SNIPPET);
    // 从第 8192 个单元往前找换行（含第 8192 个单元本身）。
    let search_end = (limit + 1).min(text.len());
    let search_end = (search_end..=text.len())
        .find(|end| text.is_char_boundary(*end))
        .unwrap_or(text.len());
    let kept = match text[..search_end].rfind('\n') {
        Some(at) if at > 0 => &text[..at],
        _ => &text[..limit],
    };
    let hidden = text[(kept.len() + 1).min(text.len())..]
        .matches('\n')
        .count()
        + 1;
    format!("{kept}\n\n... [{hidden} lines truncated] ...")
}

/// 第 `units` 个 UTF-16 单元处的字节位置（落在一个字符中间时取它之前）。
fn byte_at_utf16(text: &str, units: usize) -> usize {
    let mut counted = 0;
    for (index, ch) in text.char_indices() {
        if counted + ch.len_utf16() > units {
            return index;
        }
        counted += ch.len_utf16();
    }
    text.len()
}

#[cfg(test)]
#[path = "line_diff_tests.rs"]
mod tests;
