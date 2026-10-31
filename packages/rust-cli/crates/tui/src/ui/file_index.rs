//! `@` 补全的文件索引（docs/specs/rust-cli/tui-input.md §1.2，nucleo 风格的打分）。纯内存，不做 IO：清单由内核的
//! `workspace_files` 给出，这里只建索引与匹配。
//!
//! 与原实现的出入：小写只转 ASCII（位置与原文逐字节对齐）；位置与长度按字节算（ASCII 路径与 UTF-16 相同）。

/// 每次最多给出这么多候选。
pub const MAX_SUGGESTIONS: usize = 15;
const SCORE_MATCH: i64 = 16;
const BONUS_BOUNDARY: i64 = 8;
const BONUS_CAMEL: i64 = 6;
const BONUS_CONSECUTIVE: i64 = 4;
const BONUS_FIRST_CHAR: i64 = 8;
const PENALTY_GAP_START: i64 = 3;
const PENALTY_GAP_EXTENSION: i64 = 1;
const MAX_QUERY_LEN: usize = 64;
const TOP_LEVEL_LIMIT: usize = 100;
/// 短路径的加分：`32 − 路径长 / 4`，不小于 0。
const SHORT_PATH_BONUS: i64 = 32;

pub struct FileIndex {
    paths: Vec<String>,
    lower: Vec<String>,
    /// 每条路径含有的 a–z 字母（位图），用来先排除不可能匹配的路径。
    bits: Vec<u32>,
    top: Vec<String>,
}

fn letters(text: &str) -> u32 {
    text.bytes()
        .filter(u8::is_ascii_lowercase)
        .fold(0, |bits, b| bits | 1 << (b - b'a'))
}

fn is_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

/// 全部父目录（带分隔符），按第一次出现的顺序。
fn directories(files: &[String], separator: char) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for file in files {
        let mut ends: Vec<usize> = file
            .char_indices()
            .filter(|(_, c)| is_separator(*c))
            .map(|(index, _)| index)
            .collect();
        // 由深到浅，遇到已经记过的目录就停（它的父目录也记过了）。
        ends.reverse();
        let mut chain = Vec::new();
        for end in ends {
            let dir = &file[..end];
            if !seen.insert(dir.to_owned()) {
                break;
            }
            chain.push(format!("{dir}{separator}"));
        }
        chain.reverse();
        out.extend(chain);
    }
    out
}

/// 顶层条目：第一段，按长度再按名字排序；目录带分隔符。
fn top_level(paths: &[String], separator: char) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    for path in paths {
        let entry = match path.find(is_separator) {
            Some(end) => format!("{}{separator}", &path[..end]),
            None => path.clone(),
        };
        seen.insert(entry);
        if seen.len() >= TOP_LEVEL_LIMIT {
            break;
        }
    }
    let mut out: Vec<String> = seen.into_iter().collect();
    out.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    out
}

/// 匹配位置前一个字符带来的加分。
fn bonus(path: &[u8], position: usize, first: bool) -> i64 {
    if position == 0 {
        return if first { BONUS_FIRST_CHAR } else { 0 };
    }
    let previous = path[position - 1];
    if matches!(previous, b'/' | b'\\' | b'-' | b'_' | b'.' | b' ') {
        return BONUS_BOUNDARY;
    }
    if previous.is_ascii_lowercase() && path[position].is_ascii_uppercase() {
        return BONUS_CAMEL;
    }
    0
}

impl FileIndex {
    /// `files` 是相对路径（执行环境的分隔符）；索引是全部父目录加全部文件，去掉重复。
    pub fn new(files: Vec<String>, separator: char) -> Self {
        let mut seen = std::collections::HashSet::new();
        let files: Vec<String> = files
            .into_iter()
            .filter(|file| !file.is_empty() && seen.insert(file.clone()))
            .collect();
        let mut paths = directories(&files, separator);
        paths.extend(files);
        let lower: Vec<String> = paths.iter().map(|p| p.to_ascii_lowercase()).collect();
        let bits = lower.iter().map(|p| letters(p)).collect();
        let top = top_level(&paths, separator);
        Self {
            paths,
            lower,
            bits,
            top,
        }
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// 模糊匹配（smart case），按分数从高到低，最多 `limit` 条；空查询给顶层条目。
    pub fn search(&self, query: &str, limit: usize) -> Vec<String> {
        if limit == 0 {
            return Vec::new();
        }
        if query.is_empty() {
            return self.top.iter().take(limit).cloned().collect();
        }
        let sensitive = query.chars().any(char::is_uppercase);
        let needle: String = if sensitive {
            query.to_owned()
        } else {
            query.to_lowercase()
        };
        let chars: Vec<char> = needle.chars().take(MAX_QUERY_LEN).collect();
        let wanted = letters(&needle.to_ascii_lowercase());
        let mut found: Vec<(i64, usize)> = Vec::new();
        let mut positions = Vec::with_capacity(chars.len());
        'paths: for (index, path) in self.paths.iter().enumerate() {
            if self.bits[index] & wanted != wanted {
                continue;
            }
            let haystack = if sensitive { path } else { &self.lower[index] };
            positions.clear();
            let mut from = 0;
            let (mut gaps, mut runs) = (0i64, 0i64);
            for (step, c) in chars.iter().enumerate() {
                let Some(offset) = haystack[from..].find(*c) else {
                    continue 'paths;
                };
                let at = from + offset;
                if step > 0 {
                    let previous = positions[step - 1];
                    let gap = (at - previous - 1) as i64;
                    if gap == 0 {
                        runs += BONUS_CONSECUTIVE;
                    } else {
                        gaps += PENALTY_GAP_START + gap * PENALTY_GAP_EXTENSION;
                    }
                }
                positions.push(at);
                from = at + c.len_utf8();
            }
            let bytes = path.as_bytes();
            let mut score = chars.len() as i64 * SCORE_MATCH + runs - gaps;
            for (step, at) in positions.iter().enumerate() {
                score += bonus(bytes, *at, step == 0);
            }
            score += (SHORT_PATH_BONUS - (path.len() as i64 >> 2)).max(0);
            found.push((score, index));
        }
        found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        found
            .into_iter()
            .take(limit)
            .map(|(_, index)| self.paths[index].clone())
            .collect()
    }
}

#[cfg(test)]
#[path = "file_index_tests.rs"]
mod tests;
