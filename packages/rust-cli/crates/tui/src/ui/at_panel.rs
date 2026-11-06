//! `@` 面板的组（docs/puffin-tui.md §4.2「提及（@）」；照 Puffin `shell/at.rs` 的 `split`、`rank`、`rank_path`、
//! `relative`、`touched`、`path_panel`、`mixed_panel`）：从 host 式的回答（[`AtFiles`]）、这个对话碰过的文件与 git 的改动
//! 整理成 kit 的 [`AtPanel`]，行与预览由 kit 的 `at_panel::rows` 画。纯的。
//!
//! 与 Puffin 的不同：旧界面的 `@` 列文件、文件夹与会话（不列工作流运行与子代理，能力的差别），所以是文件与会话两组；组名与
//! 「多出的」按界面语言（[`words`]）。
use rust_cli_tui_kit::app::widgets::at_panel::data::ConversationRef;
use rust_cli_tui_kit::app::widgets::at_panel::words::{AT_WORDS, AtWords};
use rust_cli_tui_kit::app::widgets::at_panel::{AtGroup, AtItem, AtPanel, Touch, name, title_of};

use super::at_search::FileSearch;
use super::i18n::Lang;

pub const MAX_ROWS: usize = 14;
pub const NARROW_MAX_ROWS: usize = 12;
const NO_MATCH: usize = 5;

/// 文件搜索的回答：`query` 在 `under` 下找到的。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AtFiles {
    pub query: String,
    pub under: String,
    pub found: FileSearch,
}

/// 面板除了回答还要的：这个对话碰过的文件（新的在前）与 git 说改了没提交的。
pub struct Sources<'a> {
    pub files: Option<&'a AtFiles>,
    pub touched: &'a [(String, Touch)],
    pub changed: &'a [String],
    /// 这个工作区最近的会话（面板打开时 runner 读，到之前 None）与现在的会话（不列）。
    pub conversations: Option<&'a [ConversationRef]>,
    pub current: Option<&'a str>,
    pub lang: Lang,
}

/// 词在最后一个 `/` 处分开：之前的路径（带 `/`）与之后的字。
pub fn split(word: &str) -> (String, String) {
    let w = word.strip_prefix("./").unwrap_or(word);
    match w.rfind('/') {
        None => (String::new(), w.to_owned()),
        Some(i) => (w[..=i].to_owned(), w[i + 1..].to_owned()),
    }
}

fn subsequence(q: &str, within: &str) -> bool {
    let mut qs = q.chars().peekable();
    for c in within.chars() {
        if qs.peek() == Some(&c) {
            qs.next();
        }
    }
    qs.peek().is_none()
}

/// `q` 找到一个名字有多好，0 最好（不分大小写）：名字以它开头；名字的一段或各段的首字母以它开头；名字含它；名字按次序含
/// 它的字母。[`NO_MATCH`] 是不相符。
fn rank(n0: &str, q0: &str) -> usize {
    let n = n0.to_lowercase();
    let q = q0.to_lowercase();
    if q.is_empty() || n.starts_with(&q) {
        return 0;
    }
    let parts: Vec<&str> = n
        .split(['-', ':', '_', '.', ' '])
        .filter(|p| !p.is_empty())
        .collect();
    let initials: String = parts.iter().filter_map(|p| p.chars().next()).collect();
    if parts.iter().any(|p| p.starts_with(&q)) || initials.starts_with(&q) {
        1
    } else if n.contains(&q) {
        2
    } else if subsequence(&q, &n) {
        3
    } else {
        NO_MATCH
    }
}

/// 路径的 [`rank`]：按它的名字，或在第三、第四档按整个路径。
fn rank_path(path: &str, q0: &str) -> usize {
    let q = q0.to_lowercase();
    let p = path.to_lowercase();
    let by_path = if p.contains(&q) {
        2
    } else if subsequence(&q, &p) {
        3
    } else {
        NO_MATCH
    };
    rank(name(path), q0).min(by_path)
}

const EDITS: [&str; 4] = ["Edit", "MultiEdit", "Write", "NotebookEdit"];

/// 工具输入的路径相对 `cwd` 的写法；在它外面为 `None`。
fn relative(path0: &str, cwd0: &str) -> Option<String> {
    let path = path0.replace('\\', "/");
    let cwd = cwd0.replace('\\', "/");
    let cwd = cwd.strip_suffix('/').unwrap_or(&cwd);
    let drive = path.len() >= 3
        && path.as_bytes()[0].is_ascii_alphabetic()
        && &path.as_bytes()[1..3] == b":/";
    if path.is_empty() {
        None
    } else if let Some(rest) = path.strip_prefix(&format!("{cwd}/")) {
        Some(rest.to_owned())
    } else if path.starts_with('/') || drive || path.starts_with("../") {
        None
    } else {
        Some(path.strip_prefix("./").unwrap_or(&path).to_owned())
    }
}

/// 这个对话的工具行读过或改过的文件，新的在前；读过又改过的算改过。`tools` 是从旧到新的 (工具名, 输入)。
pub fn touched<'a>(
    tools: impl DoubleEndedIterator<Item = (&'a str, &'a serde_json::Value)>,
    cwd: &str,
) -> Vec<(String, Touch)> {
    let mut seen: Vec<(String, Touch)> = Vec::new();
    for (tool, input) in tools.rev() {
        let touch = if tool == "Read" {
            Touch::Read
        } else if EDITS.contains(&tool) {
            Touch::Edited
        } else {
            continue;
        };
        let raw = ["file_path", "notebook_path"]
            .iter()
            .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
            .unwrap_or("");
        let Some(p) = relative(raw, cwd) else {
            continue;
        };
        match seen.iter_mut().find(|(q, _)| *q == p) {
            None => seen.push((p, touch)),
            Some(had) if had.1 == Touch::Read && touch == Touch::Edited => had.1 = touch,
            Some(_) => {}
        }
    }
    seen
}

/// 这个对话对每个路径做了什么：它自己的在 git 的之上。
pub fn touches(s: &Sources<'_>) -> std::collections::BTreeMap<String, Touch> {
    let mut out: std::collections::BTreeMap<String, Touch> = s
        .changed
        .iter()
        .map(|p| (p.clone(), Touch::Changed))
        .collect();
    for (p, t) in s.touched.iter().rev() {
        out.insert(p.clone(), *t);
    }
    out
}

/// 组名与多出的词（Puffin 的英文，或中文）。
struct Labels {
    files: &'static str,
    recent: &'static str,
    files_in: fn(&str) -> String,
    file: &'static str,
    files_word: &'static str,
    conversations: &'static str,
    conversation: &'static str,
    conversations_word: &'static str,
}

fn labels(lang: Lang) -> Labels {
    match lang {
        Lang::En => Labels {
            files: "Files",
            recent: "Recent files",
            files_in: |under| format!("Files in {under}"),
            file: "file",
            files_word: "files",
            conversations: "Conversations",
            conversation: "conversation",
            conversations_word: "conversations",
        },
        Lang::Zh => Labels {
            files: "文件",
            recent: "最近的文件",
            files_in: |under| format!("{under} 里的文件"),
            file: "个文件",
            files_word: "个文件",
            conversations: "会话",
            conversation: "个会话",
            conversations_word: "个会话",
        },
    }
}

/// kit 的行与预览的话（[`AT_WORDS`] 是 Puffin 的英文）。
pub fn words(lang: Lang) -> AtWords {
    match lang {
        Lang::En => AT_WORDS,
        Lang::Zh => AtWords {
            looking: "正在查找文件…",
            nothing: "没有匹配的。它会作为文字发出。",
            folder: "  文件夹",
            not_found: "没有找到。",
            short_notes: ["这里改过", "这里读过", "改过"],
            long_notes: ["在这个对话里改过", "在这个对话里读过", "改过，还没提交"],
            lines: |n, more| format!("{}{n} 行", if more { "超过 " } else { "" }),
            binary: |size| format!("二进制 · {size}"),
            entries: |n| format!("{n} 项"),
            above: |n| format!("↑ 还有 {n} 项"),
            below: |n| format!("↓ 还有 {n} 项"),
            more_of: |n, word| format!("↓ 还有 {n} {word}"),
        },
    }
}

/// `@` 词 `word` 的面板，最多 `max_rows` 行。
pub fn panel(word: &str, s: &Sources<'_>, max_rows: usize) -> AtPanel {
    let (under, q) = split(word);
    if under.is_empty() {
        mixed_panel(q, s, max_rows)
    } else {
        path_panel(under, q, s)
    }
}

fn recent_of<'a>(s: &'a Sources<'_>) -> std::collections::BTreeMap<&'a str, usize> {
    let mut out = std::collections::BTreeMap::new();
    for (i, (p, _)) in s.touched.iter().enumerate() {
        out.entry(p.as_str()).or_insert(i);
    }
    out
}

fn file(p: &str, folder: bool) -> AtItem {
    AtItem::Path {
        path: p.to_owned(),
        folder,
    }
}

/// 打出了路径：只有它下面的文件与文件夹，文件夹在前。后面没有字时是这个文件夹自己的项，这个对话碰过的在前。
fn path_panel(under: String, q: String, s: &Sources<'_>) -> AtPanel {
    let l = labels(s.lang);
    let label = if q.is_empty() {
        under.clone()
    } else {
        (l.files_in)(&under)
    };
    let Some(f) = s.files.filter(|f| under.starts_with(&f.under)) else {
        return AtPanel {
            groups: vec![AtGroup::looking(&label)],
            under,
            q,
        };
    };
    let recent = recent_of(s);
    let below: Vec<(&str, bool)> = f
        .found
        .hits
        .iter()
        .filter(|h| h.path.starts_with(&under) && h.path.len() > under.len())
        .map(|h| (h.path.as_str(), h.folder))
        .collect();
    let items: Vec<(&str, bool)> = if q.is_empty() {
        let own = below.into_iter().filter(|(p, _)| {
            let rest = &p[under.len()..];
            !rest.strip_suffix('/').unwrap_or(rest).contains('/')
        });
        let (mut mine, rest): (Vec<_>, Vec<_>) = own.partition(|(p, _)| recent.contains_key(p));
        mine.sort_by_key(|(p, _)| recent.get(p).copied());
        mine.extend(rest);
        mine
    } else {
        let mut r: Vec<((&str, bool), usize, usize)> = below
            .into_iter()
            .enumerate()
            .map(|(i, h)| (h, rank_path(&h.0[under.len()..], &q), i))
            .filter(|(_, rk, _)| *rk < NO_MATCH)
            .collect();
        r.sort_by_key(|((p, folder), rk, i)| {
            (
                usize::from(!*folder),
                *rk,
                recent.get(p).copied().unwrap_or(usize::MAX),
                *i,
            )
        });
        r.into_iter().map(|(h, _, _)| h).collect()
    };
    // 还没有这个路径与词的结果：在等的时候说在找，而不是说没有。
    let waiting = items.is_empty() && !(f.under == under && f.query == q);
    let group = AtGroup {
        loading: waiting,
        ..AtGroup::of(
            &label,
            items
                .into_iter()
                .map(|(p, folder)| file(p, folder))
                .collect(),
        )
    };
    AtPanel {
        groups: vec![group],
        under,
        q,
    }
}

/// `q` 找到的会话（按标题，同文件名的 [`rank`]），好的在前；`q` 为空时全部（Puffin `ranked`）。
fn ranked(items: &[ConversationRef], q: &str) -> Vec<ConversationRef> {
    if q.is_empty() {
        return items.to_vec();
    }
    let mut r: Vec<(usize, usize, &ConversationRef)> = items
        .iter()
        .enumerate()
        .map(|(i, c)| (rank(&title_of(c), q), i, c))
        .filter(|(rk, _, _)| *rk < NO_MATCH)
        .collect();
    r.sort_by_key(|(rk, i, _)| (*rk, *i));
    r.into_iter().map(|(_, _, c)| c.clone()).collect()
}

/// 没有路径：文件、会话，按这个次序（Puffin `mixed_panel` 去掉子代理的一组）。
fn mixed_panel(q: String, s: &Sources<'_>, max_rows: usize) -> AtPanel {
    let l = labels(s.lang);
    let mut convs: Vec<ConversationRef> = s
        .conversations
        .into_iter()
        .flatten()
        .filter(|c| s.current != Some(c.session_id.as_str()))
        .cloned()
        .collect();
    convs.sort_by_key(|c| std::cmp::Reverse(c.updated_at));
    let files = s.files.filter(|f| f.under.is_empty());
    let recent = recent_of(s);
    if q.is_empty() {
        // 只有 @：你刚才在做的事（§4.2「分组（没有筛选词）」）。
        let mut recents: Vec<AtItem> = Vec::new();
        for p in s.touched.iter().map(|t| &t.0).chain(s.changed.iter()) {
            if recents.len() < 3 && !recents.contains(&file(p, false)) {
                recents.push(file(p, false));
            }
        }
        let indexed: Vec<AtItem> = files
            .map(|f| {
                f.found
                    .hits
                    .iter()
                    .filter(|h| !h.folder)
                    .take(3)
                    .map(|h| file(&h.path, false))
                    .collect()
            })
            .unwrap_or_default();
        let file_group = if !recents.is_empty() {
            vec![AtGroup::of(l.recent, recents)]
        } else if !indexed.is_empty() {
            vec![AtGroup::of(l.files, indexed)]
        } else {
            Vec::new()
        };
        let mut groups = Vec::new();
        if file_group.is_empty() && files.is_none() {
            groups.push(AtGroup::looking(l.files));
        }
        groups.extend(file_group);
        let talks: Vec<AtItem> = convs
            .into_iter()
            .take(2)
            .map(AtItem::Conversation)
            .collect();
        if !talks.is_empty() {
            groups.push(AtGroup::of(l.conversations, talks));
        }
        return AtPanel {
            groups,
            under: String::new(),
            q,
        };
    }
    let (file_items, file_total, waiting) = match files {
        None => (Vec::new(), 0, true),
        Some(f) => {
            let mut r: Vec<(usize, usize, usize, AtItem)> = f
                .found
                .hits
                .iter()
                .enumerate()
                .map(|(i, h)| (rank_path(&h.path, &q), i, h))
                .filter(|(rk, _, _)| *rk < NO_MATCH)
                .map(|(rk, i, h)| {
                    let near = recent.get(h.path.as_str()).copied().unwrap_or(usize::MAX);
                    (rk, near, i, file(&h.path, h.folder))
                })
                .collect();
            r.sort_by_key(|(rk, near, i, _)| (*rk, *near, *i));
            let items: Vec<AtItem> = r.into_iter().map(|t| t.3).collect();
            let exact = f.query == q;
            let total = items.len()
                + if exact {
                    (f.found.total as usize).saturating_sub(f.found.hits.len())
                } else {
                    0
                };
            let waiting = items.is_empty() && !exact;
            (items, total, waiting)
        }
    };
    let lists: [Vec<AtItem>; 2] = [
        file_items,
        ranked(&convs, &q)
            .into_iter()
            .map(AtItem::Conversation)
            .collect(),
    ];
    let totals = [file_total, lists[1].len()];
    let mut counts = [lists[0].len().min(4), lists[1].len().min(2)];
    let rows = |counts: &[usize; 2]| -> usize {
        (0..2)
            .map(|k| {
                if counts[k] == 0 {
                    0
                } else {
                    1 + counts[k] + usize::from(totals[k] > counts[k])
                }
            })
            .sum::<usize>()
            + if waiting { 2 } else { 0 }
    };
    // 太高时先减文件的行，再减会话，每组至少留一个（Puffin 先减文件，再减其他组里最多的那一组）。
    while rows(&counts) > max_rows {
        if counts[0] > 1 {
            counts[0] -= 1;
        } else if counts[1] > 1 {
            counts[1] -= 1;
        } else {
            break;
        }
    }
    let labels = [
        (l.files, l.files_word, l.file),
        (l.conversations, l.conversations_word, l.conversation),
    ];
    let mut groups = Vec::new();
    if waiting {
        groups.push(AtGroup::looking(l.files));
    }
    for (k, list) in lists.into_iter().enumerate() {
        if counts[k] == 0 {
            continue;
        }
        let more = totals[k] - counts[k];
        groups.push(AtGroup {
            label: labels[k].0.to_owned(),
            items: list.into_iter().take(counts[k]).collect(),
            more,
            more_word: if more == 1 { labels[k].2 } else { labels[k].1 }.to_owned(),
            loading: false,
        });
    }
    AtPanel {
        groups,
        under: String::new(),
        q,
    }
}

#[cfg(test)]
#[path = "at_panel_tests.rs"]
mod tests;
