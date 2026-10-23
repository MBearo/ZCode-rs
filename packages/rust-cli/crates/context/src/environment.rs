//! 环境段 attachment（`environment`：先决定何时发，再渲染整块或更新）。
//!
//! 压缩边界之后第一次、或平台 / shell / 系统版本变了时发整块；否则只在 cwd、git、附加目录变化时
//! 发 `# Environment update`。快照存在 attachment 的 `data.snapshot` 里，resume 后据此算增量。

use rust_cli_kernel_api::env::EnvFacts;
use rust_cli_kernel_api::message::{AttachmentMessage, Message};
use rust_cli_prompts::texts::{self, render};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::render::{Bullet, attachments, bullets, escape};

pub(crate) const SOURCE: &str = "environment";

/// 环境快照（字段名与落库记录里的相同）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub working_directory: String,
    pub is_git_repo: bool,
    #[serde(default)]
    pub additional_working_directories: Vec<String>,
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
}

impl Snapshot {
    pub(crate) fn from_facts(facts: &EnvFacts) -> Self {
        let present = |value: &str| (!value.is_empty()).then(|| value.to_owned());
        Self {
            working_directory: facts.cwd.as_str().to_owned(),
            is_git_repo: facts.is_git_repo,
            additional_working_directories: facts
                .additional_directories
                .iter()
                .map(|path| path.as_str().to_owned())
                .collect(),
            platform: facts.platform.clone(),
            shell: present(&facts.shell),
            os_version: present(&facts.os_version),
        }
    }

    /// 平台、shell、系统版本都没变；变了就要重发整块。
    fn same_host(&self, other: &Self) -> bool {
        self.platform == other.platform
            && self.shell == other.shell
            && self.os_version == other.os_version
    }
}

/// 整块。
fn whole(snapshot: &Snapshot) -> String {
    let value = |template: &str, value: &str| render(template, &[("value", value)]);
    let mut lines = vec![
        Bullet::Line(value(
            texts::environment_cwd(),
            &escape(&snapshot.working_directory),
        )),
        Bullet::Line(value(
            texts::environment_git(),
            &snapshot.is_git_repo.to_string(),
        )),
    ];
    if !snapshot.additional_working_directories.is_empty() {
        lines.push(Bullet::Line(texts::environment_additional().to_owned()));
        lines.push(Bullet::Nested(
            snapshot
                .additional_working_directories
                .iter()
                .map(|path| escape(path))
                .collect(),
        ));
    }
    lines.push(Bullet::Line(value(
        texts::environment_platform(),
        &escape(&snapshot.platform),
    )));
    if let Some(shell) = &snapshot.shell {
        lines.push(Bullet::Line(value(
            texts::environment_shell(),
            &escape(shell),
        )));
    }
    if let Some(os) = &snapshot.os_version {
        lines.push(Bullet::Line(value(texts::environment_os(), &escape(os))));
    }
    lines.push(Bullet::Line(texts::environment_untrusted().to_owned()));
    render(texts::environment(), &[("lines", &bullets(lines))])
}

/// `# Environment update`（先算变化，再渲染）；没有变化时为 None。
fn update(latest: &Snapshot, current: &Snapshot) -> Option<(String, Vec<Value>)> {
    let mut lines = Vec::new();
    let mut changes = Vec::new();
    if latest.working_directory != current.working_directory {
        lines.push(Bullet::Line(render(
            texts::environment_cwd_was(),
            &[
                ("value", &escape(&current.working_directory)),
                ("from", &escape(&latest.working_directory)),
            ],
        )));
        changes.push(json!({"field": "workingDirectory", "from": latest.working_directory}));
    }
    if latest.is_git_repo != current.is_git_repo {
        lines.push(Bullet::Line(render(
            texts::environment_git_was(),
            &[
                ("value", &current.is_git_repo.to_string()),
                ("from", &latest.is_git_repo.to_string()),
            ],
        )));
        changes.push(json!({"field": "isGitRepo"}));
    }
    let before = &latest.additional_working_directories;
    let after = &current.additional_working_directories;
    let added: Vec<String> = after
        .iter()
        .filter(|path| !before.contains(path))
        .cloned()
        .collect();
    let removed: Vec<String> = before
        .iter()
        .filter(|path| !after.contains(path))
        .cloned()
        .collect();
    if !added.is_empty() || !removed.is_empty() {
        for (label, paths) in [
            (texts::environment_additional_added(), &added),
            (texts::environment_additional_removed(), &removed),
        ] {
            if !paths.is_empty() {
                lines.push(Bullet::Line(label.to_owned()));
                lines.push(Bullet::Nested(
                    paths.iter().map(|path| escape(path)).collect(),
                ));
            }
        }
        changes.push(json!({
            "field": "additionalWorkingDirectories", "added": added, "removed": removed
        }));
    }
    if lines.is_empty() {
        return None;
    }
    Some((
        render(texts::environment_update(), &[("lines", &bullets(lines))]),
        changes,
    ))
}

fn snapshot_of(attachment: &AttachmentMessage) -> Option<Snapshot> {
    serde_json::from_value(attachment.data.get("snapshot")?.clone()).ok()
}

/// 这一步的环境 attachment：`(text, data)`，text 未包 `<system-reminder>`。
pub(crate) fn announce(history: &[Message], facts: &EnvFacts) -> Option<(String, Value)> {
    let current = Snapshot::from_facts(facts);
    let announced: Vec<&AttachmentMessage> = attachments(history, SOURCE).collect();
    let latest = announced
        .last()
        .and_then(|attachment| snapshot_of(attachment));
    let has_whole = announced
        .iter()
        .any(|attachment| attachment.data.get("changes").is_none());
    let snapshot = json!(current);
    match latest {
        Some(latest) if has_whole && latest.same_host(&current) => {
            let (text, changes) = update(&latest, &current)?;
            Some((text, json!({"snapshot": snapshot, "changes": changes})))
        }
        _ => Some((whole(&current), json!({"snapshot": snapshot}))),
    }
}

#[cfg(test)]
#[path = "environment_tests.rs"]
mod tests;
