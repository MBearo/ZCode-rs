//! Parity with the TS config implementation. Fixtures come from
//! `scripts/generate-zcode-cli-rust-fixtures.mjs`, which runs the Node functions.
use super::*;
use serde_json::{Value, json};

fn fixtures() -> Value {
    serde_json::from_str::<Value>(include_str!("../../fixtures/config.json")).unwrap()["config"]
        .clone()
}

fn scope(name: &str) -> Scope {
    match name {
        "system" => Scope::System,
        "user" => Scope::User,
        "project" => Scope::Project,
        "env" => Scope::Env,
        _ => Scope::Cli,
    }
}

#[test]
fn file_parsing_matches_node() {
    for case in fixtures()["parse"].as_array().unwrap() {
        let text = case["input"].to_string();
        let loaded = parse_file("/cfg.json", Ok(Some(&text)));
        assert_eq!(loaded.loaded, case["loaded"], "{}", case["name"]);
        assert_eq!(
            Value::Object(loaded.patch),
            case["patch"],
            "{}",
            case["name"]
        );
        let diagnostics: Vec<Value> = loaded
            .diagnostics
            .iter()
            .filter(|d| d.code != "config_file_invalid")
            .map(|d| json!({"code":d.code,"path":d.path,"severity":d.severity}))
            .collect();
        assert_eq!(
            Value::Array(diagnostics),
            case["diagnostics"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn layer_merging_and_effective_config_match_node() {
    for case in fixtures()["merge"].as_array().unwrap() {
        let patches: Vec<(Scope, Map<String, Value>)> = case["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| {
                (
                    scope(layer[0].as_str().unwrap()),
                    layer[1].as_object().unwrap().clone(),
                )
            })
            .collect();
        let layers: Vec<(Scope, &Map<String, Value>)> =
            patches.iter().map(|(s, p)| (*s, p)).collect();
        let merged = merge_layers(&layers);
        assert_eq!(
            Value::Object(merged.clone()),
            case["merged"],
            "{}",
            case["name"]
        );
        assert_eq!(effective(&merged), case["effective"], "{}", case["name"]);
    }
}

#[test]
fn env_layer_matches_node() {
    for case in fixtures()["env"].as_array().unwrap() {
        let pairs: Vec<(String, String)> = case["env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p[0].as_str().unwrap().to_owned(),
                    p[1].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let patch = env_patch(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        assert_eq!(Value::Object(patch), case["expected"]);
    }
}

#[test]
fn invalid_json_and_missing_files() {
    let missing = parse_file("/missing.json", Ok(None));
    assert!(!missing.loaded && missing.diagnostics.is_empty());
    let broken = parse_file("/broken.json", Ok(Some("{")));
    assert!(!broken.loaded);
    assert_eq!(broken.diagnostics[0].code, "config_file_invalid");
}

#[test]
fn project_files_strip_hooks_and_resolve_stdio_cwd() {
    let text = json!({
        "hooks": {"enabled": true, "events": {}},
        "mcp": {"servers": {
            "rel": {"type":"stdio","command":"x","cwd":"tools"},
            "none": {"type":"stdio","command":"x"},
            "abs": {"type":"stdio","command":"x","cwd":"/opt"},
            "web": {"type":"http","url":"https://x"}
        }}
    })
    .to_string();
    let (file, hooks) = project_file(parse_file("/repo/.zcode/config.json", Ok(Some(&text))));
    assert!(hooks.is_some());
    assert!(!file.patch.contains_key("hooks"));
    assert!(
        file.diagnostics
            .iter()
            .any(|d| d.code == "config_project_hooks_pending_trust")
    );
    let servers = &file.patch["mcp"]["servers"];
    assert_eq!(servers["rel"]["cwd"], "/repo/tools");
    assert_eq!(servers["none"]["cwd"], "/repo");
    assert_eq!(servers["abs"]["cwd"], "/opt");
    assert!(servers["web"].get("cwd").is_none());
}

#[test]
fn mcp_resolution_lets_user_shadow_project() {
    let project = json!({"mcp":{"servers":{"a":{"command":"project"},"p":{"command":"p"}}}});
    let user = json!({"mcp":{"servers":{"a":{"command":"user"}}}});
    let (servers, sources) = resolve_mcp(&[
        (Scope::User, user.as_object().unwrap()),
        (Scope::Project, project.as_object().unwrap()),
    ]);
    assert_eq!(servers["a"]["command"], "user");
    assert_eq!(sources["a"], "user");
    assert_eq!(sources["p"], "project");
}
