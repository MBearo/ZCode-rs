//! 这次会话调用过的 skill（docs/specs/rust-cli/compact.md §3.4）：压缩后带回它们的正文。
//!
//! 从历史推导：skill 正文是来源为 `skill` 的 meta user 消息，名字取它前面的那条——Skill 工具的结果
//! （`Launching skill: <名字>`）或用户输入的命令（`<command-name>/<名字></command-name>`）；之前压缩带回的
//! （`invoked_skills` attachment 的 `data.skills`）也算。最近调用的在前，同名只留最近一次。

use std::collections::HashSet;

use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::listing::InvokedSkill;
use rust_cli_kernel_api::message::{
    ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::user_command;

use super::{SKILL_MESSAGE_SOURCE, SkillCatalog, SkillSource};

/// 压缩后带回 skill 的 attachment type。
pub const INVOKED_SKILLS_SOURCE: &str = "invoked_skills";
/// Skill 工具结果的开头。
const LAUNCHING: &str = "Launching skill: ";

fn text(user: &UserMessage) -> String {
    user.content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// skill 正文前面那条消息给出的名字。
fn invoked_name(previous: &Message) -> Option<String> {
    let Message::User(user) = previous else {
        return None;
    };
    match user.origin {
        UserOrigin::ToolResult { .. } => user.content.iter().find_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => {
                content.iter().find_map(|item| match item {
                    ToolResultContent::Text { text } => {
                        text.strip_prefix(LAUNCHING).map(str::to_owned)
                    }
                    _ => None,
                })
            }
            _ => None,
        }),
        UserOrigin::Human => {
            let shown = user_command::display(&text(user))?;
            let command = shown.split_whitespace().next()?;
            command.strip_prefix('/').map(str::to_owned)
        }
        _ => None,
    }
}

/// 历史里的调用，按出现顺序：（名字，路径；None 时按目录现在的来源算，正文）。
fn calls(history: &[Message]) -> Vec<(String, Option<String>, String)> {
    let mut found = Vec::new();
    for (index, message) in history.iter().enumerate() {
        match message {
            Message::User(user) if matches!(&user.origin, UserOrigin::Meta { source } if source == SKILL_MESSAGE_SOURCE) =>
            {
                let Some(name) = index
                    .checked_sub(1)
                    .and_then(|previous| invoked_name(&history[previous]))
                else {
                    continue;
                };
                found.push((name, None, text(user)));
            }
            // 之前压缩带回的：列表里最近的在前，按出现顺序排时倒过来。
            Message::Attachment(attachment) if attachment.source == INVOKED_SKILLS_SOURCE => {
                let listed = attachment.data["skills"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                for skill in listed.iter().rev() {
                    let field = |key: &str| skill[key].as_str().map(str::to_owned);
                    if let (Some(name), Some(content)) = (field("name"), field("content")) {
                        found.push((name, field("path"), content));
                    }
                }
            }
            _ => {}
        }
    }
    found
}

/// skill 来源的名字（路径 `<来源>:<名字>` 的前半）。
fn source_label(source: &SkillSource) -> &'static str {
    match source {
        SkillSource::Managed => "policySettings",
        SkillSource::User => "userSettings",
        SkillSource::Project => "projectSettings",
        SkillSource::Plugin { .. } => "plugin",
        SkillSource::Builtin => "bundled",
    }
}

/// 调用过的 skill，最近的在前，同名只留最近一次。
pub(crate) async fn invoked(
    catalog: &SkillCatalog,
    workspace: &WorkspaceRef,
    history: &[Message],
) -> Vec<InvokedSkill> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (name, path, content) in calls(history).into_iter().rev() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let path = match path {
            Some(path) => path,
            None => match catalog.find(workspace, &name).await {
                Some(skill) => format!("{}:{name}", source_label(&skill.source)),
                None => name.clone(),
            },
        };
        out.push(InvokedSkill {
            name,
            path,
            content,
        });
    }
    out
}

#[cfg(test)]
#[path = "invoked_tests.rs"]
mod tests;
