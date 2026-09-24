//! Prompt-too-long handling of the compaction summary request and the files
//! re-attached after it (Node `compact-selection.ts`, `compact-post-reminders.ts`).
//! Spec rust-m7-compact §6.
use super::compact::{Plan, assistant_roles, group_starts, plan};
use super::context::{estimate, with_summary};
use regex::Regex;
use serde_json::{Value, json};
use std::sync::OnceLock;

/// Node `MAX_COMPACT_PROMPT_TOO_LONG_RETRIES`.
pub const MAX_RETRIES: u32 = 3;
/// Node `COMPACT_PROMPT_TOO_LONG_RETRY_MARKER`.
pub const MARKER: &str = "[earlier conversation truncated for compaction retry]";
const MAX_FILES: usize = 5;
const MAX_FILE_TOKENS: usize = 5_000;
const MAX_TOTAL_TOKENS: usize = 50_000;

/// Node `parsePromptTooLongTokenGap`: `N tokens > M` in a provider message.
pub fn token_gap(message: &str) -> Option<u64> {
    static GAP: OnceLock<Regex> = OnceLock::new();
    let gap = GAP.get_or_init(|| Regex::new(r"(?i)(\d[\d,]*)\s*tokens?\s*>\s*(\d[\d,]*)").unwrap());
    let found = gap.captures(message)?;
    let number = |i: usize| found[i].replace(',', "").parse::<u64>().ok();
    let (actual, limit) = (number(1)?, number(2)?);
    (actual > limit).then(|| actual - limit)
}

/// Token estimates of the assistant-started groups of the summary and `messages`.
fn group_tokens(messages: &[Value], summary: Option<&str>) -> Vec<u64> {
    let list = with_summary(summary, messages);
    let roles = assistant_roles(&list, false);
    let starts = group_starts(&roles);
    starts
        .iter()
        .enumerate()
        .map(|(i, start)| {
            let end = starts.get(i + 1).copied().unwrap_or(list.len());
            estimate(&list[*start..end]) as u64
        })
        .collect()
}

/// Node `countRecentGroupsToCoverTokenGap`.
fn cover(estimates: &[u64], count: usize, gap: u64) -> usize {
    if gap == 0 || count == 0 {
        return 0;
    }
    let (mut tokens, mut groups) = (0, 0);
    for index in (0..count).rev() {
        tokens += estimates.get(index).copied().unwrap_or(0);
        groups += 1;
        if tokens >= gap {
            break;
        }
    }
    if groups + 1 >= count {
        (count / 2).max(1)
    } else {
        groups.max(1)
    }
}

/// Node `selectCompactEntriesAfterPromptTooLong` (automatic compaction):
/// more recent groups move into the preserved part.
pub fn reselect(
    messages: &[Value],
    summary: Option<&str>,
    current: usize,
    gap: Option<u64>,
) -> Option<Plan> {
    let estimates = group_tokens(messages, summary);
    let total = estimates.len();
    if total < 2 || current >= total - 1 {
        return None;
    }
    let summarized = total - current;
    if summarized < 2 {
        return None;
    }
    let moves = gap.map_or(1, |gap| cover(&estimates, summarized, gap));
    let next = (current + moves).min(total - 1);
    if next <= current {
        return None;
    }
    plan(messages, summary.is_some(), false, next)
}

/// Node `selectCompactEntriesForInitialPromptTooLong` (reactive compaction).
pub fn initial(messages: &[Value], summary: Option<&str>, gap: Option<u64>) -> Option<Plan> {
    let estimates = group_tokens(messages, summary);
    let total = estimates.len();
    let gap = gap?;
    if total <= 3 {
        return None;
    }
    let remaining = gap.checked_sub(*estimates.last()?).filter(|r| *r > 0)?;
    let additional = cover(&estimates, total - 1, remaining);
    plan(messages, summary.is_some(), false, 1 + additional)
}

/// Node `truncateRuntimeEntriesForCompactRetry` (manual compaction): the
/// summarized list without its oldest groups, marked when it starts mid-round.
pub fn truncate(list: &[Value], gap: Option<u64>) -> Option<Vec<Value>> {
    let candidates = match list.first() {
        Some(first) if first["role"] == "user" && first["content"] == MARKER => &list[1..],
        _ => list,
    };
    let roles = assistant_roles(candidates, false);
    let starts = group_starts(&roles);
    if starts.len() < 2 {
        return None;
    }
    let mut drop = match gap {
        Some(gap) => {
            let (mut dropped, mut count) = (0u64, 0);
            for (i, start) in starts.iter().enumerate() {
                let end = starts.get(i + 1).copied().unwrap_or(candidates.len());
                dropped += estimate(&candidates[*start..end]) as u64;
                count += 1;
                if dropped >= gap {
                    break;
                }
            }
            count
        }
        None => (starts.len() / 5).max(1),
    };
    drop = drop.min(starts.len() - 1);
    if drop < 1 {
        return None;
    }
    let sliced = &candidates[starts[drop]..];
    let mut out = Vec::with_capacity(sliced.len() + 1);
    if sliced.first().is_some_and(|m| m["role"] == "assistant") {
        out.push(json!({"role": "user", "content": MARKER}));
    }
    out.extend_from_slice(sliced);
    Some(out)
}

/// One text Read of the session, newest first (Node `ReadFileStateEntry`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadView {
    pub path: String,
    pub content: String,
    pub offset: Option<u64>,
    pub limit: Option<u64>,
}

/// `file_path` of the Read calls in `messages` (the preserved part).
pub fn read_paths(messages: &[Value]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .filter(|call| call["function"]["name"] == "Read")
        .filter_map(|call| {
            let args = call["function"]["arguments"].as_str()?;
            let input: Value = serde_json::from_str(args).ok()?;
            input["file_path"]
                .as_str()
                .filter(|p| !p.is_empty())
                .map(normalized)
        })
        .collect()
}

fn normalized(path: &str) -> String {
    path.replace('\\', "/")
}

/// Node `buildPostCompactReadStateReminderEntries`: reminder bodies.
pub fn read_reminders(views: &[ReadView], preserved: &[String]) -> Vec<String> {
    let mut selected = vec![];
    let mut total = 0;
    let candidates = views.iter().filter(|view| {
        let path = normalized(&view.path);
        !path.contains("/.git/") && !preserved.contains(&path)
    });
    for view in candidates {
        if selected.len() >= MAX_FILES {
            break;
        }
        let tokens = view.content.encode_utf16().count().div_ceil(3);
        if tokens > MAX_FILE_TOKENS || total + tokens > MAX_TOTAL_TOKENS {
            selected.push(format!("Note: {} was read before the last conversation was summarized, but the contents are too large to include. Use Read tool if you need to access it.", view.path));
            continue;
        }
        total += tokens;
        let start = match view.offset {
            Some(0) => 0,
            Some(offset) if offset > 1 => offset,
            _ => 1,
        };
        let content = view
            .content
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .enumerate()
            .map(|(i, line)| format!("{}\t{line}", i as u64 + start))
            .collect::<Vec<_>>()
            .join("\n");
        // Node JSON.stringify 按插入顺序：file_path、offset、limit。
        let mut input = format!("{{\"file_path\":{}", Value::from(view.path.as_str()));
        for (key, value) in [("offset", view.offset), ("limit", view.limit)] {
            if let Some(value) = value {
                input.push_str(&format!(",\"{key}\":{value}"));
            }
        }
        input.push('}');
        selected.push(format!(
            "Called the Read tool with the following input: {input}\nResult of calling the Read tool:\n{content}"
        ));
    }
    selected
}

#[cfg(test)]
#[path = "compact_ptl_tests.rs"]
mod tests;
