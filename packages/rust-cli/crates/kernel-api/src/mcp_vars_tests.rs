//! `${VAR}` 展开（ext.md §3.1、remote-exec.md §22.7）：字段、默认值、缺的变量。

use std::collections::BTreeMap;

use super::{expand_transport_vars, expand_vars};
use crate::mcp::McpTransportConfig;

fn lookup(name: &str) -> Option<String> {
    match name {
        "HOME" => Some("/home/dev".to_owned()),
        "TOKEN" => Some("t-123".to_owned()),
        _ => None,
    }
}

#[test]
fn values_defaults_and_missing_names() {
    let mut missing = Vec::new();
    let text = expand_vars(
        "${HOME}/bin ${NOPE:-fallback} ${GONE} ${broken",
        &lookup,
        &mut missing,
    );
    assert_eq!(text, "/home/dev/bin fallback ${GONE} ${broken");
    assert_eq!(missing, vec!["GONE".to_owned()]);
}

#[test]
fn every_string_field_of_a_stdio_server_is_expanded() {
    let config = McpTransportConfig::Stdio {
        command: "${HOME}/bin/server".to_owned(),
        args: vec!["--token".to_owned(), "${TOKEN}".to_owned()],
        env: BTreeMap::from([("WHERE".to_owned(), "${HOME}".to_owned())]),
        cwd: Some("${HOME}/proj".to_owned()),
    };
    let (expanded, missing) = expand_transport_vars(&config, &lookup);
    assert_eq!(
        expanded,
        McpTransportConfig::Stdio {
            command: "/home/dev/bin/server".to_owned(),
            args: vec!["--token".to_owned(), "t-123".to_owned()],
            env: BTreeMap::from([("WHERE".to_owned(), "/home/dev".to_owned())]),
            cwd: Some("/home/dev/proj".to_owned()),
        }
    );
    assert!(missing.is_empty());
}

#[test]
fn urls_and_header_values_are_expanded_and_missing_names_listed_once() {
    let config = McpTransportConfig::Http {
        url: "https://${HOST:-mcp.example.com}/mcp".to_owned(),
        headers: BTreeMap::from([
            ("Authorization".to_owned(), "Bearer ${TOKEN}".to_owned()),
            ("X-One".to_owned(), "${MISSING}".to_owned()),
            ("X-Two".to_owned(), "${MISSING}".to_owned()),
        ]),
    };
    let (expanded, missing) = expand_transport_vars(&config, &lookup);
    let McpTransportConfig::Http { url, headers } = expanded else {
        panic!("still http");
    };
    assert_eq!(url, "https://mcp.example.com/mcp");
    assert_eq!(headers["Authorization"], "Bearer t-123");
    assert_eq!(headers["X-One"], "${MISSING}");
    assert_eq!(missing, vec!["MISSING".to_owned()]);
}
