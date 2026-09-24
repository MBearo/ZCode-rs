use super::*;
use serde_json::json;

#[test]
fn the_prompt_and_summary_message_are_node_text() {
    let plain = prompt(None);
    assert!(plain.starts_with("CRITICAL: Respond with TEXT ONLY. Do NOT call any tools."));
    assert!(plain.ends_with("Tool calls will be rejected and you will fail the task."));
    assert!(!plain.contains("Additional Instructions"));
    assert_eq!(prompt(Some("  ")), plain, "blank instructions are ignored");
    let custom = prompt(Some("focus on tests"));
    assert!(custom.contains("\n\nAdditional Instructions:\nfocus on tests\n\nREMINDER:"));
    let message = summary_message("<analysis>notes</analysis>\n<summary>\n1. Goal\n</summary>");
    assert!(message.starts_with(SUMMARY_HEADER));
    assert!(message.contains("covers the earlier portion of the conversation.\n\nSummary:\n1. Goal\nContinue the conversation"));
    assert!(message.ends_with("as if the break never happened."));
}

#[test]
fn summaries_format_like_node_including_js_replacement_patterns() {
    assert_eq!(format_summary("  "), "");
    assert_eq!(format_summary("plain\n\n\n\ntext"), "plain\n\ntext");
    assert_eq!(
        format_summary("<analysis>a</analysis><analysis>b</analysis>x"),
        "<analysis>b</analysis>x",
        "only the first analysis block goes"
    );
    assert_eq!(
        format_summary("<summary> cost $5 and $1 </summary>"),
        "Summary:\ncost $5 and  cost $5 and $1",
        "`$1` expands to the group as in JS"
    );
    assert_eq!(
        format_summary("<summary>$$ and $&</summary>"),
        "Summary:\n$ and <summary>$$ and $&</summary>"
    );
}

fn roles(list: &[&str]) -> Vec<serde_json::Value> {
    list.iter().map(|r| json!({"role": r})).collect()
}

#[test]
fn selection_keeps_the_last_assistant_round_for_automatic_compaction() {
    let history = roles(&[
        "user",
        "assistant",
        "tool",
        "assistant",
        "user",
        "assistant",
        "tool",
    ]);
    assert_eq!(
        select(&history, false, false),
        Some(5),
        "the last group starts at the last assistant"
    );
    assert_eq!(
        select(&history, false, true),
        Some(7),
        "manual summarizes everything"
    );
    let short = roles(&["user", "assistant"]);
    assert_eq!(
        select(&short, false, false),
        None,
        "one group to summarize is not enough"
    );
    assert_eq!(select(&short, false, true), Some(2));
    assert_eq!(select(&roles(&["user"]), false, true), None, "no assistant");
    // 已有摘要算作首条 user：它与首个 assistant 组构成两组。
    let after = roles(&["assistant", "tool", "user", "assistant"]);
    assert_eq!(select(&after, true, false), Some(3));
    assert_eq!(select(&roles(&["assistant", "tool"]), true, false), None);
    assert_eq!(select(&roles(&["assistant", "tool"]), true, true), Some(2));
}

#[test]
fn rapid_refills_block_after_three_quick_compactions() {
    let mut refill = RapidRefill::default();
    assert_eq!(
        refill.evaluate(),
        (0, false),
        "the first compaction of a turn"
    );
    refill.compacted(0);
    refill.tool_batch();
    let (count, block) = refill.evaluate();
    assert_eq!((count, block), (1, false));
    refill.compacted(count);
    let (count, _) = refill.evaluate();
    refill.compacted(count);
    assert_eq!(refill.evaluate(), (3, true));
    for _ in 0..3 {
        refill.tool_batch();
    }
    assert_eq!(
        refill.evaluate(),
        (0, false),
        "three tool turns reset the streak"
    );
    assert!(rapid_refill_error().starts_with(
        "Autocompact stopped because the context refilled within fewer than 3 tool turns"
    ));
}

#[test]
fn js_substitution_follows_get_substitution() {
    let parts = ("<", "m", ">");
    assert_eq!(
        crate::js_string::substitute("[$`|$&|$'|$$|$0|$2|$1x]", parts, &[Some("g")]),
        "[<|m|>|$|$0|$2|gx]"
    );
    assert_eq!(
        crate::js_string::substitute("$10", parts, &[Some("g")]),
        "g0"
    );
    assert_eq!(crate::js_string::substitute("end$", parts, &[]), "end$");
}
