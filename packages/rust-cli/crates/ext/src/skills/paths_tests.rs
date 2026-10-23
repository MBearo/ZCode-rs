//! `paths` 条件 skill 的激活（ext.md §2.6）。

use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{AssistantMessage, ToolResultContent, UserMessage, UserOrigin};
use serde_json::json;

use super::*;
use crate::skills::SkillSource;
use crate::skills::load::parse;

fn call(id: &str, tool: &str, path: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(format!("a_{id}")),
        content: vec![ContentBlock::ToolUse {
            id: ToolUseId::new(id),
            name: tool.to_owned(),
            input: json!({"file_path": path}),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn result(id: &str, is_error: bool) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(format!("u_{id}")),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(id),
            content: vec![ToolResultContent::Text {
                text: "ok".to_owned(),
            }],
            is_error,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new(format!("a_{id}")),
        },
    })
}

#[test]
fn successful_file_tools_inside_cwd_count() {
    let history = vec![
        call("1", "Read", "/w/src/app.ts"),
        result("1", false),
        call("2", "Edit", "/w/lib/x.py"),
        result("2", true),
        call("3", "Bash", "/w/run.sh"),
        result("3", false),
        call("4", "Write", "/elsewhere/a.ts"),
        result("4", false),
        call("5", "Write", "/w/docs/readme.md"),
    ];
    assert_eq!(touched(&history, "/w"), ["src/app.ts"]);
}

#[test]
fn conditional_skills_activate_on_matching_paths() {
    let skill = parse(
        "---\npaths: src/**/*.ts\n---\nB\n",
        "ts".to_owned(),
        "/s",
        SkillSource::Project,
    );
    assert!(!active(&skill, &[]));
    assert!(!active(&skill, &["lib/a.ts".to_owned()]));
    assert!(active(&skill, &["src/deep/a.ts".to_owned()]));
    let plain = parse("B\n", "p".to_owned(), "/s", SkillSource::Project);
    assert!(active(&plain, &[]));
}
