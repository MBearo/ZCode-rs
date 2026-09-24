//! The active conversation branch (Node `selectActiveConversationBranch`,
//! `activeSessionMessages`, `compact-session.ts`): append-only rewinds are
//! applied from `session.revert`, then the last compaction boundary.
use super::Record;
use serde_json::{Value, json};
use std::borrow::Cow;

/// The rewind cursor of `session.revert`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Branch {
    pub target: Option<String>,
    pub created: Option<String>,
    pub kept: Option<Vec<String>>,
    pub cut_after: Option<String>,
}

impl Branch {
    /// Node `resume.ts`: `targetMessageID`, `createdMessageID`, `keptMessageIDs`,
    /// `branchCutAfterMessageID`.
    pub fn from_revert(revert: Option<&Value>) -> Self {
        let Some(revert) = revert.filter(|r| r.is_object()) else {
            return Self::default();
        };
        let text = |key: &str| revert[key].as_str().map(str::to_owned);
        Self {
            target: text("targetMessageID"),
            created: text("createdMessageID"),
            kept: revert["keptMessageIDs"].as_array().map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_owned))
                    .collect()
            }),
            cut_after: text("branchCutAfterMessageID"),
        }
    }
}

fn position(messages: &[Cow<'_, Record>], id: &str) -> Option<usize> {
    messages.iter().position(|m| m.id() == id)
}

/// Node `selectActiveConversationBranch`.
pub fn select_branch<'a>(messages: Vec<Cow<'a, Record>>, branch: &Branch) -> Vec<Cow<'a, Record>> {
    let Some(target) = branch.target.as_deref() else {
        return messages;
    };
    let target_index = position(&messages, target);
    let kept: Vec<Cow<Record>> = match &branch.kept {
        Some(ids) => ids
            .iter()
            // Node 按 id 建 Map：重复 id 取最后一条。
            .filter_map(|id| messages.iter().rfind(|m| m.id() == id).cloned())
            .collect(),
        None => match target_index {
            Some(index) => messages[..index].to_vec(),
            None => messages.clone(),
        },
    };
    let after = |from: usize| messages[from..].iter().cloned();
    if let Some(cut) = branch.cut_after.as_deref() {
        return match position(&messages, cut) {
            Some(index) => kept.into_iter().chain(after(index + 1)).collect(),
            None => kept,
        };
    }
    if branch.kept.is_none() && target_index.is_none() {
        return messages;
    }
    let Some(created) = branch.created.as_deref() else {
        return kept;
    };
    match position(&messages, created) {
        Some(index) => kept.into_iter().chain(after(index)).collect(),
        None => kept,
    }
}

/// JavaScript truthiness of a JSON member (`undefined` when absent).
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0 && !v.is_nan()),
        Some(_) => true,
    }
}

/// Node `isActiveCompactionBoundaryPart`.
pub fn is_boundary_part(part: &Value) -> bool {
    part["type"] == "compaction"
        && (truthy(part.get("compactBoundary")) || !truthy(part.get("timelineStatus")))
}

fn has_boundary(record: &Record) -> bool {
    record.parts.iter().any(is_boundary_part)
}

/// Node `isCompactPreservableSessionMessage`.
fn preservable(record: &Record) -> bool {
    let info = &record.info;
    if info["semantics"]["providerVisibility"] == "hidden" {
        return false;
    }
    if record.parts.iter().any(|p| p["type"] == "compaction") {
        return false;
    }
    if info["role"] == "assistant" && truthy(info.get("error")) {
        return false;
    }
    matches!(info["role"].as_str(), Some("user" | "assistant"))
}

/// Node `invalidateRuntimeTokenUsage`: the preserved assistant's provider usage
/// belongs to the replaced prefix.
fn invalidated(record: &Record) -> Record {
    let mut copy = record.clone();
    if copy.info["role"] == "assistant"
        && let Some(tokens) = copy.info.get_mut("tokens").and_then(Value::as_object_mut)
    {
        for key in ["total", "input", "output", "reasoning"] {
            tokens.insert(key.into(), 0.into());
        }
        tokens.insert("cache".into(), json!({"read": 0, "write": 0}));
    }
    copy
}

fn boundary_of(record: &Record) -> Option<&Value> {
    record
        .parts
        .iter()
        .find(|p| p["type"] == "compaction" && truthy(p.get("compactBoundary")))
        .map(|p| &p["compactBoundary"])
}

/// Node `compactActiveSessionMessages`.
fn compact_active<'a>(
    messages: &[Cow<'a, Record>],
    boundary_index: usize,
    include_preserved: bool,
) -> Vec<Cow<'a, Record>> {
    let active = messages[boundary_index..].to_vec();
    if !include_preserved {
        return active;
    }
    let Some(segment) = boundary_of(&messages[boundary_index])
        .map(|b| &b["preservedSegment"])
        .filter(|s| s.is_object())
    else {
        return active;
    };
    let find = |key: &str| segment[key].as_str().and_then(|id| position(messages, id));
    let preserved: Vec<Cow<Record>> = match (find("headMessageId"), find("tailMessageId")) {
        (Some(head), Some(tail)) if tail >= head && tail < boundary_index => messages[head..=tail]
            .iter()
            .filter(|m| preservable(m))
            .map(|m| Cow::Owned(invalidated(m)))
            .collect(),
        _ => vec![],
    };
    if preserved.is_empty() {
        return active;
    }
    let insert = segment["anchorMessageId"]
        .as_str()
        .and_then(|id| position(&active, id))
        .map_or(1, |index| index + 1)
        .min(active.len());
    let mut out = active[..insert].to_vec();
    out.extend(preserved);
    out.extend_from_slice(&active[insert..]);
    out
}

fn last_boundary(messages: &[Cow<'_, Record>]) -> Option<usize> {
    messages.iter().rposition(|m| has_boundary(m))
}

/// Node `activeSessionMessages`.
pub fn active_messages<'a>(
    messages: &'a [Record],
    branch: &Branch,
    include_preserved: bool,
) -> Vec<Cow<'a, Record>> {
    let all: Vec<Cow<Record>> = messages.iter().map(Cow::Borrowed).collect();
    if branch.cut_after.is_none() {
        // 旧数据没有 branch cut：先按压缩边界裁剪，再选活动分支（与 Node 兼容语义一致）。
        let compact = last_boundary(&all);
        let active = match compact {
            Some(index) => compact_active(&all, index, include_preserved),
            None => all.clone(),
        };
        if let (Some(kept), Some(index)) = (&branch.kept, compact) {
            let after: Vec<&str> = all[index..].iter().map(|m| m.id()).collect();
            if !kept.iter().any(|id| after.contains(&id.as_str())) {
                return active;
            }
        }
        return select_branch(active, branch);
    }
    let active = select_branch(all, branch);
    match last_boundary(&active) {
        Some(index) => compact_active(&active, index, include_preserved),
        None => active,
    }
}
