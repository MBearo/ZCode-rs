use super::*;

#[test]
fn prompt_is_the_control_arm_with_optional_instructions() {
    let plain = prompt(None);
    assert!(plain.starts_with("CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.\n\n"));
    assert!(plain.contains(
        "- Your entire response must be plain text: an <analysis> block followed by a <summary> block.\n\nYour task is to create a detailed summary of the conversation so far"
    ));
    assert!(plain.ends_with(
        "</example>\n\n\nREMINDER: Do NOT call any tools. Respond with plain text only — an <analysis> block followed by a <summary> block. Tool calls will be rejected and you will fail the task."
    ));
    assert!(!plain.contains("Additional Instructions"));
    let custom = prompt(Some("focus on tests"));
    assert!(
        custom.contains("</example>\n\n\nAdditional Instructions:\nfocus on tests\n\nREMINDER:")
    );
    assert_eq!(prompt(Some("  ")), plain);
}

#[test]
fn format_drops_analysis_and_unwraps_summary() {
    let raw = "<analysis>\nthinking\n</analysis>\n\n<summary>\n1. Primary Request:\n   do it\n\n\n\n2. Next\n</summary>\n";
    assert_eq!(
        format(raw),
        "Summary:\n1. Primary Request:\n   do it\n\n2. Next"
    );
    assert_eq!(format("plain text"), "plain text");
}

#[test]
fn message_text_has_the_header_summary_and_notes() {
    let text = message_text("<summary>S</summary>", SummaryShape::default());
    assert_eq!(
        text,
        "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\nSummary:\nS\nContinue the conversation from where it left off without asking the user any further questions. Resume directly — do not acknowledge the summary, do not recap what was happening, do not preface with \"I'll continue\" or similar. Pick up the last task as if the break never happened."
    );
    let kept = message_text(
        "<summary>S</summary>",
        SummaryShape {
            head_truncated: true,
            kept_tail: true,
        },
    );
    assert!(
        kept.contains("Summary:\nS\n\nNote: the earliest part of the conversation was too large")
    );
    assert!(kept.ends_with("as if the break never happened.\n\nThe messages after this summary are the most recent messages from before compaction, kept verbatim. The summary was written without seeing them, so something it says has not happened yet may already have happened in them."));
}
