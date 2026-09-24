use super::*;

fn history(roles: &[(&str, usize)]) -> Vec<Value> {
    roles
        .iter()
        .map(|(role, size)| json!({"role": role, "content": "x".repeat(*size)}))
        .collect()
}

#[test]
fn the_token_gap_comes_from_the_provider_message() {
    assert_eq!(
        token_gap("prompt is too long: 210,000 tokens > 200000 maximum"),
        Some(10_000)
    );
    assert_eq!(token_gap("1 token > 5"), None);
    assert_eq!(token_gap("context_length_exceeded"), None);
}

#[test]
fn reselection_moves_recent_groups_into_the_preserved_part() {
    // 组：[u] [a u] [a u] [a u]，每组约 1000 token。
    let messages = history(&[
        ("user", 3000),
        ("assistant", 1500),
        ("user", 1500),
        ("assistant", 1500),
        ("user", 1500),
        ("assistant", 1500),
        ("user", 1500),
    ]);
    let first = plan(&messages, false, false, 0).unwrap();
    assert_eq!((first.split, first.preserved), (5, 1));
    let next = reselect(&messages, None, 1, None).unwrap();
    assert_eq!(
        (next.split, next.preserved),
        (3, 2),
        "one more group without a gap"
    );
    assert_eq!(
        reselect(&messages, None, 2, Some(10)),
        None,
        "only one summarized group left"
    );
    let covered = reselect(&messages, None, 1, Some(1_500)).unwrap();
    assert_eq!(covered.preserved, 2);
    let first_try = initial(&messages, None, Some(1_500)).unwrap();
    assert_eq!(
        first_try.preserved, 2,
        "the last group covers 1000 of the gap"
    );
    assert_eq!(
        initial(&messages, None, Some(900)),
        None,
        "the last group already covers it"
    );
}

#[test]
fn truncation_drops_the_oldest_groups_and_marks_a_mid_round_start() {
    let list = history(&[
        ("user", 3000),
        ("assistant", 3000),
        ("user", 30),
        ("assistant", 3000),
        ("user", 30),
    ]);
    let cut = truncate(&list, Some(900)).unwrap();
    assert_eq!(cut[0], json!({"role": "user", "content": MARKER}));
    assert_eq!(cut.len(), 5, "the first group went and the marker came");
    let again = truncate(&cut, None).unwrap();
    assert_eq!(again[0]["content"], MARKER);
    assert_eq!(again.len(), 3);
    assert_eq!(
        truncate(&history(&[("user", 5), ("assistant", 5)]), None)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(truncate(&history(&[("user", 5)]), None), None);
}

#[test]
fn read_reminders_follow_node() {
    let view = |path: &str, content: &str, offset: Option<u64>| ReadView {
        path: path.into(),
        content: content.into(),
        offset,
        limit: offset.map(|_| 2),
    };
    let big = "y".repeat(15_003);
    let views = [
        view("/w/a.rs", "one\r\ntwo", Some(5)),
        view("/w/.git/config", "x", None),
        view("/w/kept.rs", "x", None),
        view("/w/big.rs", &big, None),
        view("/w/b.rs", "b", None),
    ];
    let reminders = read_reminders(&views, &["/w/kept.rs".into()]);
    assert_eq!(reminders.len(), 3);
    assert_eq!(
        reminders[0],
        "Called the Read tool with the following input: {\"file_path\":\"/w/a.rs\",\"offset\":5,\"limit\":2}\nResult of calling the Read tool:\n5\tone\n6\ttwo"
    );
    assert!(
        reminders[1]
            .starts_with("Note: /w/big.rs was read before the last conversation was summarized")
    );
    assert!(reminders[2].ends_with("Result of calling the Read tool:\n1\tb"));
    let calls = vec![json!({"role": "assistant", "tool_calls": [
        {"function": {"name": "Read", "arguments": "{\"file_path\":\"C:\\\\w\\\\x.rs\"}"}},
        {"function": {"name": "Edit", "arguments": "{\"file_path\":\"/w/y.rs\"}"}}]})];
    assert_eq!(read_paths(&calls), vec!["C:/w/x.rs".to_owned()]);
}
