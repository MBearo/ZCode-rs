//! 子代理文本：文件都在、模板变量齐全、通知的包装同形。

use super::{ALL, notification_text};
use crate::texts::render;

#[test]
fn every_agent_text_is_present() {
    for (file, text) in ALL {
        assert!(!text().is_empty(), "prompts/agents/{file}.txt is empty");
        assert!(
            !text().ends_with('\n') || *file == "notification_preamble",
            "{file}"
        );
    }
}

#[test]
fn the_notification_has_the_expected_frame() {
    let text = notification_text(&[
        ("id", "agent_1"),
        ("tool_use", "toolu_1"),
        ("status", "completed"),
        ("summary", "Agent \"Find files\" finished"),
        (
            "body",
            "\n<usage><subagent_tokens>13</subagent_tokens><tool_uses>0</tool_uses><duration_ms>5</duration_ms></usage>",
        ),
    ]);
    assert!(text.starts_with("<system-reminder>\n[SYSTEM NOTIFICATION - NOT USER INPUT]\n"));
    assert!(text.contains("consent.\n\n<task-notification>\n<task-id>agent_1</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n<status>completed</status>\n"));
    assert!(text.ends_with(
        "<duration_ms>5</duration_ms></usage>\n</task-notification>\n</system-reminder>"
    ));
    assert!(!text.contains("{{"), "{text}");
}

#[test]
fn the_trailer_takes_the_agent_id_twice() {
    let text = render(
        super::handback_trailer(),
        &[
            ("id", "agent_9"),
            ("tokens", "1"),
            ("tool_uses", "2"),
            ("duration", "3"),
        ],
    );
    assert_eq!(
        text,
        "agentId: agent_9 (use SendMessage with to: 'agent_9', summary: '<5-10 word recap>' to continue this agent)\n<usage>subagent_tokens: 1\ntool_uses: 2\nduration_ms: 3</usage>"
    );
}

#[test]
fn several_notifications_share_one_preamble() {
    let text = super::notification_wrap(
        "<task-notification>a</task-notification>\n\n<task-notification>b</task-notification>",
    );
    assert_eq!(text.matches("[SYSTEM NOTIFICATION").count(), 1);
    assert!(text.starts_with("<system-reminder>\n[SYSTEM NOTIFICATION - NOT USER INPUT]\n"));
    assert!(text.ends_with("</task-notification>\n</system-reminder>"));
}
