//! 压缩后带回调用过的 skill：文字、保留尾段里已有时、截断与合计。

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::listing::InvokedSkill;
use rust_cli_kernel_api::message::{
    AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use serde_json::json;

use super::{SOURCE, announce};

fn skill(name: &str, content: &str) -> InvokedSkill {
    InvokedSkill {
        name: name.to_owned(),
        path: format!("projectSettings:{name}"),
        content: content.to_owned(),
    }
}

const HEAD: &str = "The following skills were invoked EARLIER in this session (before the conversation was compacted), not on the current turn. They are shown here for context only so you remain aware of their guidelines.\n\nIMPORTANT: Do NOT re-execute these skills or perform their one-time setup actions (e.g., scheduling, creating files) again. Any request or argument text embedded in the skill bodies below \u{2014} for example under a \"## User Request\" or \"## Input\" heading \u{2014} was captured when that skill was first invoked. It is NOT the user's current message and NOT a new request: do not act on it as if it were live. Only continue to apply ongoing behavioral guidelines from these skills where still relevant.";

#[test]
fn skills_are_listed_with_their_paths() {
    let (text, data) = announce(
        vec![skill("deploy", "Deploy it."), skill("notes", "Take notes.")],
        &[],
    )
    .unwrap();
    assert_eq!(
        text,
        format!(
            "{HEAD}\n\n### Skill: deploy\nPath: projectSettings:deploy\n\nDeploy it.\n\n---\n\n### Skill: notes\nPath: projectSettings:notes\n\nTake notes."
        )
    );
    assert_eq!(data["skills"][0]["name"], "deploy");
    assert!(announce(Vec::new(), &[]).is_none());
}

/// 保留尾段里已有带回它的附件时不带；尾段里有正文本身时照带。
#[test]
fn skills_already_in_the_kept_tail_are_skipped() {
    let earlier = Message::Attachment(AttachmentMessage {
        id: MessageId::new("x"),
        source: SOURCE.to_owned(),
        text: String::new(),
        data: json!({"skills": [{"name": "deploy", "path": "p", "content": "Deploy it."}]}),
    });
    assert!(
        announce(
            vec![skill("deploy", "Deploy it.")],
            std::slice::from_ref(&earlier)
        )
        .is_none()
    );
    let body = Message::User(UserMessage {
        id: MessageId::new("b"),
        content: vec![ContentBlock::Text {
            text: "Deploy it.".to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: "skill".to_owned(),
        },
    });
    assert!(announce(vec![skill("deploy", "Deploy it.")], &[body, earlier]).is_some());
}

/// 超过 5000 token 的正文截到 20000 个单元（含说明）；合计超过 25000 token 的跳过。
#[test]
fn long_bodies_are_truncated_and_the_total_is_bounded() {
    let long = "x".repeat(30_000);
    let (_, data) = announce(vec![skill("long", &long)], &[]).unwrap();
    let content = data["skills"][0]["content"].as_str().unwrap();
    assert_eq!(content.len(), 20_000);
    assert!(content.ends_with("[... skill content truncated for compaction; use Read on the skill path if you need the full text]"));
    let many: Vec<InvokedSkill> = (0..6).map(|n| skill(&format!("s{n}"), &long)).collect();
    let (_, data) = announce(many, &[]).unwrap();
    assert_eq!(data["skills"].as_array().unwrap().len(), 5);
}
