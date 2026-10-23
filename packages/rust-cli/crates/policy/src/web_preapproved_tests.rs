use super::*;

#[test]
fn hosts_and_path_prefixes_follow_the_list() {
    assert!(preapproved("https://docs.python.org/3/library/os.html"));
    assert!(preapproved("https://go.dev/doc/effective_go"));
    assert!(preapproved("https://go.dev/doc"));
    assert!(!preapproved("https://go.dev/blog/x"));
    assert!(!preapproved("https://go.dev/documentation"));
    assert!(!preapproved("https://example.com/docs"));
    // 编码过的分隔符不放行。
    assert!(!preapproved("https://vercel.com/docs%2f..%2fadmin"));
    // 开放规范的站点在列表里（tools-more.md §4.1）。
    assert!(preapproved("https://modelcontextprotocol.io/specification"));
    assert!(preapproved("https://agentskills.io/"));
}
