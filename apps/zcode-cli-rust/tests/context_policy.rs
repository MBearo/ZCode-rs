use serde_json::json;
use zcode_cli_domain::compact::select;
use zcode_cli_domain::context::{ContextPolicy, estimate};

#[test]
fn budget_matches_preflight_and_counts_reasoning_arguments_and_utf16() {
    let policy = ContextPolicy::default();
    assert_eq!(policy.threshold(), 166_000);
    assert_eq!(
        estimate(&[
            json!({"role":"assistant","content":"中文😀","reasoning_content":"想想","tool_calls":[{"function":{"name":"Edit","arguments":"{}"}}]})
        ]),
        4
    );
}

#[test]
fn summary_preserves_the_latest_assistant_round_like_node() {
    let messages = vec![
        json!({"role":"user","content":"one"}),
        json!({"role":"assistant","content":"reply"}),
        json!({"role":"user","content":"two"}),
        json!({"role":"assistant","tool_calls":[{"id":"c","function":{"name":"Read","arguments":"{}"}}]}),
        json!({"role":"tool","tool_call_id":"c","content":"file"}),
    ];
    assert_eq!(select(&messages, false, false), Some(3));
    assert_eq!(select(&messages, false, true), Some(5));
    // Node 按 assistant 分组，不检查未完成的调用（规范历史里调用总有结果）。
    assert_eq!(select(&messages[..4], false, true), Some(4));
    assert_eq!(select(&messages[..1], false, false), None);
}

#[test]
fn session_estimate_tracks_append_and_survives_context_reload() {
    use zcode_cli_domain::session::Session;
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
    session.context = zcode_cli_domain::context::ContextState {
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
