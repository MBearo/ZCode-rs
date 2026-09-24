use super::*;
use crate::loaded::Source;
use std::path::PathBuf;

fn loaded(marketplace: &str) -> Loaded {
    Loaded {
        id: format!("p@{marketplace}"),
        manifest: json!({"name":"p","userConfig":{
            "token":{"sensitive":true},"region":{"default":"eu"},"port":{"default":8.0}}}),
        manifest_path: PathBuf::from("/p/.zcode-plugin/plugin.json"),
        marketplace: marketplace.into(),
        root: PathBuf::from("/p"),
        source: Source::Cache,
    }
}

fn resolve_all(definitions: Value, options: Value) -> (Vec<(String, Value)>, Vec<Diagnostic>) {
    let loaded = loaded("m");
    let env = |name: &str| (name == "ZCODE_HOME" || name == "API_KEY").then(|| format!("<{name}>"));
    let options = options.as_object().cloned().unwrap_or_default();
    let context = Context {
        loaded: &loaded,
        data_path: Path::new("/data/p"),
        cwd: Path::new("/w"),
        env: &env,
        options: &options,
    };
    let definitions = definitions
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut diagnostics = vec![];
    let servers = resolve(&definitions, &context, &mut diagnostics);
    (servers, diagnostics)
}

#[test]
fn templates_follow_node_sinks_and_identity_rules() {
    let (servers, diagnostics) = resolve_all(
        json!({"s":{"command":"${CLAUDE_PLUGIN_ROOT}/bin","args":["${user_config.region}","${UNKNOWN}","${}"],
            "env":{"TOKEN":"${user_config.token}","KEY":"${API_KEY}","ZCODE_PLUGIN_ID":"forged","N":1}},
            "h":{"url":"https://x/${user_config.port}","headers":{"A":"${API_KEY}"},"timeoutMs":5}}),
        json!({"token":"secret"}),
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let find = |name: &str| &servers.iter().find(|(n, _)| n == name).unwrap().1;
    let stdio = find("plugin:p:s");
    assert_eq!(stdio["command"], "/p/bin");
    assert_eq!(stdio["args"], json!(["eu", "${UNKNOWN}", "${}"]));
    assert_eq!(stdio["env"]["TOKEN"], "secret");
    assert_eq!(stdio["env"]["KEY"], "<API_KEY>");
    assert_eq!(
        stdio["env"]["ZCODE_PLUGIN_ID"], "p@m",
        "identity is authoritative"
    );
    assert_eq!(stdio["env"]["CLAUDE_PLUGIN_DATA"], "/data/p");
    assert!(stdio["env"].get("N").is_none(), "non-string env is dropped");
    assert_eq!(stdio["source"]["kind"], "plugin");
    let http = find("plugin:p:h");
    assert_eq!(http["url"], "https://x/8", "JS number formatting");
    assert_eq!(http["headers"]["A"], "<API_KEY>");
    assert_eq!(http["timeoutMs"], 5);
}

#[test]
fn failures_disable_single_servers() {
    let (servers, diagnostics) = resolve_all(
        json!({"a":{"command":"x","args":["${user_config.token}"]},
            "b":{"url":"https://${API_KEY}"},
            "c":{"type":"ws","url":"x"},
            "d":{"command":"x","env":{"S":"${ZCODE_SESSION_ID}"}},
            "e":{"type":"http"},
            "f":{"url":"https://x","oauth":{"type":"client_credentials"}},
            "ok":{"command":"node","env":{"H":"${ZCODE_HOME}"}}}),
        json!({"token":"secret"}),
    );
    assert_eq!(servers.len(), 2);
    assert_eq!(
        servers[0].1["url"], "https://${API_KEY}",
        "other variables stay literal outside sensitive sinks"
    );
    assert_eq!(servers[1].1["env"]["H"], "<ZCODE_HOME>");
    let summary: Vec<_> = diagnostics
        .iter()
        .map(|d| (d.code, d.message.as_str()))
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "plugin_variable_missing",
                "Sensitive plugin user_config value cannot be used in this field: token"
            ),
            (
                "plugin_mcp_server_disabled",
                "Unsupported MCP transport: ws"
            ),
            (
                "plugin_variable_missing",
                "Plugin variable requires a runtime session context: ZCODE_SESSION_ID"
            ),
            ("plugin_mcp_server_disabled", "http MCP server requires url"),
            (
                "plugin_mcp_server_disabled",
                "MCP server f: auth is not supported by this runtime yet"
            ),
        ]
    );
}
