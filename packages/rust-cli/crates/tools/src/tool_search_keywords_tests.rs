use super::*;

fn candidate(name: &str, description: &str, hint: Option<&str>) -> Candidate {
    Candidate {
        name: name.to_owned(),
        description: description.to_owned(),
        hint: hint.map(str::to_owned),
        mcp: name.starts_with("mcp__"),
    }
}

fn pool() -> Vec<Candidate> {
    vec![
        candidate(
            "NotebookEdit",
            "Completely replaces the contents of a specific cell in a Jupyter notebook.",
            None,
        ),
        candidate(
            "WebFetch",
            "Fetches a URL, converts the page to markdown.",
            Some("fetch and extract content from a URL"),
        ),
        candidate(
            "WebSearch",
            "Search the web.",
            Some("search the web for current information"),
        ),
        candidate("mcp__slack__send_message", "Send a Slack message.", None),
        candidate("mcp__slack__list_channels", "List channels.", None),
    ]
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn keywords_score_names_hints_and_descriptions() {
    assert_eq!(
        search("notebook jupyter", &pool(), &[], 5),
        names(&["NotebookEdit"])
    );
    // 名字词全等（10）+ 粗分词包含（3）+ 提示词（4）> 只有描述命中的工具。
    assert_eq!(
        search("web search", &pool(), &[], 5),
        names(&["WebSearch", "WebFetch"])
    );
    assert_eq!(search("fetch", &pool(), &[], 1), names(&["WebFetch"]));
}

#[test]
fn required_terms_filter_and_mcp_names_split_by_server() {
    assert_eq!(
        search("+slack send", &pool(), &[], 5),
        names(&["mcp__slack__send_message", "mcp__slack__list_channels"])
    );
    assert_eq!(
        search("mcp__slack__list", &pool(), &[], 5),
        names(&["mcp__slack__list_channels"])
    );
    assert_eq!(
        mcp_tool_name("mcp__slack__send_message"),
        Some("send_message")
    );
    assert_eq!(mcp_tool_name("Read"), None);
}

#[test]
fn an_exact_name_returns_that_tool_even_when_loaded() {
    assert_eq!(search("webfetch", &pool(), &[], 5), names(&["WebFetch"]));
    assert_eq!(
        search("Read", &pool(), &names(&["Read"]), 5),
        names(&["Read"])
    );
    assert!(search("kubernetes", &pool(), &[], 5).is_empty());
}
