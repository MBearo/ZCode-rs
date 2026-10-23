use super::*;

#[test]
fn the_names_and_values_keep_their_spelling() {
    // F55：值与收拢前各处写的完全相同。
    assert_eq!(
        [ACCEPT, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER],
        ["accept", "authorization", "content-type", "retry-after"]
    );
    assert_eq!(
        [JSON, EVENT_STREAM],
        ["application/json", "text/event-stream"]
    );
    assert_eq!(bearer("t-1").expose(), "Bearer t-1");
    assert!(!format!("{:?}", bearer("t-1")).contains("t-1"));
}

/// 模型请求的 User-Agent（F58）。
#[test]
fn the_model_user_agent_follows_node() {
    assert_eq!(USER_AGENT, "user-agent");
    assert_eq!(zcode_user_agent("0.1.52"), "ZCode/0.1.52");
}
