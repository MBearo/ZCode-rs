//! Edit semantics (Node `core/src/tool/handlers/edit.ts`): Node's failure
//! codes and texts, the matching fallbacks, replacement normalization and the
//! model-visible result. File IO stays in `tool_files`.
use crate::domain::edit_match::{self, Match};
use std::path::Path;

/// Node `EditErrorCode`.
pub(super) mod code {
    pub const NO_CHANGE: u32 = 1;
    pub const FILE_EXISTS_NO_OLD_STRING: u32 = 3;
    pub const FILE_NOT_EXIST: u32 = 4;
    pub const NOTEBOOK_FILE: u32 = 5;
    pub const FILE_NOT_READ: u32 = 6;
    pub const STALE_FILE: u32 = 7;
    pub const OLD_STRING_NOT_FOUND: u32 = 8;
    pub const AMBIGUOUS_REPLACE: u32 = 9;
    pub const INVALID_PATH: u32 = 13;
}

pub(super) const NOT_READ: &str = "File has not been read yet. Read it first before writing to it.";
pub(super) const STALE: &str = "File has been modified since read, either by the user or by a linter. Read it again before attempting to write it.";
pub(super) const FRESHNESS_SUFFIX: &str =
    " (file state is current in your context — no need to Read it back)";
const NON_UNIQUE: &str = "old_string is not unique in the file. Provide more surrounding context or set replace_all to true.";

/// Node `ToolHandlerFailure` with an `EditErrorCode`.
pub(super) fn failure(code: u32, message: impl Into<String>) -> anyhow::Error {
    crate::contract::ToolError::handler(code, message)
}

/// The resolved replacement of one Edit call.
pub(super) struct Planned {
    pub content: String,
    pub actual_old: String,
    pub actual_new: String,
    pub strategy: &'static str,
    pub candidates: usize,
}

/// Node's matching and replacement over `\n`-normalized `content`; `raw_old`
/// is the unnormalized `old_string` quoted in failure texts.
pub(super) fn plan(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
    raw_old: &str,
) -> anyhow::Result<Planned> {
    let (actual_old, strategy, candidates) = match edit_match::find(content, old, replace_all) {
        Match::NotFound => {
            return Err(failure(
                code::OLD_STRING_NOT_FOUND,
                format!("String to replace not found in file.\nString: {raw_old}"),
            ));
        }
        Match::Ambiguous { candidates, .. } => {
            return Err(failure(
                code::AMBIGUOUS_REPLACE,
                ambiguous(candidates, raw_old),
            ));
        }
        Match::Matched {
            actual,
            strategy,
            candidates,
        } => (actual, strategy, candidates),
    };
    let occurrences = content.matches(actual_old.as_str()).count();
    if !replace_all && occurrences > 1 {
        return Err(failure(
            code::AMBIGUOUS_REPLACE,
            ambiguous(occurrences, raw_old),
        ));
    }
    let normalized = edit_match::normalize_replacement(strategy, new);
    let actual_new = edit_match::preserve_quote_style(old, &actual_old, &normalized);
    let content = apply(content, &actual_old, &actual_new, replace_all);
    Ok(Planned {
        content,
        actual_old,
        actual_new,
        strategy: strategy.as_str(),
        candidates,
    })
}

fn ambiguous(count: usize, raw_old: &str) -> String {
    if count == 0 {
        return NON_UNIQUE.into();
    }
    format!(
        "Found {count} matches of the string to replace, but replace_all is false. To replace all occurrences, set replace_all to true. To replace only one occurrence, please provide more context to uniquely identify the instance.\nString: {raw_old}"
    )
}

/// Node `applyEditToContent`: deleting text also deletes its trailing newline.
fn apply(content: &str, old: &str, new: &str, replace_all: bool) -> String {
    let with_newline = format!("{old}\n");
    let search = if new.is_empty() && !old.ends_with('\n') && content.contains(&with_newline) {
        with_newline.as_str()
    } else {
        old
    };
    if replace_all {
        content.replace(search, new)
    } else {
        content.replacen(search, new, 1)
    }
}

/// Node `formatEditModelContent` (`file_path` as the model gave it).
pub(super) fn model_content(file_path: &str, replace_all: bool) -> String {
    if replace_all {
        format!(
            "The file {file_path} has been updated. All occurrences were successfully replaced.{FRESHNESS_SUFFIX}"
        )
    } else {
        format!("The file {file_path} has been updated successfully.{FRESHNESS_SUFFIX}")
    }
}

/// Node `detectLineEndings`: CRLF only when most line breaks are CRLF.
pub(super) fn crlf(raw: &str) -> bool {
    let crlf = raw.matches("\r\n").count();
    let lf = raw.matches('\n').count() - crlf;
    crlf > lf
}

/// Node `createMissingEditFileMessage` with `findSimilarFilename`.
pub(super) async fn missing_message(path: &Path, cwd: &Path) -> String {
    let mut message = format!(
        "File does not exist. Note: your current working directory is {}.",
        cwd.display()
    );
    if let Some(suggestion) = similar(path).await {
        message.push_str(&format!(" Did you mean {suggestion}?"));
    }
    message
}

async fn similar(path: &Path) -> Option<String> {
    let parent = path.parent()?;
    let target = path.file_name()?.to_str()?;
    let stem = |name: &str| {
        Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
    };
    let mut entries = vec![];
    let mut reader = tokio::fs::read_dir(parent).await.ok()?;
    while let Ok(Some(entry)) = reader.next_entry().await {
        let kind = entry.file_type().await.ok()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if (kind.is_file() || kind.is_symlink()) && name != target {
            entries.push(name);
        }
    }
    entries.sort();
    let target_stem = stem(target);
    if let Some(same) = entries.iter().find(|name| stem(name) == target_stem) {
        return Some(same.clone());
    }
    entries.into_iter().find(|name| distance(name, target) <= 3)
}

/// Levenshtein distance over UTF-16 code units (JS string indexing).
fn distance(left: &str, right: &str) -> usize {
    let (a, b): (Vec<u16>, Vec<u16>) = (
        left.encode_utf16().collect(),
        right.encode_utf16().collect(),
    );
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut current = vec![i + 1; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            current[j + 1] = (current[j] + 1)
                .min(previous[j + 1] + 1)
                .min(previous[j] + usize::from(x != y));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
#[path = "tool_edit_tests.rs"]
mod tests;
