use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::ModelSelection;
use serde_json::Value;

use super::*;

fn selection(model: &str) -> ModelSelection {
    ModelSelection {
        provider: "p".to_owned(),
        model: model.to_owned(),
        reasoning: None,
        connection: None,
    }
}

fn recorded(source: &str, (text, data): (String, Value)) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("a"),
        source: source.to_owned(),
        text,
        data,
    })
}

fn human() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("u"),
        content: vec![ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn answer(input: u64, output: u64) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new("m"),
        content: Vec::new(),
        model: "m".to_owned(),
        stop_reason: None,
        usage: Some(Usage {
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }),
    })
}

#[test]
fn the_model_identity_is_announced_when_it_changes() {
    let (text, data) = model(&[], &selection("claude-opus-5-5")).unwrap();
    assert_eq!(
        text,
        "You are powered by the model named Opus 5.5. The exact model ID is claude-opus-5-5. Assistant knowledge cutoff is June 2026."
    );
    let history = [recorded(MODEL, (text, data))];
    assert!(model(&history, &selection("claude-opus-5-5")).is_none());
    let (text, _) = model(&history, &selection("glm-5")).unwrap();
    assert_eq!(text, "You are powered by the model glm-5.");
}

#[test]
fn the_date_is_told_once_and_again_when_it_changes() {
    let first = date(&[], "2026-10-17").unwrap();
    assert_eq!(first.0, "Today's date is 2026-10-17.");
    let history = [recorded(DATE, first)];
    assert!(date(&history, "2026-10-17").is_none());
    let (text, data) = date(&history, "2026-10-18").unwrap();
    assert!(text.starts_with("The date has changed. Today's date is now 2026-10-18."));
    assert_eq!(data["changed"], true);
}

#[test]
fn tokens_left_reset_at_each_prompt_and_only_go_down() {
    assert_eq!(tokens_left(&[]), TOTAL_TOKENS_BUDGET);
    assert_eq!(
        tokens_left(&[answer(500, 10), human()]),
        TOTAL_TOKENS_BUDGET
    );
    let history = [human(), answer(1_000, 100), answer(900, 50)];
    // 锚点前没有响应（0）；两步的上下文分别是 1100 与 950，取最大的增长。
    assert_eq!(tokens_left(&history), TOTAL_TOKENS_BUDGET - 1_100);
}

#[test]
fn attribution_is_announced_once_and_when_it_is_turned_off() {
    let signed = Attribution::default();
    let (text, data) = attribution(&[], &signed, true).unwrap();
    assert_eq!(
        text,
        "Attribution for git commits and pull requests you create from here on (this replaces ZCode's own earlier attribution guidance, such as a previous copy of this reminder; the user's own instructions about these lines, such as a AGENTS.md or memory rule, take precedence over this reminder, but do not add attribution lines this reminder leaves out):\n- End git commit messages with:\nCo-Authored-By: ZCode <noreply@z.ai>\n- End pull request descriptions with:\n🤖 Generated with [ZCode](https://zcode.z.ai)"
    );
    let history = [recorded(ATTRIBUTION, (text, data))];
    assert!(attribution(&history, &signed, true).is_none());
    assert!(attribution(&[], &signed, false).is_none());
    let off = Attribution {
        commit: None,
        pr: None,
    };
    assert!(attribution(&[], &off, true).is_none());
    let (text, _) = attribution(&history, &off, false).unwrap();
    assert!(text.starts_with("From here on, do not add attribution lines"));
}

/// 剩余 token 只在人类输入之后、或工具结果之后的一步宣布（普通的用户输入，或回合中途
/// 没有新输入；投递开启的回合、恢复重试与 Stop hook 续写都不经过这条路径）。
#[test]
fn tokens_are_due_after_human_input_and_tool_results_only() {
    use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
    use rust_cli_kernel_api::message::{
        AttachmentMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
    };
    let user = |origin: UserOrigin| {
        Message::User(UserMessage {
            id: MessageId::new("u"),
            content: vec![ContentBlock::Text {
                text: "x".to_owned(),
            }],
            origin,
        })
    };
    let attachment = Message::Attachment(AttachmentMessage {
        id: MessageId::new("a"),
        source: "queued_command".to_owned(),
        text: "<system-reminder>\nq\n</system-reminder>".to_owned(),
        data: serde_json::Value::Null,
    });
    let result = Message::User(UserMessage {
        id: MessageId::new("r"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("t"),
            content: vec![ToolResultContent::Text {
                text: "ok".to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("m"),
        },
    });
    assert!(tokens_due(&[user(UserOrigin::Human)]));
    assert!(tokens_due(&[result.clone(), attachment.clone()]));
    // Skill 的正文排在 tool_result 之后，算在这次工具结果里。
    let skill = user(UserOrigin::Meta {
        source: "skill".to_owned(),
    });
    assert!(tokens_due(&[result.clone(), skill.clone()]));
    // 用户调用 skill（命令标签 + 正文）不是普通的用户输入：不宣布（ext.md §2.7）。
    let command = Message::User(UserMessage {
        id: MessageId::new("c"),
        content: vec![ContentBlock::Text {
            text: rust_cli_kernel_api::user_command::command_tags("greet", "Ada"),
        }],
        origin: UserOrigin::Human,
    });
    assert!(!tokens_due(&[command.clone(), skill.clone()]));
    assert!(!tokens_due(&[command, skill, attachment.clone()]));
    let notice = user(UserOrigin::Meta {
        source: "task-notification".to_owned(),
    });
    assert!(!tokens_due(&[result, notice]));
    let nudge = user(UserOrigin::Meta {
        source: "max_output_tokens_recovery".to_owned(),
    });
    assert!(!tokens_due(&[nudge, attachment]));
    assert!(!tokens_due(&[]));
}

/// `/goal` 的说明与 Stop hook 的反馈之后不算 `getAttachments` 那一组（goal.md §8）。
#[test]
fn inputs_are_not_announced_after_the_goal_prompt_or_stop_feedback() {
    let meta = |source: &str| {
        Message::User(UserMessage {
            id: MessageId::new("m"),
            content: vec![ContentBlock::Text {
                text: "x".to_owned(),
            }],
            origin: UserOrigin::Meta {
                source: source.to_owned(),
            },
        })
    };
    let status = recorded("goal_status", (String::new(), Value::Null));
    assert!(inputs_due(&[human()]));
    assert!(inputs_due(&[]));
    assert!(!inputs_due(&[human(), meta("goal"), status]));
    assert!(!inputs_due(&[human(), meta("stop_hook_feedback")]));
    assert!(inputs_due(&[meta("goal"), human()]));
    assert!(inputs_due(&[human(), meta("skill")]));
}

/// 修 bug（修复线 F9）：bash 模式开启的回合不算当步 attachment 那一组（bash 模式的回合不组装
/// 当步 attachment，context.md §4）；下一条普通输入照常宣布。
#[test]
fn inputs_are_not_announced_after_a_bash_mode_command() {
    let said = |text: &str| {
        Message::User(UserMessage {
            id: MessageId::new("b"),
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            origin: UserOrigin::Human,
        })
    };
    let input = said("<bash-input>cat note.txt</bash-input>");
    let output = said("<bash-stdout>NOTE</bash-stdout><bash-stderr></bash-stderr>");
    let failed = said("<bash-stderr>Command failed: boom</bash-stderr>");
    assert!(!inputs_due(&[input.clone(), output.clone()]));
    assert!(!inputs_due(&[input.clone(), failed]));
    let date = recorded("date", (String::new(), Value::Null));
    assert!(!inputs_due(&[input.clone(), output.clone(), date]));
    // 之后的普通输入、工具结果照常。
    assert!(inputs_due(&[input, output, human()]));
}
