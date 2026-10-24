//! `invoked_skills` 压缩附件（docs/specs/rust-cli/compact.md §3.4）：压缩之前调用过的 skill 的正文。
//!
//! 最近调用的在前；保留尾段里已有带回它的附件时不带；每个正文最多 5000 token（超过时截断、接说明），合计
//! 25000 token，放不下的跳过。

use rust_cli_kernel_api::compaction::rough_tokens;
use rust_cli_kernel_api::listing::InvokedSkill;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_prompts::{compact, texts};
use serde_json::{Value, json};

/// 来源（attachment type）。
pub(crate) const SOURCE: &str = "invoked_skills";
/// 一个正文的上限。
const MAX_SKILL_TOKENS: u64 = 5_000;
/// 合计的上限。
const MAX_TOTAL_TOKENS: u64 = 25_000;

/// 保留尾段里的 meta 消息的文字（各文字块以空行连接；有别的块时不算）。
fn meta_text(message: &Message) -> Option<String> {
    let Message::User(user) = message else {
        return None;
    };
    if !matches!(user.origin, UserOrigin::Meta { .. }) {
        return None;
    }
    let mut parts = Vec::new();
    for block in &user.content {
        match block {
            ContentBlock::Text { text } => parts.push(text.as_str()),
            _ => return None,
        }
    }
    Some(parts.join("\n\n"))
}

/// 保留尾段里已经有带回这段正文的附件（有原文的正文消息时不算）。
fn already_kept(kept: &[Message], content: &str) -> bool {
    let mut in_attachment = false;
    for message in kept.iter().rev() {
        if let Message::Attachment(attachment) = message {
            let listed = attachment.source == SOURCE
                && attachment.data["skills"]
                    .as_array()
                    .is_some_and(|skills| skills.iter().any(|skill| skill["content"] == content));
            in_attachment |= listed;
            continue;
        }
        if meta_text(message).as_deref() == Some(content) {
            return false;
        }
    }
    in_attachment
}

/// 超过上限时截到 `上限 × 4 − 说明的长度` 个 UTF-16 单元，接上说明。
fn truncated(content: &str) -> String {
    if rough_tokens(content) <= MAX_SKILL_TOKENS {
        return content.to_owned();
    }
    let note = compact::invoked_skill_truncated();
    let keep = (MAX_SKILL_TOKENS as usize * 4).saturating_sub(note.encode_utf16().count());
    let mut used = 0;
    let mut out = String::new();
    for character in content.chars() {
        if used + character.len_utf16() > keep {
            break;
        }
        used += character.len_utf16();
        out.push(character);
    }
    out.push_str(note);
    out
}

/// 压缩附件：没有可带回的 skill 时为 None。
pub(crate) fn announce(skills: Vec<InvokedSkill>, kept: &[Message]) -> Option<(String, Value)> {
    let mut total = 0;
    let mut shown = Vec::new();
    for skill in skills {
        if skill.content.is_empty() || already_kept(kept, &skill.content) {
            continue;
        }
        let content = truncated(&skill.content);
        let tokens = rough_tokens(&content);
        if total + tokens > MAX_TOTAL_TOKENS {
            continue;
        }
        total += tokens;
        shown.push(InvokedSkill { content, ..skill });
    }
    if shown.is_empty() {
        return None;
    }
    let bodies = shown
        .iter()
        .map(|skill| {
            texts::render(
                compact::invoked_skill(),
                &[
                    ("name", &skill.name),
                    ("path", &skill.path),
                    ("content", &skill.content),
                ],
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    let text = texts::render(compact::invoked_skills(), &[("skills", &bodies)]);
    Some((text, json!({ "skills": shown })))
}

#[cfg(test)]
#[path = "invoked_tests.rs"]
mod tests;
