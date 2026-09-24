use super::*;

#[test]
fn budgets_follow_node_declarations() {
    assert_eq!(for_tool("Grep"), budget(20_000, Strategy::Artifact));
    assert_eq!(for_tool("Read").max_bytes, 262_144);
    assert_eq!(for_tool("SendMessage").max_bytes, 4_096);
    assert_eq!(
        for_tool("mcp__srv__tool"),
        budget(50_000, Strategy::Truncate)
    );
    assert_eq!(for_tool("Unknown"), budget(100_000, Strategy::Truncate));
    let bash = for_tool("Bash");
    assert!(bash.owned && bash.tail && bash.max_bytes == 30_000);
    assert_eq!(
        plan(&"x".repeat(40_000), bash),
        Plan::Inline,
        "Bash bounds itself"
    );
}

#[test]
fn fitting_keeps_whole_code_points() {
    assert_eq!(fit("héllo", 2, false), "h");
    assert_eq!(fit("héllo", 3, false), "hé");
    assert_eq!(fit("héllo", 4, true), "llo");
    assert_eq!(fit("短", 2, false), "");
    assert_eq!(fit_with_suffix("abcdef", 4, "XY", false), "abXY");
    assert_eq!(fit_with_suffix("abcdef", 4, "XY", true), "efXY");
    assert_eq!(
        fit_with_suffix("abc", 2, "XYZ", false),
        "XY",
        "the suffix fills the budget"
    );
    assert_eq!(fit_with_suffix("abc", 0, "XY", false), "");
}

#[test]
fn oversized_results_truncate_or_persist() {
    let small = budget(20, Strategy::Truncate);
    assert_eq!(plan("fits", small), Plan::Inline);
    let long = "a".repeat(200);
    let Plan::Truncate(text) = plan(&long, budget(150, Strategy::Truncate)) else {
        panic!("truncate expected");
    };
    assert_eq!(text.len(), 150);
    assert!(text.ends_with(
        "[Tool output truncated by resultBudget: originalBytes=200, maxModelBytes=150, strategy=truncate]"
    ));
    assert_eq!(plan(&long, budget(10, Strategy::Artifact)), Plan::Persist);
    let envelope = persisted(&long, "/tmp/r.json");
    assert!(envelope.starts_with(
        "<persisted-output>\nOutput too large (200 B). Full output saved to: /tmp/r.json"
    ));
}

#[test]
fn hooks_join_within_the_budget() {
    let limit = budget(40, Strategy::Truncate);
    let (mut content, mut model) = ("result".to_owned(), None);
    append_hook(&mut content, &mut model, "hook", limit);
    assert_eq!(content, "result\n\nhook");

    let mut content = "r".repeat(40);
    let mut model = Some(
        json!([{"type":"_zcode_attachment","asset":"a"},{"type":"text","text":"r".repeat(40)}]),
    );
    append_hook(&mut content, &mut model, "hook", limit);
    assert_eq!(content, format!("{}\n\nhook", "r".repeat(34)));
    let blocks = model.unwrap();
    assert_eq!(blocks[0]["type"], "_zcode_attachment");
    assert_eq!(blocks[1]["text"], format!("{}\n\nhook", "r".repeat(34)));

    let mut envelope = persisted(&"x".repeat(100), "/p");
    let before = envelope.clone();
    let mut model = None;
    append_hook(
        &mut envelope,
        &mut model,
        &"h".repeat(100),
        budget(20, Strategy::Artifact),
    );
    assert_eq!(
        envelope,
        format!("{before}\n\n{}", "h".repeat(18)),
        "the envelope stays whole"
    );
}
