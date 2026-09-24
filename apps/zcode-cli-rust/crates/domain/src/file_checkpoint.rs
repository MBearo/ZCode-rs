use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCheckpoint {
    pub id: String,
    pub path: String,
    pub tool: String,
    pub before: Option<String>,
    pub after: String,
    pub mode: Option<u32>,
    pub row: u64,
    #[serde(default)]
    pub restored: bool,
    /// The Node checkpoint recording this change (spec rust-m11-node-storage §5.5).
    #[serde(default)]
    pub node: Option<NodeCheckpoint>,
}

/// A Node `CheckpointCreated`: its id, the turn's user message and the
/// before-change artifact.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeCheckpoint {
    pub checkpoint: String,
    pub message: String,
    pub snapshot: String,
}

/// A stored Node checkpoint read back for a resumed session: the tool call it
/// belongs to and the file contents before and after it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportedCheckpoint {
    pub node: NodeCheckpoint,
    pub call: String,
    pub tool: String,
    pub path: String,
    pub before: Option<String>,
    pub after: String,
    pub restored: bool,
}

/// Applies structured patch hunks (`{oldStart, oldLines, newStart, newLines,
/// lines}`: jsdiff `structuredPatch`, or Rust's single context-free hunk) to
/// `before`; `None` when a context or removed line does not match.
pub fn apply_patch(before: &str, hunks: &[serde_json::Value]) -> Option<String> {
    let body = before.strip_suffix('\n').unwrap_or(before);
    let mut lines: Vec<String> = if before.is_empty() {
        vec![]
    } else {
        body.split('\n').map(str::to_owned).collect()
    };
    let mut newline = before.ends_with('\n');
    let mut offset: i64 = 0;
    for hunk in hunks {
        let old_start = hunk["oldStart"].as_i64()?;
        let start = usize::try_from((old_start - 1).max(0) + offset).ok()?;
        let (mut old, mut new) = (vec![], vec![]);
        let mut previous = ' ';
        for line in hunk["lines"].as_array()? {
            let line = line.as_str()?;
            let (mark, text) = line.split_at(line.len().min(1));
            match mark {
                " " => {
                    old.push(text.to_owned());
                    new.push(text.to_owned());
                }
                "-" => old.push(text.to_owned()),
                "+" => new.push(text.to_owned()),
                // jsdiff「\ No newline at end of file」：标记所跟的那一侧末尾没有换行。
                "\\" => {
                    newline = previous == '-';
                    continue;
                }
                _ => return None,
            }
            previous = mark.chars().next().unwrap_or(' ');
        }
        if lines.get(start..start + old.len())? != old.as_slice() {
            return None;
        }
        offset += new.len() as i64 - old.len() as i64;
        lines.splice(start..start + old.len(), new);
    }
    let mut out = lines.join("\n");
    if newline && !lines.is_empty() {
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patches_apply_like_jsdiff() {
        let hunks = [
            json!({"oldStart": 2, "oldLines": 3, "newStart": 2, "newLines": 3,
            "lines": [" b", "-c", "+C", " d"]}),
        ];
        assert_eq!(
            apply_patch("a\nb\nc\nd\n", &hunks).as_deref(),
            Some("a\nb\nC\nd\n")
        );
        assert_eq!(apply_patch("a\nb\nx\nd\n", &hunks), None);
        let insert = [
            json!({"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 2,
            "lines": [" a", "+b"]}),
        ];
        assert_eq!(apply_patch("a\n", &insert).as_deref(), Some("a\nb\n"));
        // Rust 自己的补丁：无上下文，纯插入的 oldStart 是插入位置的下一行。
        let rust = [
            json!({"oldStart": 2, "oldLines": 0, "newStart": 2, "newLines": 1,
            "lines": ["+x"]}),
        ];
        assert_eq!(apply_patch("a\nb\n", &rust).as_deref(), Some("a\nx\nb\n"));
        let tail = [
            json!({"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1,
            "lines": ["-a", "\\ No newline at end of file", "+b"]}),
        ];
        assert_eq!(apply_patch("a", &tail).as_deref(), Some("b\n"));
    }
}
