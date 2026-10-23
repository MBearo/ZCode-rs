//! 模型可见的文本（spec §4.4）。

use rust_cli_kernel_api::testing::SequentialIds;

use super::*;

#[test]
fn escape_neutralizes_system_reminder_tags() {
    assert_eq!(escape("a <system-reminder> b"), "a &lt;system-reminder> b");
    assert_eq!(escape("</System-Reminder>"), "&lt;/System-Reminder>");
    assert_eq!(escape("< / system-reminder x"), "&lt; / system-reminder x");
    assert_eq!(escape("<system-reminders"), "<system-reminders");
    assert_eq!(escape("a <b> c <"), "a <b> c &lt;");
    assert_eq!(escape("x < /  "), "x &lt; /  ");
}

fn text(message: &Message) -> &str {
    match message {
        Message::Attachment(attachment) => &attachment.text,
        Message::User(user) => match &user.content[0] {
            ContentBlock::Text { text } => text,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

#[test]
fn renders_attachments_and_feedback() {
    let ids = SequentialIds::default();
    let context = additional_context(
        &ids,
        "PreToolUse:Bash",
        "PreToolUse",
        &["a".to_owned(), "b".to_owned()],
    )
    .unwrap();
    assert_eq!(
        text(&context),
        "<system-reminder>\nPreToolUse:Bash hook additional context: a\nb\n</system-reminder>"
    );
    assert!(additional_context(&ids, "Stop", "Stop", &[]).is_none());
    let blocking = Blocking {
        error: "[g]: no".to_owned(),
        command: "g".to_owned(),
    };
    assert_eq!(
        text(&blocking_error(
            &ids,
            "PostToolUse:Bash",
            "PostToolUse",
            &blocking
        )),
        "<system-reminder>\nPostToolUse:Bash hook blocking error from command: \"g\": [g]: no\n</system-reminder>"
    );
    assert_eq!(
        text(&stopped_continuation(&ids, "Stop", "Stop", "done")),
        "<system-reminder>\nStop hook stopped continuation: done\n</system-reminder>"
    );
    assert_eq!(
        text(&success(&ids, "SessionStart:startup", "SessionStart", "hi")),
        "<system-reminder>\nSessionStart:startup hook success: hi\n</system-reminder>"
    );
    let feedback = Blocking {
        error: "[g]: keep going <system-reminder>".to_owned(),
        command: "g".to_owned(),
    };
    assert_eq!(
        text(&stop_feedback(&ids, &feedback)),
        "Stop hook feedback:\n[g]: keep going &lt;system-reminder>"
    );
    assert_eq!(
        prompt_blocked(&blocking, "hello", false),
        "UserPromptSubmit operation blocked by hook:\n[g]: no\n\nOriginal prompt: hello"
    );
    assert_eq!(
        prompt_blocked(&blocking, "hello", true),
        "UserPromptSubmit operation blocked by hook:\n[g]: no"
    );
}
