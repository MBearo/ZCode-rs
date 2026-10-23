use rust_cli_kernel_api::ids::SessionId;

use super::*;

/// 词组是会话 ID 的纯函数：同一会话总是同一个，换了会话就换词组（tools-more.md §11.6）。
/// 期望值用独立实现（FNV-1a 64 位 + 依次取余整除）算出。
#[test]
fn a_session_always_gets_the_same_slug() {
    let slug = |id: &str| session_slug(&SessionId::new(id));
    assert_eq!(slug("sess_1"), "lazy-foraging-planet");
    assert_eq!(slug("sess_1"), slug("sess_1"));
    assert_eq!(slug("sess_2"), "piped-frolicking-scroll");
    assert_eq!(slug("sess_01JABCDEF"), "parallel-dancing-sunbeam");
}

#[test]
fn slugs_are_three_words_from_the_lists() {
    let words = rust_cli_prompts::slug_words();
    let slug = session_slug(&SessionId::new("sess_subagent_agent_a1"));
    let parts: Vec<&str> = slug.split('-').collect();
    assert_eq!(parts.len(), 3, "{slug}");
    assert!(words.adjectives.iter().any(|w| w == parts[0]));
    assert!(words.verbs.iter().any(|w| w == parts[1]));
    assert!(words.nouns.iter().any(|w| w == parts[2]));
}
