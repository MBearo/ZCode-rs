use serde_json::json;
use zcode_rust::domain::context::{ContextPolicy, estimate, microcompact, split_for_summary};

#[test]
fn budget_matches_preflight_and_counts_reasoning_arguments_and_utf16() {
    let policy = ContextPolicy::default();
    assert_eq!(policy.threshold(), 166_000);
    assert_eq!(policy.micro_threshold(), 149_400);
    assert_eq!(
        estimate(&[
            json!({"role":"assistant","content":"中文😀","reasoning_content":"想想","tool_calls":[{"function":{"name":"Edit","arguments":"{}"}}]})
        ]),
        4
    );
}

#[test]
fn summary_preserves_latest_complete_tool_round_and_rejects_open_calls() {
    let messages = vec![
        json!({"role":"user","content":"one"}),
        json!({"role":"assistant","content":"reply"}),
        json!({"role":"user","content":"two"}),
        json!({"role":"assistant","tool_calls":[{"id":"c","function":{"name":"Read","arguments":"{}"}}]}),
        json!({"role":"tool","tool_call_id":"c","content":"file"}),
    ];
    assert_eq!(split_for_summary(&messages, false), Some(3));
    assert_eq!(split_for_summary(&messages, true), Some(5));
    assert_eq!(split_for_summary(&messages[..4], true), None);
    assert_eq!(split_for_summary(&messages[..1], false), None);
}

#[test]
fn microcompact_keeps_recent_five_errors_and_canonical_input() {
    let mut messages = vec![];
    for i in 0..8 {
        messages.push(json!({"role":"assistant","tool_calls":[{"id":i.to_string(),"function":{"name":"Read"}}]}));
        messages.push(json!({"role":"tool","tool_call_id":i.to_string(),"content":if i == 0 {"Tool failed: ".to_owned()+&"x".repeat(2000)}else{"x".repeat(2000)}}));
    }
    let projected = microcompact(messages.clone(), 1);
    assert_eq!(projected[1], messages[1]);
    assert!(
        projected[3]["content"]
            .as_str()
            .unwrap()
            .contains("cleared")
    );
    assert_eq!(projected[15], messages[15]);
    assert_eq!(messages[3]["content"].as_str().unwrap().len(), 2000);
    assert_eq!(microcompact(messages.clone(), usize::MAX), messages);
}

#[test]
fn session_estimate_tracks_append_and_survives_context_reload() {
    use zcode_rust::domain::session::Session;
    let mut session = Session::new(
        "s".into(),
        "w".into(),
        "p".into(),
        "m".into(),
        "none".into(),
        "e".into(),
        0,
    );
    session.append_message(json!({"role":"user","content":"history".repeat(100)}));
    session.active_context_tokens();
    session
        .append_message(json!({"role":"assistant","content":"done","reasoning_content":"中文😀"}));
    assert_eq!(session.active_context_tokens(), estimate(&session.messages));
    session.context = zcode_rust::domain::context::ContextState {
        offset: 2,
        summary: Some("short".into()),
    };
    session.context_tokens = None;
    let summary = session.active_context_tokens();
    session.append_message(json!({"role":"user","content":"next"}));
    assert_eq!(session.active_context_tokens(), summary + 2);
    session.context_tokens = None; // 冷恢复只重建派生估算，持久化边界仍为事实。
    assert_eq!(session.active_context_tokens(), summary + 2);
}
