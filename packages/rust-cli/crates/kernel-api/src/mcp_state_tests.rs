use super::*;

fn server(name: &str, state: McpServerState) -> McpServerView {
    McpServerView {
        name: name.to_owned(),
        state,
        reconnecting: false,
    }
}

#[test]
fn pending_names_are_sorted_by_code_unit() {
    let view = McpView {
        servers: vec![
            server("zeta", McpServerState::Pending),
            server("Alpha", McpServerState::Pending),
            server("beta", McpServerState::Connected),
            server("alpha", McpServerState::Pending),
        ],
        instructions: Vec::new(),
    };
    assert_eq!(view.pending(), ["Alpha", "alpha", "zeta"]);
}

#[test]
fn names_are_normalized_for_tool_prefixes() {
    assert_eq!(normalize_name("my server.v2"), "my_server_v2");
    assert_eq!(normalize_name("ok-name_1"), "ok-name_1");
}

#[test]
fn a_tool_belongs_to_the_servers_with_the_longest_prefix() {
    let view = McpView {
        servers: vec![
            server("a", McpServerState::Pending),
            server("a__b", McpServerState::Pending),
            server("a b", McpServerState::Connected),
            server("other", McpServerState::Pending),
        ],
        instructions: Vec::new(),
    };
    let names = |tool: &str| -> Vec<String> {
        servers_for_tool(&view, tool)
            .into_iter()
            .map(|server| server.name.clone())
            .collect()
    };
    assert_eq!(names("mcp__a__b__lookup"), ["a__b"]);
    assert_eq!(names("mcp__a__lookup"), ["a"]);
    // `a b` 规范化后是 a_b。
    assert_eq!(names("mcp__a_b__lookup"), ["a b"]);
    assert!(names("mcp__missing__x").is_empty());
    assert!(names("Read").is_empty());
}
