//! `@` 面板的文件搜索（docs/specs/rust-cli/tui-input.md §1.2；照 Puffin 的 host 的 `mentions.rs` 里的
//! `entries`、`score`、`search`、`limited`、`changed_paths`）：纯的，runner 在异步任务里对缓存的清单调用，回答像 host 的
//! `atFiles` 一样晚一步到（孪生里与 Puffin 同一个时刻）。
//!
//! - 清单是 `Kernel::workspace_files`（执行环境，远端也一样），分隔符统一成 `/`；文件夹从文件的路径推出，以 `/` 结尾。
//! - 匹配不分大小写：文件名含它先于路径含它先于按次序含它的字母；同分时路径短的在前，再按路径。筛选词为空时是这个文件夹
//!   直接的项，文件夹在前。
//!
//! 以前是另一套 `FileIndex` 的移植（smart case、加分扣分、前 15 名），面板按它的次序（2026-10-31 改照 Puffin）。

/// 一次最多给面板这么多个。
pub const MAX_HITS: usize = 200;
/// 文件夹的预览列这么多个名字。
pub const PEEK_NAMES: usize = 24;

/// 一个文件或文件夹（文件夹的以 `/` 结尾）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHit {
    pub path: String,
    pub folder: bool,
}

/// 最多 [`MAX_HITS`] 个，按匹配的次序；`total` 是全部匹配的数目。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileSearch {
    pub hits: Vec<FileHit>,
    pub total: u64,
}

/// 执行环境的清单（分隔符是 `separator`）→ 文件与从它们推出的文件夹，按第一次出现的次序，分隔符是 `/`。
pub fn entries(files: &[String], separator: char) -> Vec<FileHit> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for file in files.iter().filter(|f| !f.is_empty()) {
        let file = file.replace(separator, "/");
        let mut end = 0;
        while let Some(at) = file[end..].find('/') {
            end += at + 1;
            let folder = &file[..end];
            if seen.insert(folder.to_owned()) {
                out.push(FileHit {
                    path: folder.to_owned(),
                    folder: true,
                });
            }
        }
        if seen.insert(file.clone()) {
            out.push(FileHit {
                path: file,
                folder: false,
            });
        }
    }
    out
}

/// 不分大小写的匹配分：文件名含它 > 路径含它 > 按次序含它的字母；不匹配时 None。
fn score(path: &str, query: &str) -> Option<i64> {
    let path = path.to_lowercase();
    let trimmed = path.trim_end_matches('/');
    let name_at = trimmed.rfind('/').map_or(0, |at| at + 1);
    if let Some(at) = trimmed[name_at..].find(query) {
        return Some(3_000_000 - at as i64);
    }
    if let Some(at) = path.find(query) {
        return Some(2_000_000 - at as i64);
    }
    // 按次序：每跳过一个字符扣一分。
    let mut gaps = 0i64;
    let mut wanted = query.chars().peekable();
    for c in path.chars() {
        match wanted.peek() {
            Some(&next) if next == c => {
                wanted.next();
            }
            Some(_) => gaps += 1,
            None => break,
        }
    }
    wanted.peek().is_none().then(|| 1_000_000 - gaps)
}

/// 文件夹在前，再按路径（不分大小写）。
fn folder_then_name(a: &FileHit, b: &FileHit) -> std::cmp::Ordering {
    b.folder
        .cmp(&a.folder)
        .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
}

/// 在 `under`（`""` 是根，否则以 `/` 结尾）下找 `query`（规则见模块头）。
pub fn search(entries: &[FileHit], query: &str, under: &str) -> Vec<FileHit> {
    let query = query.to_lowercase();
    if query.is_empty() {
        let mut direct: Vec<FileHit> = entries
            .iter()
            .filter(|hit| {
                hit.path.starts_with(under) && {
                    let rest = hit.path[under.len()..].trim_end_matches('/');
                    !rest.is_empty() && !rest.contains('/')
                }
            })
            .cloned()
            .collect();
        direct.sort_by(folder_then_name);
        return direct;
    }
    let mut scored: Vec<(i64, &FileHit)> = entries
        .iter()
        .filter(|hit| hit.path.starts_with(under) && hit.path.len() > under.len())
        .filter_map(|hit| Some((score(&hit.path[under.len()..], &query)?, hit)))
        .collect();
    scored.sort_by(|(a, x), (b, y)| {
        b.cmp(a)
            .then_with(|| x.path.len().cmp(&y.path.len()))
            .then_with(|| x.path.cmp(&y.path))
    });
    scored.into_iter().map(|(_, hit)| hit.clone()).collect()
}

/// 截成至多 [`MAX_HITS`] 个，带全部的数目。
pub fn limited(hits: Vec<FileHit>) -> FileSearch {
    let total = hits.len() as u64;
    FileSearch {
        hits: hits.into_iter().take(MAX_HITS).collect(),
        total,
    }
}

/// numstat 与未跟踪的文件 → 改动的路径（去重，按出现的次序）。改名写成 `a => b` 或 `dir/{a => b}/x`，取新的路径。
pub fn changed_paths(numstat: &str, untracked: &str) -> Vec<String> {
    let renamed = |path: &str| -> String {
        match (path.find('{'), path.find(" => "), path.find('}')) {
            (Some(open), Some(arrow), Some(close)) if open < arrow && arrow < close => {
                let joined = format!(
                    "{}{}{}",
                    &path[..open],
                    &path[arrow + 4..close],
                    &path[close + 1..]
                );
                joined.replace("//", "/")
            }
            (_, Some(arrow), _) => path[arrow + 4..].to_owned(),
            _ => path.to_owned(),
        }
    };
    let mut out: Vec<String> = Vec::new();
    let numstat = numstat
        .lines()
        .filter_map(|line| line.splitn(3, '\t').nth(2))
        .map(renamed);
    let untracked = untracked.lines().map(str::to_owned);
    for path in numstat.chain(untracked) {
        let path = path.trim().to_owned();
        if !path.is_empty() && !path.starts_with("../") && !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
#[path = "at_search_tests.rs"]
mod tests;
