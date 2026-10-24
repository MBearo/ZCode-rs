//! 调用过的 skill：名字从前一条消息取、路径按来源、最近的在前、之前压缩带回的也算。

use std::sync::Arc;

use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::listing::InvokedSkill;
use rust_cli_kernel_api::message::{
    AttachmentMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::testing::MemoryEnvironment;
use serde_json::json;

use super::{INVOKED_SKILLS_SOURCE, invoked};
use crate::skills::{SKILL_MESSAGE_SOURCE, SkillCatalog};
use crate::test_support::{CWD, facts, workspace};

fn user(text: &str, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("u"),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin,
    })
}

fn body(text: &str) -> Message {
    user(
        text,
        UserOrigin::Meta {
            source: SKILL_MESSAGE_SOURCE.to_owned(),
        },
    )
}

fn launched(name: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("r"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("t"),
            content: vec![ToolResultContent::Text {
                text: format!("Launching skill: {name}"),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("a"),
        },
    })
}

fn skill(name: &str, path: &str, content: &str) -> InvokedSkill {
    InvokedSkill {
        name: name.to_owned(),
        path: path.to_owned(),
        content: content.to_owned(),
    }
}

#[tokio::test]
async fn invoked_skills_are_named_by_the_call_and_listed_newest_first() {
    let env = MemoryEnvironment::new(facts()).with_file(
        &format!("{CWD}/.zcode/skills/deploy/SKILL.md"),
        "---\ndescription: Deploy\n---\nDeploy it.\n",
    );
    let env = Arc::new(env);
    let catalog = SkillCatalog::new(env.clone(), env);
    let earlier = Message::Attachment(AttachmentMessage {
        id: MessageId::new("x"),
        source: INVOKED_SKILLS_SOURCE.to_owned(),
        text: String::new(),
        data: json!({"skills": [
            {"name": "notes", "path": "userSettings:notes", "content": "NOTES"},
            {"name": "deploy", "path": "projectSettings:deploy", "content": "OLD DEPLOY"},
        ]}),
    });
    let history = vec![
        earlier,
        user(
            "<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>now</command-args>",
            UserOrigin::Human,
        ),
        body("REVIEW"),
        launched("deploy"),
        body("DEPLOY"),
        // 前面不是调用的 skill 正文不算。
        body("STRAY"),
    ];
    assert_eq!(
        invoked(&catalog, &workspace(), &history).await,
        [
            skill("deploy", "projectSettings:deploy", "DEPLOY"),
            skill("review", "review", "REVIEW"),
            skill("notes", "userSettings:notes", "NOTES"),
        ]
    );
}
