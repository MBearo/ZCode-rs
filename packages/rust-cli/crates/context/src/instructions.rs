//! `instructions` attachment（context.md §11.5）：会话开始时的规则文件，之后只宣布差异。
//!
//! 加载结果按会话缓存（[`Memo`]）：规则文件在会话里缓存，压缩后重读。缓存只是
//! 读取缓存；“宣布过什么”的唯一来源是历史里已落库的 `instructions`。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::Message;
use rust_cli_prompts::texts::{self, render};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::render::attachments;
use crate::rules::RuleFile;

pub(crate) const SOURCE: &str = "instructions";

/// 重读原因（记录里的 `refreshReason`；本工程只会产生这两种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    SessionStart,
    Compaction,
}

impl Reason {
    fn name(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::Compaction => "compaction",
        }
    }

    fn phrase(self) -> &'static str {
        match self {
            Self::SessionStart => texts::refresh_session_start(),
            Self::Compaction => texts::refresh_compaction(),
        }
    }
}

/// 宣布出去的一个文件（内容去首尾空白）。`kind` 是记录里的 `type`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Announced {
    pub path: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub content: String,
}

#[derive(Deserialize)]
struct Data {
    files: Vec<Announced>,
    #[serde(default)]
    removed: Vec<String>,
    #[serde(default)]
    changed: bool,
}

/// 当前要宣布的文件：启动时加载的无条件规则。
pub(crate) fn current(files: &[RuleFile]) -> Vec<Announced> {
    files
        .iter()
        .map(|file| Announced {
            path: file.path.clone(),
            kind: file.kind.name().to_owned(),
            content: file.content.trim().to_owned(),
        })
        .collect()
}

/// 已宣布的 `path → 文件`：最后一条不带 `changed` 的作为基准，再叠加其后的增量。
/// 顺序同 JS `Map`：覆盖不改位置，删除即移除。没有基准时为 None。
pub(crate) fn announced(history: &[Message]) -> Option<Vec<Announced>> {
    let parsed: Vec<Data> = attachments(history, SOURCE)
        .filter_map(|attachment| serde_json::from_value(attachment.data.clone()).ok())
        .collect();
    let base = parsed.iter().rposition(|data| !data.changed)?;
    let mut map: Vec<Announced> = Vec::new();
    for data in &parsed[base..] {
        for file in &data.files {
            match map.iter_mut().find(|known| known.path == file.path) {
                Some(known) => *known = file.clone(),
                None => map.push(file.clone()),
            }
        }
        map.retain(|known| !data.removed.contains(&known.path));
    }
    Some(map)
}

fn label(kind: &str) -> &'static str {
    match kind {
        "Managed" => texts::instructions_managed(),
        "User" => texts::instructions_user(),
        "Project" => texts::instructions_project(),
        "Local" => texts::instructions_local(),
        "AutoMem" => texts::instructions_automem(),
        _ => "",
    }
}

/// `Contents of <path><label>:\n\n<content>`（`nested_memory` 用空 label、不 trim 的内容）。
pub(crate) fn entry(path: &str, label: &str, content: &str) -> String {
    render(
        texts::instructions_file(),
        &[("path", path), ("label", label), ("content", content)],
    )
}

/// 每个文件一段，空行分隔。
fn entries(files: &[Announced]) -> String {
    files
        .iter()
        .map(|file| entry(&file.path, label(&file.kind), file.content.trim()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 一组规则文件按 `instructions` 的格式（说明加每个文件）；没有文件时为 None（`/cd`，tui-manage.md
/// §7.1）。
pub(crate) fn listed(files: &[RuleFile]) -> Option<String> {
    let current = current(files);
    (!current.is_empty()).then(|| render(texts::instructions(), &[("files", &entries(&current))]))
}

/// 增量的 `changed` 文字（本工程的增量总带原因）。
fn changed_text(files: &[Announced], removed: &[String], reason: Reason) -> String {
    let listed = entries(files);
    let when = reason.phrase();
    let mut parts = Vec::new();
    if !listed.is_empty() {
        let lead = render(texts::instructions_reread(), &[("when", when)]);
        parts.push(render(
            texts::instructions_changed(),
            &[("lead", &lead), ("files", &listed)],
        ));
    }
    if !removed.is_empty() && listed.is_empty() {
        parts.push(render(texts::instructions_reread_only(), &[("when", when)]));
    }
    parts.extend(
        removed
            .iter()
            .map(|path| render(texts::instructions_removed(), &[("path", path)])),
    );
    parts.join("\n\n")
}

/// 这一步的 `instructions`：`(text, data)`，text 未包 `<system-reminder>`。
pub(crate) fn announce(
    history: &[Message],
    files: &[RuleFile],
    reason: Reason,
) -> Option<(String, Value)> {
    let current = current(files);
    let Some(known) = announced(history) else {
        if current.is_empty() {
            return None;
        }
        let text = render(texts::instructions(), &[("files", &entries(&current))]);
        return Some((text, json!({ "files": current })));
    };
    let files: Vec<Announced> = current
        .iter()
        .filter(|file| {
            !known.iter().any(|old| {
                old.path == file.path && old.content == file.content && old.kind == file.kind
            })
        })
        .cloned()
        .collect();
    let removed: Vec<String> = known
        .iter()
        .filter(|old| !current.iter().any(|file| file.path == old.path))
        .map(|old| old.path.clone())
        .collect();
    if files.is_empty() && removed.is_empty() {
        return None;
    }
    let text = changed_text(&files, &removed, reason);
    let mut data = json!({ "files": files });
    if !removed.is_empty() {
        data["removed"] = json!(removed);
    }
    data["changed"] = json!(true);
    data["reason"] = json!(reason.name());
    Some((text, data))
}

#[derive(Clone)]
struct Entry {
    boundary: Option<MessageId>,
    files: Arc<Vec<RuleFile>>,
    reason: Reason,
}

/// 规则文件的读取缓存：每个会话一条，键是会话 ID 加最近一次压缩边界的 ID。
#[derive(Default)]
pub(crate) struct Memo(Mutex<HashMap<SessionId, Entry>>);

pub(crate) enum Lookup {
    Hit(Arc<Vec<RuleFile>>, Reason),
    /// 要重读；带上这次重读的原因：同一进程里换了压缩边界为 `compaction`，否则为 `session_start`。
    Miss(Reason),
}

impl Memo {
    pub(crate) fn lookup(&self, session: &SessionId, boundary: Option<&MessageId>) -> Lookup {
        let entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match entries.get(session) {
            Some(entry) if entry.boundary.as_ref() == boundary => {
                Lookup::Hit(entry.files.clone(), entry.reason)
            }
            Some(_) => Lookup::Miss(Reason::Compaction),
            None => Lookup::Miss(Reason::SessionStart),
        }
    }

    pub(crate) fn store(
        &self,
        session: SessionId,
        boundary: Option<MessageId>,
        files: Arc<Vec<RuleFile>>,
        reason: Reason,
    ) {
        let entry = Entry {
            boundary,
            files,
            reason,
        };
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session, entry);
    }

    /// 会话关闭：丢掉它的缓存（再打开时按 `session_start` 重读，同 `/clear` 与 resume）。
    pub(crate) fn forget(&self, session: &SessionId) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session);
    }
}

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod tests;
